<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import StorageManager from '$lib/components/StorageManager.svelte';
  import Icon from '$lib/components/Icon.svelte';

  // Intergalactic Model Share — a decentralized, censorship-resistant network of
  // AI models shared over IPFS (the InterPlanetary File System — we think
  // bigger). Every Orb auto-enrolls at boot and gossips its catalog on a pubsub
  // topic, so this console shows models from every Orb on the network with no
  // token or fleet enrollment. Upload a model (with a model card), browse what
  // others share, and download (pin) any of them to host it here too — every
  // holder helps serve it, so popular models download faster.

  type Card = {
    kind?: string; base_model?: string; params?: string; quant?: string;
    license?: string; description?: string; intended_use?: string;
    tags?: string[]; format?: string; image?: string; readme?: boolean;
  };
  type Entry = {
    cid: string; name: string; file?: string; size_bytes: number;
    sha256?: string; card?: Card; added_at_ms?: number;
    origin_id?: string; origin_label?: string;
    verified?: boolean; source?: string;
  };
  type Host = { label: string; is_self: boolean; online: boolean; peer_id?: string; addrs?: string[] };
  type Row = { entry: Entry; hosts: Host[]; local: boolean };
  type NodeStatus = {
    enabled: boolean; daemon: string; peers: number; gateway_port: number;
    repo_bytes: number; storage_max: string;
  };

  let node: NodeStatus | null = null;
  let rows: Row[] = [];
  let tasks: Record<string, string> = {};
  let peerCount = 0;
  let selfPeer = '';
  let err = '';
  let poll: ReturnType<typeof setInterval>;

  // filters
  let query = '';
  let kindFilter = 'all';
  const KINDS = ['all', 'llm', 'vlm', 'vision', 'stt', 'tts', 'embedding', 'other'];
  const KIND_ICON: Record<string, string> = {
    llm: '💬', vlm: '👁️', vision: '🖼️', stt: '🎙️', tts: '🔊', embedding: '🧭', other: '📦',
  };

  // upload / share form
  let fileInput: HTMLInputElement;
  let imageInput: HTMLInputElement;
  let pendingFile: File | null = null;      // weights (null when editing metadata only)
  let editingCid: string | null = null;      // set when editing an existing model
  let form = { name: '', kind: 'llm', base_model: '', params: '', quant: '', format: '', license: '', description: '', intended_use: '', tags: '', readme: '' };
  let imageB64 = '';   // data URL of a newly picked image (share or replace)
  let imageExt = '';
  let existingImageUrl = '';  // gateway URL of the current image when editing
  let removeImage = false;
  let uploadPct = -1;
  let uploadName = '';
  let showShare = false;
  let saving = false;

  // detail modal
  let detail: Row | null = null;
  let detailReadme = '';       // fetched README text for the open detail
  let detailReadmeLoaded = '';  // cid whose readme we've fetched

  // IPFS toggle + HuggingFace import
  let toggling = false;
  let hfUrl = '';
  let hfBusy = false;

  // ── Push a model to a connected system (Agent Dashboard: DGX / gateways) ──
  let systems: Array<{ id: string; label: string; address: string; roles?: string[]; status?: string }> = [];
  let pushSel = '';
  let pushMsg = '';
  let pushActive = false;

  // Push dialog + destination
  let pushEntry: Entry | null = null;        // the model being pushed (opens the dialog)
  let pushMode: 'home' | 'custom' = 'home';  // default home folder vs a browsed folder
  let pushDest = '';                          // chosen target directory (custom mode)

  // Remote folder browser (lists directories on the target over SSH)
  let browsePath = '';
  let browseParent = '';
  let browseDirs: string[] = [];
  let browseLoading = false;
  let browseErr = '';

  async function loadSystems() {
    try {
      const r = await fetch('/api/agent/systems', { credentials: 'same-origin' }).then((x) => x.json());
      systems = r?.systems ?? [];
      if (!pushSel && systems[0]) pushSel = systems[0].id;
    } catch { /* not admin / none registered */ }
  }

  function slugify(name: string): string {
    const s = name.trim().toLowerCase().replace(/[^a-z0-9.-]+/g, '-').replace(/^-+|-+$/g, '');
    return (s || 'model').slice(0, 80);
  }

  function openPush(entry: Entry) {
    pushEntry = entry;
    pushMode = 'home'; pushDest = '';
    browseErr = ''; browseDirs = [];
    pushMsg = ''; pushActive = false;
    loadSystems();
  }
  function closePush() { pushEntry = null; }

  function doPush() {
    if (!pushEntry || !pushSel) return;
    // home → backend defaults to ~/aeon-models/<slug>; custom → the browsed
    // folder + a per-model subdir so pushes don't collide.
    const dest = pushMode === 'custom' && pushDest ? `${pushDest.replace(/\/+$/, '')}/${slugify(pushEntry.name)}` : '';
    pushToSystem(pushEntry, pushSel, dest);
  }

  async function pushToSystem(entry: Entry, systemId: string, dest: string) {
    pushActive = true; pushMsg = 'starting…';
    try {
      const r = await fetch(`/api/agent/systems/${encodeURIComponent(systemId)}/models/push`, {
        method: 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid: entry.cid, name: entry.name, dest }),
      }).then((x) => x.json());
      if (!r?.ok) { pushMsg = 'error: ' + (r?.err ?? 'push failed'); pushActive = false; return; }
      const slug = String(r.task ?? '').split(':').slice(1).join(':');
      pollPush(systemId, slug);
    } catch (e: any) { pushMsg = 'error: ' + (e?.message ?? 'push failed'); pushActive = false; }
  }

  async function browseTo(path: string) {
    if (!pushSel) return;
    browseLoading = true; browseErr = '';
    try {
      const r = await fetch(`/api/agent/systems/${encodeURIComponent(pushSel)}/browse?path=${encodeURIComponent(path)}`, { credentials: 'same-origin' }).then((x) => x.json());
      if (!r?.ok) { browseErr = r?.err ?? 'cannot open folder'; }
      else { browsePath = r.path; browseParent = r.parent ?? ''; browseDirs = r.dirs ?? []; pushMode = 'custom'; pushDest = r.path; }
    } catch (e: any) { browseErr = e?.message ?? 'browse failed'; }
    browseLoading = false;
  }
  function startBrowse() { browseTo(''); }

  async function pollPush(sysId: string, slug: string) {
    try {
      const r = await fetch(`/api/agent/systems/${encodeURIComponent(sysId)}/models/push/status`, { credentials: 'same-origin' }).then((x) => x.json());
      const st = r?.pushes?.[slug];
      if (st) {
        pushMsg =
          st.phase === 'transferring' ? `transferring ${st.pct ?? 0}%`
          : st.phase === 'pulling' ? 'pulling to Orb…'
          : st.phase === 'queued' ? 'queued…'
          : st.phase === 'done' ? '✓ pushed to ' + (st.system ?? 'system') + ' → ' + (st.path ?? '')
          : st.phase === 'error' ? 'error: ' + (st.err ?? 'failed')
          : String(st.phase ?? '');
        if (st.done) { pushActive = false; return; }
      }
    } catch { /* transient */ }
    setTimeout(() => pollPush(sysId, slug), 1500);
  }

  async function toggleIpfs() {
    if (!node) return;
    toggling = true; err = '';
    try {
      await api(node.enabled ? '/disable' : '/enable', { method: 'POST' });
    } catch (e: any) { err = e?.message ?? 'toggle failed'; }
    // enabling installs kubo on first run — give it a moment, then reload.
    setTimeout(async () => { toggling = false; await load(); }, 1500);
  }

  async function importHf() {
    const url = hfUrl.trim();
    if (!url) return;
    hfBusy = true; err = '';
    try {
      const r = await api('/models/import-hf', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ url }),
      });
      if (r && r.ok === false) err = r.err || 'import failed';
      else hfUrl = '';
    } catch (e: any) { err = e?.message ?? 'import failed'; }
    hfBusy = false; await load();
  }

  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/ipfs${path}`, { credentials: 'same-origin', ...opts }).then((r) => r.json());

  async function load() {
    try {
      node = await api('/status');
      if (node?.daemon === 'active') {
        const r = await api('/models/registry');
        if (r?.ok) {
          rows = r.models ?? [];
          tasks = r.tasks ?? {};
          peerCount = r.peer_count ?? 0;
          selfPeer = r.self_peer_id ?? '';
          err = '';
        }
      }
    } catch (e: any) {
      err = e?.message ?? 'failed to load';
    }
  }

  function fmtBytes(n: number): string {
    if (!n) return '—';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0, x = n;
    while (x >= 1024 && i < u.length - 1) { x /= 1024; i++; }
    return `${x.toFixed(x < 10 && i > 0 ? 1 : 0)} ${u[i]}`;
  }

  // Card images are served THROUGH the supervisor (same-origin HTTPS), not the
  // plain-HTTP :8080 gateway — otherwise the browser blocks them as mixed
  // content on the HTTPS console and they silently fail to render.
  const imageSrc = (cid: string, name: string) =>
    `/api/ipfs/models/image?cid=${cid}&name=${encodeURIComponent(name)}`;

  $: gatewayBase =
    typeof location !== 'undefined' ? `http://${location.hostname}:${node?.gateway_port ?? 8080}` : '';
  function downloadUrl(e: Entry) {
    return `${gatewayBase}/ipfs/${e.cid}${e.file ? '/' + encodeURIComponent(e.file) : ''}?download=true&filename=${encodeURIComponent(e.name)}`;
  }

  $: filtered = rows.filter((r) => {
    if (kindFilter !== 'all' && (r.entry.card?.kind || 'other') !== kindFilter) return false;
    if (!query.trim()) return true;
    const q = query.toLowerCase();
    const c = r.entry.card;
    return [r.entry.name, c?.base_model, c?.description, c?.params, c?.quant, ...(c?.tags ?? [])]
      .filter(Boolean).join(' ').toLowerCase().includes(q);
  });

  function resetForm() {
    pendingFile = null; editingCid = null; imageB64 = ''; imageExt = '';
    existingImageUrl = ''; removeImage = false; saving = false;
    form = { name: '', kind: 'llm', base_model: '', params: '', quant: '', format: '', license: '', description: '', intended_use: '', tags: '', readme: '' };
  }

  // ── Share (new model) ──
  function pickFile() { resetForm(); fileInput?.click(); }
  function onFile() {
    const f = fileInput?.files?.[0];
    if (!f) return;
    pendingFile = f;
    if (!form.name) form.name = f.name.replace(/\.(gguf|safetensors|onnx|hef|bin|pt|pth)$/i, '');
    const ext = (f.name.split('.').pop() || '').toLowerCase();
    if (!form.format && ['gguf', 'safetensors', 'onnx', 'hef', 'bin', 'pt', 'pth'].includes(ext)) form.format = ext;
    showShare = true;
    if (fileInput) fileInput.value = '';
  }

  // ── Edit (existing model — metadata / image / README only) ──
  async function editModel(row: Row) {
    resetForm();
    editingCid = row.entry.cid;
    const c = row.entry.card ?? {};
    form = {
      name: row.entry.name, kind: c.kind || 'other', base_model: c.base_model || '',
      params: c.params || '', quant: c.quant || '', format: c.format || '',
      license: c.license || '', description: c.description || '', intended_use: c.intended_use || '',
      tags: (c.tags || []).join(', '), readme: '',
    };
    if (c.image) existingImageUrl = imageSrc(row.entry.cid, c.image);
    showShare = true;
    detail = null;
    // pull the existing README so the editor is prefilled
    if (c.readme) {
      try {
        const r = await api(`/models/file?cid=${row.entry.cid}&name=README.md`);
        if (r?.ok) form.readme = r.text ?? '';
      } catch {}
    }
  }

  function onImage() {
    const f = imageInput?.files?.[0];
    if (!f) return;
    if (f.size > 4 * 1024 * 1024) { err = 'image too large (max 4 MB)'; return; }
    imageExt = (f.name.split('.').pop() || 'png').toLowerCase();
    removeImage = false;
    const reader = new FileReader();
    reader.onload = () => { imageB64 = String(reader.result || ''); };
    reader.readAsDataURL(f);
    if (imageInput) imageInput.value = '';
  }
  function clearImage() { imageB64 = ''; existingImageUrl = ''; removeImage = true; }

  function cardFromForm(): Card {
    return {
      kind: form.kind, base_model: form.base_model.trim(), params: form.params.trim(),
      quant: form.quant.trim(), format: form.format.trim(), license: form.license.trim(),
      description: form.description.trim(), intended_use: form.intended_use.trim(),
      tags: form.tags.split(',').map((t) => t.trim()).filter(Boolean),
    };
  }

  // Publish a new model: stream the weights (progress), then finalize with the
  // card + README + image as JSON.
  function submitShare() {
    if (editingCid) return submitEdit();
    if (!pendingFile) return;
    const f = pendingFile;
    uploadName = form.name || f.name;
    uploadPct = 0; err = '';
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/api/ipfs/models/upload-weights');
    xhr.setRequestHeader('Content-Disposition', `attachment; filename="${f.name}"`);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.withCredentials = true;
    xhr.upload.addEventListener('progress', (e) => {
      if (e.lengthComputable) uploadPct = Math.floor((e.loaded / e.total) * 100);
    });
    xhr.addEventListener('load', async () => {
      let draft: any = {};
      try { draft = JSON.parse(xhr.responseText); } catch {}
      if (!draft.ok) { uploadPct = -1; err = draft.err || `HTTP ${xhr.status}`; return; }
      uploadPct = 100;
      const r = await api('/models/publish', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          draft_id: draft.draft_id, name: form.name.trim() || f.name, card: cardFromForm(),
          readme: form.readme, image_b64: imageB64, image_ext: imageExt,
        }),
      });
      uploadPct = -1;
      if (r && r.ok === false) err = r.err || 'publish failed';
      showShare = false; resetForm(); await load();
    });
    xhr.addEventListener('error', () => { uploadPct = -1; err = 'upload failed (network)'; });
    xhr.send(f);
  }

  // Edit an existing model's metadata/image/README — rebuilds the IPFS
  // directory on the Orb reusing the weights (no re-upload).
  async function submitEdit() {
    if (!editingCid) return;
    saving = true; err = '';
    const body: any = { cid: editingCid, name: form.name.trim(), card: cardFromForm(), readme: form.readme };
    if (imageB64) { body.image_b64 = imageB64; body.image_ext = imageExt; }
    else if (removeImage) body.remove_image = true;
    try {
      const r = await api('/models/edit', {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
      });
      if (r && r.ok === false) err = r.err || 'edit failed';
    } catch (e: any) { err = e?.message ?? 'edit failed'; }
    saving = false; showShare = false; resetForm(); await load();
  }

  // Minimal, XSS-safe markdown → HTML for READMEs from untrusted Orbs: escape
  // ALL html first, then re-introduce only a fixed, safe tag set. Links are
  // autolinked http(s) only.
  function renderMd(src: string): string {
    let s = src.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
    s = s.replace(/```([\s\S]*?)```/g, (_m, c) => `<pre class="bg-ink-950 border border-ink-800 rounded p-2 overflow-x-auto text-[11px]">${c.trim()}</pre>`);
    s = s.replace(/`([^`]+)`/g, '<code class="text-cursed-300">$1</code>');
    s = s.replace(/^###\s+(.*)$/gm, '<h3 class="font-mono text-ink-100 mt-3 mb-1">$1</h3>');
    s = s.replace(/^##\s+(.*)$/gm, '<h2 class="font-mono text-ink-100 text-base mt-3 mb-1">$1</h2>');
    s = s.replace(/^#\s+(.*)$/gm, '<h1 class="font-mono text-ink-100 text-lg mt-3 mb-1">$1</h1>');
    s = s.replace(/\*\*([^*]+)\*\*/g, '<strong class="text-ink-100">$1</strong>');
    s = s.replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>');
    s = s.replace(/^[-*]\s+(.*)$/gm, '<li class="ml-4 list-disc">$1</li>');
    s = s.replace(/\bhttps?:\/\/[^\s<)]+/g, (u) => `<a href="${u}" target="_blank" rel="noreferrer" class="text-cursed-300 hover:underline">${u}</a>`);
    return s.replace(/\n{2,}/g, '<br><br>').replace(/\n/g, '<br>');
  }

  // Lazy-load the README when a detail modal opens for a model that has one.
  $: if (detail && detail.entry.card?.readme && detailReadmeLoaded !== detail.entry.cid) {
    const cid = detail.entry.cid;
    detailReadmeLoaded = cid;
    detailReadme = '';
    api(`/models/file?cid=${cid}&name=README.md`).then((r) => { if (r?.ok && detail?.entry.cid === cid) detailReadme = r.text ?? ''; }).catch(() => {});
  }

  async function download(row: Row) {
    const src = row.hosts.find((h) => !h.is_self && h.online && h.peer_id) ?? row.hosts[0];
    err = '';
    try {
      const r = await api('/models/fetch', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...row.entry, addrs: src?.addrs ?? [], peer_id: src?.peer_id ?? '' }),
      });
      if (r && r.ok === false) err = r.err || 'download failed';
    } catch (e: any) { err = e?.message ?? 'download failed'; }
    await load();
  }

  let confirmRemove = '';
  async function unshare(cid: string) {
    if (confirmRemove !== cid) {
      confirmRemove = cid;
      setTimeout(() => { if (confirmRemove === cid) confirmRemove = ''; }, 3000);
      return;
    }
    confirmRemove = '';
    try {
      await api('/models/remove', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid }),
      });
    } catch {}
    if (detail?.entry.cid === cid) detail = null;
    await load();
  }

  onMount(() => { load(); loadSystems(); poll = setInterval(load, 5000); });
  // Reset the push status line whenever a different model detail opens.
  $: if (detail) { pushMsg = ''; pushActive = false; }
  onDestroy(() => clearInterval(poll));
</script>

<div class="min-h-screen bg-ink-950 text-ink-100">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
    <h1 class="font-mono text-lg text-cursed-300 flex items-center gap-2"><Icon name="aether" class="w-5 h-5" /> Intergalactic <span class="text-ink-400">Model Share</span></h1>
    <div class="w-28 text-right">
      {#if node?.daemon === 'active'}
        <span class="text-[10px] font-mono uppercase tracking-wider px-2 py-1 rounded bg-emerald-900/50 text-emerald-300"
              title="This Orb is enrolled in the Intergalactic Model Share network (IPFS)">on-network</span>
      {:else}
        <span class="text-[10px] font-mono uppercase tracking-wider px-2 py-1 rounded bg-amber-900/50 text-amber-300">connecting…</span>
      {/if}
    </div>
  </header>

  <main class="max-w-5xl mx-auto px-5 py-6 space-y-5">
    <p class="text-ink-400 text-xs leading-relaxed -mt-1">
      Built on <span class="text-cursed-300">IPFS — the InterPlanetary File System</span>. We just think bigger.
    </p>
    <p class="text-ink-300 text-sm leading-relaxed">
      A decentralized, censorship-resistant network for AI models — no fleet, no accounts, no central server to
      take down. Every Aeon Orb auto-joins at boot and announces the models it hosts, so the library below is
      contributed by Orbs everywhere, and each model is served by everyone who holds it — the more popular a
      model, the faster it downloads. <span class="text-cursed-300">Share</span> a model to publish it with a
      model card; <span class="text-cursed-300">download</span> anyone's to run it locally (and help host it).
    </p>

    {#if err}
      <div class="rounded border border-red-700 bg-red-950/40 text-red-300 px-3 py-2 text-sm">{err}</div>
    {/if}

    <!-- IPFS node status + on/off toggle -->
    <div class="rounded-lg border border-ink-700 bg-ink-900 p-3 flex items-center gap-3 flex-wrap">
      <span class="font-mono text-[10px] uppercase tracking-wider px-2 py-1 rounded {node?.daemon === 'active' ? 'bg-emerald-900/50 text-emerald-300' : node?.daemon === 'failed' ? 'bg-red-900/50 text-red-300' : node?.enabled ? 'bg-amber-900/50 text-amber-300' : 'bg-ink-800 text-ink-400'}">
        {node?.daemon === 'active' ? 'IPFS running' : node?.daemon === 'failed' ? 'IPFS failed' : node?.enabled ? 'IPFS starting' : 'IPFS off'}
      </span>
      {#if node?.daemon === 'active'}
        <span class="text-ink-500 text-xs font-mono">{node.peers} swarm peers · {fmtBytes(node.repo_bytes)} /
          <a href="/orbnet/ipfs" class="underline decoration-dotted hover:text-cursed-300"
             title="This is your IPFS storage allocation, not an upload cap — click to adjust how much storage this Orb shares with the network">{node.storage_max}</a></span>
      {/if}
      <div class="flex-1"></div>
      <!-- on/off switch -->
      <button
        class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors {node?.enabled ? 'bg-cursed-600' : 'bg-ink-700'} disabled:opacity-50"
        on:click={toggleIpfs} disabled={toggling} title={node?.enabled ? 'Disable IPFS (leave Model Share)' : 'Enable IPFS (join Model Share)'}>
        <span class="inline-block h-5 w-5 transform rounded-full bg-white transition-transform {node?.enabled ? 'translate-x-5' : 'translate-x-0.5'}"></span>
      </button>
      <span class="text-xs font-mono text-ink-400 w-16">{toggling ? '…' : node?.enabled ? 'enabled' : 'disabled'}</span>
    </div>

    {#if node?.enabled}
      <!-- Storage allocation, external drives + LAN sharing — the same controls
           as the IPFS page, collapsible so the model grid stays front and centre. -->
      <details class="rounded-lg border border-ink-700 bg-ink-900/60 group">
        <summary class="cursor-pointer select-none px-4 py-2.5 font-mono text-sm text-cursed-300 flex items-center gap-2 list-none">
          <span class="text-ink-500 transition-transform group-open:rotate-90">▸</span>
          Storage, drives &amp; LAN sharing
        </summary>
        <div class="p-3 pt-0"><StorageManager /></div>
      </details>

      <!-- toolbar -->
      <div class="flex items-center gap-3 flex-wrap">
        <button class="btn-primary text-sm px-4 py-2 rounded-md" on:click={pickFile} disabled={uploadPct >= 0}>+ Share a model</button>
        <input type="file" bind:this={fileInput} class="hidden" on:change={onFile} />
        <input class="flex-1 min-w-[12rem] bg-ink-800 border border-ink-700 rounded px-3 py-2 text-ink-100 text-sm"
               bind:value={query} placeholder="Search models, base model, tags…" />
        <select bind:value={kindFilter} class="bg-ink-800 border border-ink-700 rounded px-2 py-2 text-ink-100 text-sm font-mono">
          {#each KINDS as k}<option value={k}>{k}</option>{/each}
        </select>
        <span class="text-ink-500 text-xs font-mono whitespace-nowrap">{filtered.length} model{filtered.length === 1 ? '' : 's'} · {peerCount} peer{peerCount === 1 ? '' : 's'}</span>
      </div>

      <!-- import straight from HuggingFace -->
      <div class="flex items-center gap-2 flex-wrap">
        <span class="text-lg">🤗</span>
        <input class="flex-1 min-w-[14rem] bg-ink-800 border border-ink-700 rounded px-3 py-2 text-ink-100 text-sm font-mono"
               bind:value={hfUrl} placeholder="Import from HuggingFace — paste a model URL (huggingface.co/org/model)"
               on:keydown={(e) => e.key === 'Enter' && importHf()} />
        <button class="btn text-sm px-4 py-2 rounded-md" on:click={importHf} disabled={hfBusy || !hfUrl.trim()}>{hfBusy ? 'Starting…' : 'Import'}</button>
      </div>
      <p class="text-[11px] text-ink-600 -mt-2">Pulls the weights, the README + author image, and verifies each file's SHA-256 against the hash HuggingFace publishes.</p>
    {/if}

    {#if uploadPct >= 0}
      <div class="rounded-lg border border-cursed-700/50 bg-ink-900 p-4 space-y-1">
        <div class="flex justify-between text-xs font-mono text-ink-300"><span class="truncate">Uploading {uploadName}…</span><span>{uploadPct}%</span></div>
        <div class="h-2 rounded-full bg-ink-800 overflow-hidden"><div class="h-full bg-cursed-500 transition-all duration-150" style="width:{uploadPct}%"></div></div>
        <div class="text-[11px] text-ink-500">Streaming to IPFS — after the upload it's added to the blockstore and announced to the network.</div>
      </div>
    {/if}

    <!-- in-flight fetch/add phases -->
    {#each Object.entries(tasks) as [key, phase] (key)}
      <div class="flex items-center gap-2 text-xs font-mono {phase.startsWith('error') ? 'text-red-400' : 'text-amber-300'}">
        {#if !phase.startsWith('error')}<span class="inline-block h-3 w-3 rounded-full border-2 border-amber-400 border-t-transparent animate-spin"></span>{/if}
        <span class="truncate max-w-[16rem]">{key}</span><span class="text-ink-600">·</span><span>{phase}</span>
      </div>
    {/each}

    <!-- model grid -->
    {#if !node?.enabled}
      <div class="text-ink-500 text-sm text-center py-10">IPFS is off — flip the switch above to join Model Share.</div>
    {:else if node?.daemon === 'failed'}
      <div class="text-red-300 text-sm text-center py-10">IPFS daemon failed to start — SSH into the Orb and check <code class="font-mono">journalctl -u aeon-ipfs</code>.</div>
    {:else if node?.daemon !== 'active'}
      <div class="text-ink-500 text-sm text-center py-10">Connecting to the IPFS network… (first run downloads kubo, ~30 MB)</div>
    {:else if filtered.length === 0}
      <div class="text-ink-500 text-sm text-center py-10">
        {rows.length ? 'No models match your filter.' : 'No models shared on the network yet — be the first: + Share a model, or import one from HuggingFace.'}
      </div>
    {:else}
      <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
        {#each filtered as row (row.entry.cid)}
          {@const c = row.entry.card ?? {}}
          {@const busy = tasks[row.entry.cid]}
          <div class="rounded-lg border bg-ink-900 p-3.5 flex flex-col gap-2 transition {row.local ? 'border-emerald-500/40' : 'border-ink-700 hover:border-ink-600'}">
            <div class="flex items-start gap-2.5">
              <!-- circular avatar, HuggingFace-style -->
              <button class="shrink-0" on:click={() => (detail = row)}>
                {#if c.image}
                  <img src={imageSrc(row.entry.cid, c.image)} alt="" class="w-11 h-11 rounded-full object-cover border border-ink-700 bg-ink-950" loading="lazy" />
                {:else}
                  <div class="w-11 h-11 rounded-full bg-ink-800 border border-ink-700 flex items-center justify-center text-xl">{KIND_ICON[c.kind || 'other'] ?? '📦'}</div>
                {/if}
              </button>
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-1.5">
                  <button class="font-mono text-sm text-ink-100 truncate hover:text-cursed-300 text-left" on:click={() => (detail = row)} title={row.entry.name}>{row.entry.name}</button>
                  {#if row.entry.verified}<span class="shrink-0 text-cursed-300" title="Verified — every file's SHA-256 matches the hash HuggingFace published">✓</span>{/if}
                </div>
                <div class="text-[11px] text-ink-500 font-mono flex flex-wrap gap-x-2">
                  {#if c.params}<span>{c.params}</span>{/if}
                  {#if c.quant}<span>{c.quant}</span>{/if}
                  <span>{fmtBytes(row.entry.size_bytes)}</span>
                </div>
              </div>
              {#if c.kind}<span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-ink-800 text-cursed-300 shrink-0">{c.kind}</span>{/if}
            </div>

            {#if c.description}<div class="text-[11px] text-ink-400 line-clamp-2">{c.description}</div>{/if}
            {#if c.base_model}<div class="text-[10px] text-ink-500 font-mono truncate">base: {c.base_model}</div>{/if}

            <div class="flex flex-wrap gap-1">
              {#each row.hosts.slice(0, 3) as h}
                <span class="text-[10px] font-mono px-1.5 py-0.5 rounded {h.is_self ? 'bg-emerald-900/50 text-emerald-300' : 'bg-ink-800 text-ink-400'}"
                      title={h.is_self ? 'hosted on this Orb' : 'hosted by ' + h.label}>{h.is_self ? 'this orb' : h.label}</span>
              {/each}
              {#if row.hosts.length > 3}<span class="text-[10px] font-mono text-ink-600">+{row.hosts.length - 3}</span>{/if}
            </div>

            <div class="mt-auto pt-1 flex items-center gap-2 text-xs">
              {#if busy}
                <span class="text-amber-300 font-mono">{busy}</span>
              {:else}
                <!-- Primary action on every model: push it to a connected system. -->
                <button class="btn-primary text-xs py-1 px-2.5 rounded inline-flex items-center gap-1"
                        on:click={() => openPush(row.entry)} title="Push this model to a connected system (DGX / gateway)">⇧ Push to server</button>
                {#if row.local}
                  <button class="ml-auto text-ink-400 hover:text-cursed-300" on:click={() => editModel(row)} title="Edit this model's card, image + README">edit</button>
                  <a href={downloadUrl(row.entry)} class="text-ink-400 hover:text-cursed-300" title="Download the file from your gateway">save</a>
                  <button class="{confirmRemove === row.entry.cid ? 'text-red-400' : 'text-ink-500 hover:text-red-400'}" on:click={() => unshare(row.entry.cid)}>{confirmRemove === row.entry.cid ? 'sure?' : 'unshare'}</button>
                {:else}
                  <button class="ml-auto text-ink-500 hover:text-cursed-300" on:click={() => download(row)} title="Download + host on this Orb">↓ host</button>
                  <button class="text-ink-500 hover:text-cursed-300" on:click={() => (detail = row)}>details</button>
                {/if}
              {/if}
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </main>
</div>

<!-- ── Share / Edit form modal ──────────────────────────────────────────── -->
{#if showShare && (pendingFile || editingCid)}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-40 bg-black/60 flex items-center justify-center p-4" on:click={() => (showShare = false)}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="w-full max-w-lg rounded-xl border border-ink-700 bg-ink-900 p-5 space-y-3 max-h-[90vh] overflow-y-auto" on:click|stopPropagation>
      <div class="flex items-center justify-between">
        <h2 class="font-mono text-cursed-300">{editingCid ? 'Edit model card' : `Share “${pendingFile?.name}”`}</h2>
        {#if pendingFile}<span class="text-xs text-ink-500 font-mono">{fmtBytes(pendingFile.size)}</span>{/if}
      </div>
      <p class="text-[11px] text-ink-500">
        {#if editingCid}Editing the card, image + README rebuilds the model's IPFS entry — the weights are reused, never re-uploaded.{:else}Fill the model card — it's published inside the IPFS directory alongside the weights so anyone can read it before downloading.{/if}
      </p>

      <!-- image + name row -->
      <div class="flex gap-3">
        <div class="shrink-0">
          <div class="w-24 h-24 rounded-full border border-ink-700 bg-ink-950 overflow-hidden flex items-center justify-center">
            {#if imageB64}<img src={imageB64} alt="" class="w-full h-full object-cover" />
            {:else if existingImageUrl}<img src={existingImageUrl} alt="" class="w-full h-full object-cover" />
            {:else}<span class="text-ink-600 text-3xl">🖼️</span>{/if}
          </div>
          <div class="flex items-center justify-center gap-2 mt-1">
            <button class="text-[11px] text-cursed-300 hover:underline" on:click={() => imageInput?.click()}>{imageB64 || existingImageUrl ? 'replace' : 'add image'}</button>
            {#if imageB64 || existingImageUrl}<button class="text-[11px] text-ink-500 hover:text-red-400" on:click={clearImage}>remove</button>{/if}
          </div>
          <input type="file" accept="image/*" bind:this={imageInput} class="hidden" on:change={onImage} />
        </div>
        <label class="flex-1 block text-xs font-mono text-ink-400">Name
          <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.name} placeholder="Qwen3-VL 8B Instruct" />
          <span class="block mt-2">Base model</span>
          <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.base_model} placeholder="Qwen/Qwen3-VL-8B" />
        </label>
      </div>

      <div class="grid grid-cols-2 gap-2">
        <label class="block text-xs font-mono text-ink-400">Kind
          <select class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.kind}>
            {#each KINDS.slice(1) as k}<option value={k}>{k}</option>{/each}
          </select></label>
        <label class="block text-xs font-mono text-ink-400">Format
          <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.format} placeholder="gguf / safetensors / hef" /></label>
        <label class="block text-xs font-mono text-ink-400">Parameters
          <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.params} placeholder="8B" /></label>
        <label class="block text-xs font-mono text-ink-400">Quantization
          <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.quant} placeholder="int4 / q4_k_m" /></label>
      </div>
      <label class="block text-xs font-mono text-ink-400">Description
        <textarea rows="2" class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.description} placeholder="What it is, how it was trained/tuned, notable strengths."></textarea></label>
      <label class="block text-xs font-mono text-ink-400">Intended use
        <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.intended_use} placeholder="GUI grounding for agent computer-use" /></label>
      <div class="grid grid-cols-2 gap-2">
        <label class="block text-xs font-mono text-ink-400">License
          <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.license} placeholder="apache-2.0" /></label>
        <label class="block text-xs font-mono text-ink-400">Tags (comma-sep)
          <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.tags} placeholder="grounding, agent, vision" /></label>
      </div>
      <label class="block text-xs font-mono text-ink-400">README (markdown)
        <textarea rows="5" class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm font-mono" bind:value={form.readme} placeholder="# Usage&#10;How to run it, prompt format, benchmarks, credits…"></textarea></label>

      <div class="flex justify-end gap-2 pt-1">
        <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => (showShare = false)}>Cancel</button>
        <button class="btn-primary text-sm px-4 py-1.5 rounded disabled:opacity-50" on:click={submitShare} disabled={saving || uploadPct >= 0}>
          {editingCid ? (saving ? 'Saving…' : 'Save changes') : 'Publish to network'}
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- ── Detail / model-card modal ────────────────────────────────────────── -->
{#if detail}
  {@const c = detail.entry.card ?? {}}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-40 bg-black/60 flex items-center justify-center p-4" on:click={() => (detail = null)}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="w-full max-w-lg rounded-xl border border-ink-700 bg-ink-900 p-5 space-y-3 max-h-[90vh] overflow-y-auto" on:click|stopPropagation>
      <div class="flex items-start gap-4">
        {#if c.image}
          <img src={imageSrc(detail.entry.cid, c.image)} alt="" class="w-20 h-20 rounded-full object-cover border border-ink-700 bg-ink-950 shrink-0" />
        {:else}
          <div class="w-20 h-20 rounded-full bg-ink-800 border border-ink-700 flex items-center justify-center text-3xl shrink-0">{KIND_ICON[c.kind || 'other'] ?? '📦'}</div>
        {/if}
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <div class="font-mono text-lg text-ink-100 truncate">{detail.entry.name}</div>
            {#if detail.entry.verified}
              <span class="shrink-0 text-[10px] font-mono px-1.5 py-0.5 rounded bg-cursed-500/15 text-cursed-300 border border-cursed-500/40" title="Every file's SHA-256 matches the hash HuggingFace published">✓ verified</span>
            {/if}
          </div>
          <div class="text-xs text-ink-500 font-mono">{fmtBytes(detail.entry.size_bytes)} · {c.kind || 'model'}{c.format ? ' · ' + c.format : ''}</div>
          {#if detail.entry.source}
            <a href={detail.entry.source} target="_blank" rel="noreferrer" class="text-xs text-cursed-300 hover:underline font-mono break-all">{detail.entry.source.replace('https://', '')}</a>
          {/if}
        </div>
        {#if detail.local}<button class="text-xs text-cursed-300 hover:underline shrink-0" on:click={() => detail && editModel(detail)}>edit</button>{/if}
      </div>
      {#if c.description}<p class="text-sm text-ink-300 leading-relaxed">{c.description}</p>{/if}
      <div class="grid grid-cols-2 gap-x-4 gap-y-1.5 text-xs font-mono">
        {#if c.base_model}<div class="text-ink-500">base model</div><div class="text-ink-200 text-right truncate">{c.base_model}</div>{/if}
        {#if c.params}<div class="text-ink-500">parameters</div><div class="text-ink-200 text-right">{c.params}</div>{/if}
        {#if c.quant}<div class="text-ink-500">quantization</div><div class="text-ink-200 text-right">{c.quant}</div>{/if}
        {#if c.license}<div class="text-ink-500">license</div><div class="text-ink-200 text-right">{c.license}</div>{/if}
        {#if c.intended_use}<div class="text-ink-500">intended use</div><div class="text-ink-200 text-right">{c.intended_use}</div>{/if}
        {#if detail.entry.origin_label}<div class="text-ink-500">shared by</div><div class="text-ink-200 text-right truncate">{detail.entry.origin_label}</div>{/if}
        <div class="text-ink-500">sha256</div><div class="text-ink-200 text-right truncate" title={detail.entry.sha256}>{(detail.entry.sha256 ?? '').slice(0, 16)}…</div>
      </div>
      {#if c.tags?.length}
        <div class="flex flex-wrap gap-1">
          {#each c.tags as t}<span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-ink-800 text-ink-300">{t}</span>{/each}
        </div>
      {/if}
      {#if c.readme}
        <div class="border-t border-ink-800 pt-3">
          <div class="text-[11px] font-mono text-ink-500 uppercase tracking-wider mb-1">README</div>
          {#if detailReadme}
            <div class="text-sm text-ink-300 leading-relaxed">{@html renderMd(detailReadme)}</div>
          {:else}
            <div class="text-xs text-ink-600">loading…</div>
          {/if}
        </div>
      {/if}
      <div class="text-[11px] text-ink-500 font-mono">
        hosts: {detail.hosts.map((h) => (h.is_self ? 'this orb' : h.label)).join(', ')}
      </div>
      <div class="text-[10px] text-ink-600 font-mono break-all">CID {detail.entry.cid}</div>

      <div class="flex justify-end gap-2 pt-1 border-t border-ink-800">
        <button class="btn-primary text-sm px-3 py-1.5 rounded mr-auto inline-flex items-center gap-1"
                on:click={() => detail && openPush(detail.entry)}>⇧ Push to server</button>
        <a href={downloadUrl(detail.entry)} class="btn text-sm px-3 py-1.5 rounded">Save file</a>
        {#if detail.local}
          <span class="self-center text-emerald-400 font-mono text-xs">✓ hosted here</span>
        {:else if tasks[detail.entry.cid]}
          <span class="self-center text-amber-300 font-mono text-xs">{tasks[detail.entry.cid]}</span>
        {:else}
          <button class="btn-primary text-sm px-4 py-1.5 rounded" on:click={() => detail && download(detail)}>↓ Download + host</button>
        {/if}
      </div>
    </div>
  </div>
{/if}

<!-- ── Push-to-server dialog: pick a connected system + destination folder ── -->
{#if pushEntry}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-50 bg-black/60 flex items-center justify-center p-4" on:click={closePush}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="w-full max-w-md rounded-xl border border-ink-700 bg-ink-900 p-5 space-y-4 max-h-[90vh] overflow-y-auto" on:click|stopPropagation>
      <div class="flex items-center justify-between gap-2">
        <h2 class="font-mono text-cursed-300 text-sm truncate">⇧ Push “{pushEntry.name}”</h2>
        <button class="text-ink-500 hover:text-ink-200 shrink-0" on:click={closePush}>✕</button>
      </div>

      {#if !systems.length}
        <div class="text-center py-4 space-y-3">
          <p class="text-sm text-ink-300">Pushing a model needs a server to push it to — a DGX, an agent
            gateway, or any box you've SSH-linked. You don't have any connected yet.</p>
          <a href="/agent" class="inline-block bg-cursed-600 hover:bg-cursed-500 text-white font-mono text-sm px-4 py-2 rounded-lg transition-colors">↳ Connect your first system</a>
          <p class="text-xs text-ink-500">Set it up once in the Agent Dashboard, then come back and push.</p>
        </div>
        <div class="flex justify-end"><button class="btn text-sm px-3 py-1.5 rounded" on:click={closePush}>Close</button></div>
      {:else}
        <!-- 1. target system -->
        <label class="block text-xs font-mono text-ink-400">Target system
          <select bind:value={pushSel} class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm">
            {#each systems as s}
              <option value={s.id}>{s.label || s.address}{s.roles?.length ? ' · ' + s.roles.join('/') : ''}</option>
            {/each}
          </select>
        </label>

        <!-- 2. destination -->
        <div class="space-y-1.5">
          <div class="text-xs font-mono text-ink-400">Destination folder</div>
          <label class="flex items-center gap-2 text-sm text-ink-200 cursor-pointer">
            <input type="radio" bind:group={pushMode} value="home" /> Home folder <code class="text-ink-500 text-xs">~/aeon-models/</code>
          </label>
          <label class="flex items-center gap-2 text-sm text-ink-200 cursor-pointer">
            <input type="radio" bind:group={pushMode} value="custom" on:change={startBrowse} /> Browse the target…
          </label>
        </div>

        <!-- remote folder browser -->
        {#if pushMode === 'custom'}
          <div class="rounded border border-ink-700 bg-ink-950/50 p-2 space-y-1.5">
            <div class="flex items-center gap-2 text-[11px] font-mono">
              <button class="text-cursed-300 hover:text-cursed-200 disabled:opacity-40" on:click={() => browseTo(browseParent)} disabled={!browseParent || browseLoading} title="Up one level">⬆</button>
              <span class="truncate flex-1 text-ink-300" title={browsePath}>{browsePath || '…'}</span>
              {#if browseLoading}<span class="text-ink-500">…</span>{/if}
            </div>
            {#if browseErr}<div class="text-[11px] text-red-400 font-mono">{browseErr}</div>{/if}
            <div class="max-h-40 overflow-y-auto space-y-0.5">
              {#each browseDirs as d}
                <button class="w-full text-left text-xs font-mono text-ink-300 hover:text-cursed-300 hover:bg-ink-800 rounded px-2 py-1 truncate"
                        on:click={() => browseTo(browsePath.replace(/\/$/, '') + '/' + d)}>📁 {d}</button>
              {:else}
                {#if !browseLoading && !browseErr}<div class="text-[11px] text-ink-600 px-2 py-1">no sub-folders here</div>{/if}
              {/each}
            </div>
            {#if browsePath}
              <div class="text-[10px] text-ink-600 leading-snug break-all">Lands in <code class="text-ink-500">{browsePath.replace(/\/$/, '')}/{slugify(pushEntry.name)}</code></div>
            {/if}
          </div>
        {/if}

        {#if pushMsg}
          <div class="text-xs font-mono {pushMsg.startsWith('error') ? 'text-red-400' : pushMsg.startsWith('✓') ? 'text-emerald-400' : 'text-amber-300'} break-all">{pushMsg}</div>
        {/if}

        <div class="flex justify-end gap-2 pt-1">
          <button class="btn text-sm px-3 py-1.5 rounded" on:click={closePush}>Close</button>
          <button class="btn-primary text-sm px-4 py-1.5 rounded disabled:opacity-50"
                  on:click={doPush} disabled={pushActive || !pushSel || (pushMode === 'custom' && !browsePath)}>
            {pushActive ? 'pushing…' : '⇧ Push'}
          </button>
        </div>
        <p class="text-[10px] text-ink-600 leading-snug">Pulls the model down to this Orb (if it isn't already), then rsyncs it to the chosen system over its SSH key.</p>
      {/if}
    </div>
  </div>
{/if}
