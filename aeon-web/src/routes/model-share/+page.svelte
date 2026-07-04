<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  // Model Share — a friendly, fleet-FREE marketplace of AI models shared over
  // IPFS. Every Orb auto-enrolls at boot and gossips its catalog on a pubsub
  // topic, so this console shows models from every Orb on the network with no
  // token or fleet enrollment. Upload a model (with a model card), browse what
  // others share, and download (pin) any of them to host it here too.

  type Card = {
    kind?: string; base_model?: string; params?: string; quant?: string;
    license?: string; description?: string; intended_use?: string;
    tags?: string[]; format?: string; image?: string; readme?: boolean;
  };
  type Entry = {
    cid: string; name: string; file?: string; size_bytes: number;
    sha256?: string; card?: Card; added_at_ms?: number;
    origin_id?: string; origin_label?: string;
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
    if (c.image) existingImageUrl = `${gatewayBase}/ipfs/${row.entry.cid}/${c.image}`;
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

  onMount(() => { load(); poll = setInterval(load, 5000); });
  onDestroy(() => clearInterval(poll));
</script>

<div class="min-h-screen bg-ink-950 text-ink-100">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
    <h1 class="font-mono text-lg text-cursed-300">🛰️ Model Share</h1>
    <div class="w-28 text-right">
      {#if node?.daemon === 'active'}
        <span class="text-[10px] font-mono uppercase tracking-wider px-2 py-1 rounded bg-emerald-900/50 text-emerald-300"
              title="This Orb is enrolled in the IPFS Model Share network">on-network</span>
      {:else}
        <span class="text-[10px] font-mono uppercase tracking-wider px-2 py-1 rounded bg-amber-900/50 text-amber-300">connecting…</span>
      {/if}
    </div>
  </header>

  <main class="max-w-5xl mx-auto px-5 py-6 space-y-5">
    <p class="text-ink-300 text-sm leading-relaxed">
      A shared library of AI models across every Aeon Orb — no fleet, no accounts. Every Orb auto-joins the
      IPFS network at boot and announces the models it hosts, so what you see below is contributed by Orbs
      everywhere. <span class="text-cursed-300">Share</span> a model to publish it with a model card;
      <span class="text-cursed-300">download</span> anyone's to run it locally (and help host it).
    </p>

    {#if err}
      <div class="rounded border border-red-700 bg-red-950/40 text-red-300 px-3 py-2 text-sm">{err}</div>
    {/if}

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
    {#if node?.daemon !== 'active'}
      <div class="text-ink-500 text-sm text-center py-10">Connecting to the IPFS network… (first boot downloads kubo, ~30 MB)</div>
    {:else if filtered.length === 0}
      <div class="text-ink-500 text-sm text-center py-10">
        {rows.length ? 'No models match your filter.' : 'No models shared on the network yet — be the first: + Share a model.'}
      </div>
    {:else}
      <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
        {#each filtered as row (row.entry.cid)}
          {@const c = row.entry.card ?? {}}
          {@const busy = tasks[row.entry.cid]}
          <div class="rounded-lg border bg-ink-900 p-3.5 flex flex-col gap-2 transition {row.local ? 'border-emerald-500/40' : 'border-ink-700 hover:border-ink-600'}">
            {#if c.image}
              <button class="block -mx-3.5 -mt-3.5 mb-1 h-24 overflow-hidden rounded-t-lg bg-ink-950" on:click={() => (detail = row)}>
                <img src="{gatewayBase}/ipfs/{row.entry.cid}/{c.image}" alt="" class="w-full h-full object-cover" loading="lazy" />
              </button>
            {/if}
            <div class="flex items-start gap-2">
              <div class="text-xl leading-none mt-0.5">{KIND_ICON[c.kind || 'other'] ?? '📦'}</div>
              <div class="min-w-0 flex-1">
                <button class="font-mono text-sm text-ink-100 truncate hover:text-cursed-300 text-left w-full" on:click={() => (detail = row)} title={row.entry.name}>{row.entry.name}</button>
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
              {:else if row.local}
                <span class="text-emerald-400 font-mono">✓ hosted here</span>
                <button class="ml-auto text-ink-400 hover:text-cursed-300" on:click={() => editModel(row)} title="Edit this model's card, image + README">edit</button>
                <a href={downloadUrl(row.entry)} class="text-ink-400 hover:text-cursed-300" title="Download the file from your gateway">save</a>
                <button class="{confirmRemove === row.entry.cid ? 'text-red-400' : 'text-ink-500 hover:text-red-400'}" on:click={() => unshare(row.entry.cid)}>{confirmRemove === row.entry.cid ? 'sure?' : 'unshare'}</button>
              {:else}
                <button class="btn text-xs py-1 px-2.5 rounded" on:click={() => download(row)}>↓ Download</button>
                <button class="ml-auto text-ink-500 hover:text-cursed-300" on:click={() => (detail = row)}>details</button>
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
          <div class="w-24 h-24 rounded-lg border border-ink-700 bg-ink-950 overflow-hidden flex items-center justify-center">
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
      {#if c.image}
        <img src="{gatewayBase}/ipfs/{detail.entry.cid}/{c.image}" alt="" class="w-full max-h-48 object-cover rounded-lg border border-ink-800" />
      {/if}
      <div class="flex items-start gap-3">
        <div class="text-2xl">{KIND_ICON[c.kind || 'other'] ?? '📦'}</div>
        <div class="min-w-0 flex-1">
          <div class="font-mono text-lg text-ink-100">{detail.entry.name}</div>
          <div class="text-xs text-ink-500 font-mono">{fmtBytes(detail.entry.size_bytes)} · {c.kind || 'model'}{c.format ? ' · ' + c.format : ''}</div>
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
      <div class="flex justify-end gap-2 pt-1">
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
