<script lang="ts">
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { onMount, onDestroy } from 'svelte';
  import qrcode from 'qrcode-generator';
  import StorageManager from '$lib/components/StorageManager.svelte';
  import { getFleetRoster, type FleetOrb, type IpfsModel } from '$lib/api';

  type Status = {
    ok: boolean; enabled: boolean; installed: boolean; daemon: string;
    version: string; peer_id: string; peers: number; repo_bytes: number;
    storage_max: string; gateway_port: number;
    disk_free_bytes?: number; disk_total_bytes?: number;
  };

  /// An index row: one model (by CID) + every fleet Orb that hosts it.
  type IndexRow = {
    m: IpfsModel;
    hosts: { label: string; addrs: string[]; peer_id: string; online: boolean; is_self: boolean }[];
  };

  let status: Status | null = null;
  let pins: string[] = [];
  let err = '';
  let busy = '';
  let poll: ReturnType<typeof setInterval>;

  // Startup watchdog. "starting" is a DERIVED state (enabled=true but the daemon
  // isn't active yet); without a deadline a dead/crash-looping daemon would show
  // amber "starting" forever. If the backend reports the daemon 'failed' we show
  // that immediately; otherwise, if it hasn't come up within STALL_MS we flip to
  // 'stalled' with a diagnostic hint — so the UI always escapes "starting", even
  // against an older backend that only reports active/inactive.
  const STALL_MS = 120_000;
  let startedAt = Date.now();
  // Recomputed every 4s poll (each `status` reassignment retriggers this).
  $: uiState = !status
    ? 'off'
    : status.daemon === 'active'
      ? 'running'
      : status.daemon === 'failed'
        ? 'failed'
        : status.enabled && Date.now() - startedAt > STALL_MS
          ? 'stalled'
          : status.enabled
            ? 'starting'
            : 'off';

  let cid = '';
  let copied = '';

  // ── AI model sharing state ──
  let models: IpfsModel[] = [];
  let tasks: Record<string, string> = {};
  let fleetIndex: IndexRow[] = [];
  let fleetConfigured = false;
  let fileInput: HTMLInputElement;
  let uploadPct = -1; // -1 = idle
  let uploadName = '';
  let shareKind = 'llm';
  let shareDesc = '';
  let confirmRemove = ''; // cid armed for two-step unshare

  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/ipfs${path}`, { credentials: 'same-origin', ...opts }).then((r) => r.json());

  async function load() {
    try {
      status = await api('/status');
      err = '';
      // Reset the startup clock while the daemon is healthy, so if it later dies
      // the stall watchdog measures from that point, not page load.
      if (status?.daemon === 'active') startedAt = Date.now();
      if (status?.enabled && status?.daemon === 'active') {
        const p = await api('/pins');
        pins = p?.pins ?? [];
        await loadModels();
        await loadFleetIndex();
      }
    } catch (e: any) {
      err = e?.message ?? 'failed to load';
    }
  }

  async function loadModels() {
    try {
      const r = await api('/models');
      if (r?.ok) {
        models = r.models ?? [];
        tasks = r.tasks ?? {};
      }
    } catch {}
  }

  // The federated index = union of ipfs_models over the fleet roster (self +
  // peers), grouped by CID so a model mirrored on several Orbs is one row
  // with multiple host chips.
  async function loadFleetIndex() {
    try {
      const roster = await getFleetRoster();
      fleetConfigured = roster?.configured ?? false;
      const orbs: FleetOrb[] = [roster.self, ...(roster.peers ?? [])].filter(Boolean);
      const byCid = new Map<string, IndexRow>();
      for (const o of orbs) {
        const im = o.ipfs_models;
        if (!im?.enabled || !im.models?.length) continue;
        const host = {
          label: o.label || o.hostname || o.addr || o.id || 'orb',
          addrs: (o.addrs?.length ? o.addrs : [o.lan_ip || o.addr || '']).filter(Boolean) as string[],
          peer_id: im.peer_id ?? '',
          online: o.online !== false,
          is_self: !!o.is_self,
        };
        for (const m of im.models) {
          const row = byCid.get(m.cid);
          if (row) row.hosts.push(host);
          else byCid.set(m.cid, { m, hosts: [host] });
        }
      }
      fleetIndex = [...byCid.values()].sort((a, b) => (b.m.added_at_ms ?? 0) - (a.m.added_at_ms ?? 0));
    } catch {}
  }

  $: localCids = new Set(models.map((m) => m.cid));

  // Streaming upload with progress — same raw-body XHR contract as the ISO
  // upload (Content-Disposition carries the filename; server streams to disk).
  function uploadModel(f: File) {
    uploadName = f.name;
    uploadPct = 0;
    err = '';
    const q = new URLSearchParams({ kind: shareKind, desc: shareDesc.trim() });
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
      try {
        const r = JSON.parse(xhr.responseText);
        if (!r.ok) err = r.err || `HTTP ${xhr.status}`;
      } catch { err = `HTTP ${xhr.status}`; }
      shareDesc = '';
      loadModels();
    });
    xhr.addEventListener('error', () => { uploadPct = -1; err = 'upload failed (network)'; });
    xhr.send(f);
  }

  function onModelFile() {
    const f = fileInput?.files?.[0];
    if (f) uploadModel(f);
    if (fileInput) fileInput.value = '';
  }

  // Pin a fleet peer's model here (mirror it): pass the source Orb's
  // addresses + PeerID so the node swarm-connects directly instead of
  // waiting on DHT routing.
  async function fetchModel(row: IndexRow) {
    const src = row.hosts.find((h) => !h.is_self && h.online && h.peer_id) ?? row.hosts[0];
    err = '';
    try {
      const r = await api('/models/fetch', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          ...row.m,
          addrs: src?.addrs ?? [],
          peer_id: src?.peer_id ?? '',
        }),
      });
      if (r && r.ok === false) err = r.err || 'fetch failed';
    } catch (e: any) {
      err = e?.message ?? 'fetch failed';
    }
    await loadModels();
  }

  async function removeModel(c: string) {
    if (confirmRemove !== c) {
      confirmRemove = c;
      setTimeout(() => { if (confirmRemove === c) confirmRemove = ''; }, 3000);
      return;
    }
    confirmRemove = '';
    try {
      await api('/models/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid: c }),
      });
    } catch {}
    await load();
  }

  const shortCid = (c: string) => (c.length > 16 ? `${c.slice(0, 8)}…${c.slice(-6)}` : c);

  async function enable() {
    busy = 'Installing kubo + starting…';
    startedAt = Date.now(); // restart the stall watchdog for this attempt
    try { await api('/enable', { method: 'POST' }); } catch {}
    busy = ''; await load();
  }
  async function disable() {
    busy = 'Stopping…';
    try { await api('/disable', { method: 'POST' }); } catch {}
    busy = ''; await load();
  }

  async function pin() {
    if (!cid.trim()) return;
    err = ''; busy = 'Pinning…';
    try {
      const r = await api('/pin', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid: cid.trim() }),
      });
      if (r && r.ok === false) err = typeof r.err === 'string' ? r.err : JSON.stringify(r.err);
      else cid = '';
    } catch (e: any) {
      err = e?.message ?? 'pin failed';
    }
    busy = ''; await load();
  }

  async function unpin(c: string) {
    busy = 'Unpinning…';
    try {
      await api('/unpin', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid: c }),
      });
    } catch {}
    busy = ''; await load();
  }

  async function copy(text: string, id: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = id;
      setTimeout(() => (copied = ''), 1200);
    } catch {}
  }

  function fmtBytes(n: number): string {
    if (!n) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let x = n;
    while (x >= 1024 && i < u.length - 1) {
      x /= 1024;
      i++;
    }
    return `${x.toFixed(1)} ${u[i]}`;
  }

  $: gatewayUrl =
    typeof location !== 'undefined' ? `http://${location.hostname}:${status?.gateway_port ?? 8080}` : '';
  $: qrSvg = (() => {
    if (!gatewayUrl) return '';
    try {
      const q = qrcode(0, 'M');
      q.addData(gatewayUrl);
      q.make();
      return q.createSvgTag({ cellSize: 3, margin: 2 });
    } catch {
      return '';
    }
  })();

  onMount(() => {
    load();
    poll = setInterval(load, 4000);
  });
  onDestroy(() => clearInterval(poll));
</script>

<div class="min-h-screen bg-ink-950 text-ink-100">
  <PageHeader title="IPFS" backHref="/orbnet" backLabel="ORBNET" />

  <main class="max-w-3xl mx-auto px-5 py-6 space-y-6">
    <p class="text-ink-300 text-sm leading-relaxed">
      Run a local <span class="text-cursed-300">IPFS</span> node + HTTP gateway. Host content on the decentralized
      web, pin what you want to keep available, and reach the gateway from any device on your LAN or Tailscale.
    </p>

    {#if err}
      <div class="rounded border border-red-700 bg-red-950/40 text-red-300 px-3 py-2 text-sm">{err}</div>
    {/if}

    <div class="flex items-center gap-3 flex-wrap">
      <span
        class="font-mono text-[10px] uppercase tracking-wider px-2 py-1 rounded {uiState === 'running'
          ? 'bg-emerald-900/50 text-emerald-300'
          : uiState === 'failed' || uiState === 'stalled'
            ? 'bg-red-900/50 text-red-300'
            : uiState === 'starting'
              ? 'bg-amber-900/50 text-amber-300'
              : 'bg-ink-800 text-ink-400'}"
      >{uiState}</span>
      {#if status?.daemon === 'active'}
        <span class="text-ink-400 text-xs"
          >{status.peers} peers · {fmtBytes(status.repo_bytes)} / {status.storage_max} · kubo {status.version}</span>
      {/if}
      {#if uiState === 'failed' || uiState === 'stalled'}
        <span class="text-red-300/80 text-xs"
          >daemon didn't come up — SSH in and check <code class="font-mono">journalctl -u aeon-ipfs</code></span>
      {/if}
      <div class="flex-1"></div>
      {#if busy}<span class="text-xs text-cursed-300">{busy}</span>{/if}
      {#if status?.enabled}
        <button class="text-xs text-ink-400 hover:text-ink-200" on:click={disable} disabled={!!busy}>Disable</button>
      {:else}
        <button class="text-xs text-cursed-300 hover:text-cursed-200" on:click={enable} disabled={!!busy}>Enable</button>
      {/if}
    </div>

    {#if status?.daemon === 'active'}
      <div class="rounded-lg border border-ink-700 bg-ink-900 p-4 flex gap-4 items-center">
        <div class="bg-white p-1 rounded shrink-0 w-32 h-32 flex items-center justify-center [&_svg]:w-full [&_svg]:h-full">{@html qrSvg}</div>
        <div class="text-sm space-y-1 min-w-0">
          <div class="font-mono text-cursed-300">Your IPFS gateway</div>
          <div class="flex items-center gap-2">
            <code class="text-cursed-300 text-xs break-all">{gatewayUrl}</code>
            <button class="text-xs text-ink-400 hover:text-cursed-300 shrink-0" on:click={() => copy(gatewayUrl, 'gw')}>{copied === 'gw' ? '✓' : 'copy'}</button>
          </div>
          <div class="text-xs text-ink-400 leading-relaxed">
            Scan to open it on a phone, or point any device at it (a browser, or set it as your gateway in IPFS
            Companion). Fetch content at <code class="text-ink-300">{gatewayUrl}/ipfs/&lt;CID&gt;</code>. Works over
            Tailscale too — swap in your Orb's tailnet name.
          </div>
        </div>
      </div>

      <!-- Storage allocation + external drives + NAS (shared with Model Share). -->
      <StorageManager />

      <div class="rounded-lg border border-ink-700 bg-ink-900 p-4 space-y-3">
        <h2 class="font-mono text-cursed-300 text-sm">Pin content (host a CID)</h2>
        <div class="flex gap-2">
          <input class="flex-1 bg-ink-800 border border-ink-600 rounded px-2 py-1 text-ink-100 text-sm font-mono" bind:value={cid} placeholder="Qm… or bafy… CID" />
          <button class="font-mono text-sm px-3 py-1 rounded bg-cursed-700 hover:bg-cursed-600 text-white disabled:opacity-50" on:click={pin} disabled={!!busy}>Pin</button>
        </div>
        {#if pins.length}
          <div class="space-y-1">
            {#each pins as c (c)}
              <div class="flex items-center justify-between gap-2 text-xs">
                <code class="text-cursed-300 break-all">{c}</code>
                <div class="flex gap-2 shrink-0">
                  <a href="{gatewayUrl}/ipfs/{c}" target="_blank" rel="noreferrer" class="text-ink-400 hover:text-cursed-300">open</a>
                  <button class="text-ink-500 hover:text-red-400" on:click={() => unpin(c)}>unpin</button>
                </div>
              </div>
            {/each}
          </div>
        {:else}
          <div class="text-ink-500 text-xs">No pins yet. Paste a CID to host it, or have an agent publish a site with the <code class="text-ink-400">ipfs_add</code> tool.</div>
        {/if}
      </div>

      <!-- ── AI models shared from this Orb ─────────────────────────────── -->
      <div class="rounded-lg border border-ink-700 bg-ink-900 p-4 space-y-3">
        <div class="flex items-center justify-between gap-2 flex-wrap">
          <h2 class="font-mono text-cursed-300 text-sm">AI models — shared from this Orb</h2>
          <div class="flex items-center gap-2">
            <select bind:value={shareKind}
                    class="bg-ink-800 border border-ink-600 rounded px-2 py-1 text-ink-100 text-xs font-mono">
              <option value="llm">llm</option>
              <option value="vlm">vlm</option>
              <option value="vision">vision</option>
              <option value="stt">stt</option>
              <option value="tts">tts</option>
              <option value="other">other</option>
            </select>
            <button class="font-mono text-xs px-3 py-1 rounded bg-cursed-700 hover:bg-cursed-600 text-white disabled:opacity-50"
                    on:click={() => fileInput?.click()} disabled={uploadPct >= 0}>Share a model…</button>
            <input type="file" bind:this={fileInput} class="hidden" on:change={onModelFile} />
          </div>
        </div>
        <input class="w-full bg-ink-800 border border-ink-600 rounded px-2 py-1 text-ink-100 text-xs font-mono"
               bind:value={shareDesc} placeholder="optional description (e.g. Qwen3-VL 8B, int4, GUI grounding)" />

        {#if uploadPct >= 0}
          <div class="space-y-1">
            <div class="flex justify-between text-xs font-mono text-ink-400">
              <span class="truncate">{uploadName}</span><span>{uploadPct}%</span>
            </div>
            <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
              <div class="h-full bg-cursed-500 transition-all duration-150" style="width:{uploadPct}%"></div>
            </div>
          </div>
        {/if}

        {#each Object.entries(tasks) as [key, phase] (key)}
          <div class="flex items-center gap-2 text-xs font-mono {phase.startsWith('error') ? 'text-red-400' : 'text-amber-300'}">
            {#if !phase.startsWith('error')}
              <span class="inline-block h-3 w-3 rounded-full border-2 border-amber-400 border-t-transparent animate-spin"></span>
            {/if}
            <span class="truncate">{key}</span><span class="text-ink-500">·</span><span>{phase}</span>
          </div>
        {/each}

        {#if models.length}
          <div class="divide-y divide-ink-800">
            {#each models as m (m.cid)}
              <div class="py-2 flex items-center gap-3">
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-2">
                    <span class="font-mono text-sm text-ink-100 truncate">{m.name}</span>
                    {#if m.kind}<span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-ink-800 text-cursed-300">{m.kind}</span>{/if}
                  </div>
                  <div class="text-[11px] text-ink-500 font-mono flex items-center gap-2 flex-wrap">
                    <span>{fmtBytes(m.size_bytes)}</span>
                    {#if m.origin_label && m.origin_id !== status?.peer_id}<span>from {m.origin_label}</span>{/if}
                    <button class="hover:text-cursed-300" title={m.cid} on:click={() => copy(m.cid, m.cid)}>
                      {copied === m.cid ? '✓ copied' : shortCid(m.cid)}
                    </button>
                  </div>
                  {#if m.desc}<div class="text-[11px] text-ink-400 truncate">{m.desc}</div>{/if}
                </div>
                <div class="flex gap-2 shrink-0 text-xs">
                  <a href="{gatewayUrl}/ipfs/{m.cid}?filename={encodeURIComponent(m.name)}" target="_blank" rel="noreferrer"
                     class="text-ink-400 hover:text-cursed-300">open</a>
                  <button class="{confirmRemove === m.cid ? 'text-red-400' : 'text-ink-500 hover:text-red-400'}"
                          on:click={() => removeModel(m.cid)}>
                    {confirmRemove === m.cid ? 'sure?' : 'unshare'}
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {:else if !Object.keys(tasks).length}
          <div class="text-ink-500 text-xs">
            Nothing shared yet. Upload a model file (GGUF, HEF, ONNX, safetensors…) — it's added to IPFS, pinned,
            and announced to your fleet so every Orb can pull it.
          </div>
        {/if}
      </div>

      <!-- ── Federated fleet model index ────────────────────────────────── -->
      <div class="rounded-lg border border-ink-700 bg-ink-900 p-4 space-y-3">
        <h2 class="font-mono text-cursed-300 text-sm">Fleet model index</h2>
        {#if fleetIndex.length}
          <div class="divide-y divide-ink-800">
            {#each fleetIndex as row (row.m.cid)}
              <div class="py-2 flex items-center gap-3">
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-2">
                    <span class="font-mono text-sm text-ink-100 truncate">{row.m.name}</span>
                    {#if row.m.kind}<span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-ink-800 text-cursed-300">{row.m.kind}</span>{/if}
                    <span class="text-[11px] text-ink-500 font-mono">{fmtBytes(row.m.size_bytes)}</span>
                  </div>
                  {#if row.m.desc}<div class="text-[11px] text-ink-400 truncate">{row.m.desc}</div>{/if}
                  <div class="flex items-center gap-1.5 flex-wrap mt-0.5">
                    {#each row.hosts as h}
                      <span class="text-[10px] font-mono px-1.5 py-0.5 rounded {h.is_self ? 'bg-cursed-900/50 text-cursed-300' : h.online ? 'bg-emerald-900/50 text-emerald-300' : 'bg-ink-800 text-ink-500'}"
                            title={h.online ? 'hosting now' : 'offline'}>
                        {h.is_self ? 'this orb' : h.label}
                      </span>
                    {/each}
                    {#if row.m.origin_label}<span class="text-[10px] font-mono text-ink-600">origin: {row.m.origin_label}</span>{/if}
                  </div>
                </div>
                <div class="shrink-0 text-xs">
                  {#if localCids.has(row.m.cid)}
                    <span class="text-emerald-400 font-mono">pinned ✓</span>
                  {:else if tasks[row.m.cid]}
                    <span class="text-amber-300 font-mono">{tasks[row.m.cid]}</span>
                  {:else}
                    <button class="font-mono px-3 py-1 rounded bg-cursed-700 hover:bg-cursed-600 text-white"
                            on:click={() => fetchModel(row)}>Pin here</button>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {:else}
          <div class="text-ink-500 text-xs">
            {#if fleetConfigured}
              No models shared across the fleet yet — share one above, or from any other Orb's IPFS console.
            {:else}
              Fleet not configured — set a shared token + seeds on the <a href="/fleet" class="text-cursed-400 hover:underline">Fleet page</a>
              and every fleet Orb's shared models will appear here.
            {/if}
          </div>
        {/if}
      </div>
    {:else if status?.enabled}
      <div class="text-ink-400 text-sm text-center py-6">{busy || 'Starting IPFS… (first run downloads kubo, ~30 MB)'}</div>
    {:else}
      <div class="text-ink-500 text-sm text-center py-6">IPFS is off. Enable it to stand up your node + gateway.</div>
    {/if}
  </main>
</div>
