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
    tags?: string[]; format?: string;
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
  let pendingFile: File | null = null;
  let form = { name: '', kind: 'llm', base_model: '', params: '', quant: '', format: '', license: '', description: '', intended_use: '', tags: '' };
  let uploadPct = -1;
  let uploadName = '';
  let showShare = false;

  // detail modal
  let detail: Row | null = null;

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

  function pickFile() { fileInput?.click(); }
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

  function submitShare() {
    if (!pendingFile) return;
    const f = pendingFile;
    const card: Card = {
      kind: form.kind, base_model: form.base_model.trim(), params: form.params.trim(),
      quant: form.quant.trim(), format: form.format.trim(), license: form.license.trim(),
      description: form.description.trim(), intended_use: form.intended_use.trim(),
      tags: form.tags.split(',').map((t) => t.trim()).filter(Boolean),
    };
    const q = new URLSearchParams({ name: form.name.trim() || f.name, card: JSON.stringify(card) });
    uploadName = form.name || f.name;
    uploadPct = 0;
    err = '';
    const xhr = new XMLHttpRequest();
    xhr.open('POST', `/api/ipfs/models/upload?${q}`);
    xhr.setRequestHeader('Content-Disposition', `attachment; filename="${f.name}"`);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.withCredentials = true;
    xhr.upload.addEventListener('progress', (e) => {
      if (e.lengthComputable) uploadPct = Math.floor((e.loaded / e.total) * 100);
    });
    xhr.addEventListener('load', () => {
      uploadPct = -1;
      try { const r = JSON.parse(xhr.responseText); if (!r.ok) err = r.err || `HTTP ${xhr.status}`; }
      catch { err = `HTTP ${xhr.status}`; }
      pendingFile = null; showShare = false;
      form = { name: '', kind: 'llm', base_model: '', params: '', quant: '', format: '', license: '', description: '', intended_use: '', tags: '' };
      load();
    });
    xhr.addEventListener('error', () => { uploadPct = -1; err = 'upload failed (network)'; });
    xhr.send(f);
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
                <a href={downloadUrl(row.entry)} class="ml-auto text-ink-400 hover:text-cursed-300" title="Download the file from your gateway">save</a>
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

<!-- ── Share form modal ─────────────────────────────────────────────────── -->
{#if showShare && pendingFile}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-40 bg-black/60 flex items-center justify-center p-4" on:click={() => (showShare = false)}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="w-full max-w-lg rounded-xl border border-ink-700 bg-ink-900 p-5 space-y-3 max-h-[90vh] overflow-y-auto" on:click|stopPropagation>
      <div class="flex items-center justify-between">
        <h2 class="font-mono text-cursed-300">Share “{pendingFile.name}”</h2>
        <span class="text-xs text-ink-500 font-mono">{fmtBytes(pendingFile.size)}</span>
      </div>
      <p class="text-[11px] text-ink-500">Fill the model card — it's published inside the IPFS directory alongside the weights so anyone can read it before downloading.</p>
      <label class="block text-xs font-mono text-ink-400">Name
        <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.name} placeholder="Qwen3-VL 8B Instruct" /></label>
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
      <label class="block text-xs font-mono text-ink-400">Base model
        <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.base_model} placeholder="Qwen/Qwen3-VL-8B" /></label>
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
      <div class="flex justify-end gap-2 pt-1">
        <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => (showShare = false)}>Cancel</button>
        <button class="btn-primary text-sm px-4 py-1.5 rounded" on:click={submitShare}>Publish to network</button>
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
      <div class="flex items-start gap-3">
        <div class="text-2xl">{KIND_ICON[c.kind || 'other'] ?? '📦'}</div>
        <div class="min-w-0 flex-1">
          <div class="font-mono text-lg text-ink-100">{detail.entry.name}</div>
          <div class="text-xs text-ink-500 font-mono">{fmtBytes(detail.entry.size_bytes)} · {c.kind || 'model'}{c.format ? ' · ' + c.format : ''}</div>
        </div>
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
