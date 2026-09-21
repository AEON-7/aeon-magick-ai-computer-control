<script lang="ts">
  // Self-contained storage controls shared by the IPFS page and Model Share:
  //   • Storage shared with the network (the IPFS datastore cap slider)
  //   • External USB/SSD drives (detect / adopt / reformat)
  //   • Optional LAN NAS (Samba)
  // Fetches its own state so it can drop into any page. The slider uses a
  // `dirty` flag (NOT an activeElement check) so background polling never
  // snaps your edit back — it only re-syncs from the node once you Apply.
  //
  // variant="spotlight" (Model Share): drives + LAN are always-visible tiles
  // with a primary button — not a collapsed <details>. variant="full" is the
  // stacked instrument panels on the IPFS page.
  import { onMount, onDestroy } from 'svelte';
  import Icon from './Icon.svelte';

  export let variant: 'full' | 'spotlight' = 'full';
  $: spotlight = variant === 'spotlight';

  type Status = {
    ok: boolean; enabled: boolean; daemon: string; repo_bytes: number;
    storage_max: string; disk_free_bytes?: number; disk_total_bytes?: number;
  };
  type Disk = { path: string; size_bytes: number; model: string; vendor: string; fs: string[]; label: string | null; status: string; is_aeon: boolean; part: string };

  let status: Status | null = null;
  let err = '';
  let poll: ReturnType<typeof setInterval>;

  // ── Storage allocation ──
  let storageGB = 10;
  let storageDirty = false; // true once the user moves the slider; blocks re-sync
  let applyBusy = false;

  function allocCapGB(s: Status | null): number {
    if (s && s.disk_free_bytes != null && s.disk_total_bytes) {
      return Math.max(1, Math.floor((s.repo_bytes + s.disk_free_bytes) / 1e9));
    }
    return 200;
  }
  $: maxAllocGB = allocCapGB(status);
  $: appliedGB = (() => {
    const m = (status?.storage_max ?? '').match(/(\d+)/);
    return m ? parseInt(m[1]) : storageGB;
  })();

  // ── External drives ──
  let disks: Disk[] = [];
  let adopted: { adopted: boolean; mount: string; free_bytes: number; total_bytes: number } | null = null;
  let diskBusy = '';
  let prepArm: Record<string, string> = {};
  let showFmt: Record<string, boolean> = {};

  // ── NAS ──
  let nas: { installed: boolean; running: string; host: string; user: string; shares: string[] } | null = null;
  let nasPw = '';
  let nasBusy = false;
  let nasMsg = '';

  const ipfsApi = (p: string, o: RequestInit = {}) => fetch(`/api/ipfs${p}`, { credentials: 'same-origin', ...o }).then((r) => r.json());
  const disksApi = (p: string, o: RequestInit = {}) => fetch(`/api/disks${p}`, { credentials: 'same-origin', ...o }).then((r) => r.json());

  async function load() {
    try {
      status = await ipfsApi('/status');
      const m = (status?.storage_max ?? '10GB').match(/(\d+)/);
      // Only sync the slider from the node when the user ISN'T mid-edit.
      if (m && !storageDirty) storageGB = Math.min(parseInt(m[1]), allocCapGB(status));
      await loadDisks();
      await loadNas();
    } catch (e: any) {
      err = e?.message ?? 'failed to load';
    }
  }

  async function applyStorage() {
    applyBusy = true;
    try {
      await ipfsApi('/storage', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ size: `${storageGB}GB` }) });
      storageDirty = false; // saved — let polling drive it again
    } catch { /* err surfaced on next load */ }
    applyBusy = false; await load();
  }

  async function loadDisks() {
    try {
      const [l, s] = await Promise.all([disksApi(''), disksApi('/status')]);
      disks = l?.disks ?? [];
      adopted = s?.ok ? s : null;
    } catch { /* admin-only */ }
  }
  async function prepareDisk(dev: string) {
    if (prepArm[dev] !== 'FORMAT') return;
    diskBusy = dev;
    try {
      const r = await disksApi('/prepare', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ dev, confirm: 'FORMAT' }) });
      if (!r?.ok) err = r?.err ?? 'prepare failed';
      prepArm[dev] = ''; showFmt[dev] = false;
    } catch (e: any) { err = e?.message ?? 'prepare failed'; }
    diskBusy = ''; await loadDisks();
  }
  async function useDisk(dev: string) {
    diskBusy = dev;
    try {
      const r = await disksApi('/use', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ dev }) });
      if (!r?.ok) err = r?.err ?? 'adopt failed';
    } catch (e: any) { err = e?.message ?? 'adopt failed'; }
    diskBusy = ''; await load();
  }
  async function releaseDisk() {
    diskBusy = 'release';
    try {
      const r = await disksApi('/release', { method: 'POST' });
      if (!r?.ok) err = r?.err ?? 'release failed';
    } catch (e: any) { err = e?.message ?? 'release failed'; }
    diskBusy = ''; await load();
  }

  async function loadNas() {
    try { nas = await fetch('/api/nas/status', { credentials: 'same-origin' }).then((r) => r.json()); } catch { /* admin-only */ }
  }
  async function enableNas() {
    if (nasPw.length < 4) { nasMsg = 'password too short (min 4)'; return; }
    nasBusy = true; nasMsg = 'setting up Samba… (first run installs it, ~30s)';
    try {
      const r = await fetch('/api/nas/enable', { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ password: nasPw }) }).then((r) => r.json());
      nasMsg = r?.ok ? '✓ file sharing enabled' : 'error: ' + (r?.err ?? 'enable failed');
      if (r?.ok) nasPw = '';
    } catch (e: any) { nasMsg = 'error: ' + (e?.message ?? 'enable failed'); }
    nasBusy = false; await loadNas();
  }
  async function disableNas() {
    nasBusy = true; nasMsg = '';
    try {
      const r = await fetch('/api/nas/disable', { method: 'POST', credentials: 'same-origin' }).then((r) => r.json());
      nasMsg = r?.ok ? 'file sharing off' : 'error: ' + (r?.err ?? 'disable failed');
    } catch (e: any) { nasMsg = 'error: ' + (e?.message ?? 'disable failed'); }
    nasBusy = false; await loadNas();
  }

  function fmtBytes(n: number): string {
    if (!n) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB', 'TB']; let i = 0, x = n;
    while (x >= 1024 && i < u.length - 1) { x /= 1024; i++; }
    return `${x.toFixed(1)} ${u[i]}`;
  }

  onMount(() => { load(); poll = setInterval(load, 5000); });
  onDestroy(() => clearInterval(poll));
</script>

{#if status?.daemon === 'active'}
<div class="space-y-3">
  {#if err}<p class="text-xs text-red-400 font-mono">{err}</p>{/if}

  {#if !spotlight}
    <!-- Storage shared with the network (full / IPFS page) -->
    <div class="panel p-4 space-y-3">
      <div class="flex items-center justify-between">
        <h2 class="font-mono text-cursed-300 text-sm">Storage shared with the network</h2>
        <span class="text-ink-300 text-sm font-mono">{storageGB} GB</span>
      </div>
      <p class="text-[11px] text-ink-400 leading-relaxed">
        How much of this Orb's disk to lend the IPFS network — room for the models you host
        <em>plus</em> content you cache and re-serve for others. Your own shared models stay pinned and
        are never evicted; a bigger allocation means you help mirror more of the network.
      </p>
      <input type="range" min="1" max={maxAllocGB} step="1" bind:value={storageGB}
             on:input={() => (storageDirty = true)} class="w-full accent-cursed-500" />
      <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
        <div class="h-full bg-cursed-500 transition-all duration-150"
             style="width:{Math.min(100, (status.repo_bytes / (storageGB * 1e9)) * 100)}%"></div>
      </div>
      <div class="flex items-center justify-between text-[11px] text-ink-500 font-mono">
        <span>{fmtBytes(status.repo_bytes)} used</span>
        <span>{(((status.disk_free_bytes ?? 0) / 1e9)).toFixed(1)} GB free on disk</span>
      </div>
      <div class="flex items-center gap-2">
        <button class="btn-primary btn-sm" on:click={applyStorage} disabled={applyBusy || storageGB === appliedGB}>Apply</button>
        {#if storageGB !== appliedGB}<span class="text-[11px] text-amber-300/80 font-mono">unsaved — was {appliedGB} GB</span>{/if}
      </div>
    </div>
  {/if}

  <div class={spotlight ? 'grid grid-cols-1 md:grid-cols-2 gap-3' : 'space-y-4'}>

    <!-- External USB/SSD storage -->
    <div class="relative {spotlight ? (adopted?.adopted ? 'panel-live' : 'panel-cursed') : 'panel'} p-4 space-y-3 flex flex-col">
      {#if spotlight}<div class="status-strip {adopted?.adopted ? 'status-strip-live' : 'status-strip-cursed'} absolute inset-x-0 top-0"></div>{/if}
      <div class="flex items-start justify-between gap-3">
        <div class="flex items-start gap-3 min-w-0">
          {#if spotlight}
            <div class="w-9 h-9 rounded-sm border {adopted?.adopted ? 'border-live-500/40 bg-live-500/10 text-live-400' : 'border-cursed-500/40 bg-cursed-900/40 text-cursed-300'} flex items-center justify-center shrink-0">
              <Icon name="drive" class="w-5 h-5" />
            </div>
          {/if}
          <div class="min-w-0">
            <h2 class="font-mono {spotlight ? 'inscription-sm text-zinc-100' : 'text-cursed-300 text-sm'}">
              {spotlight ? 'Storage drive' : 'External storage'}
            </h2>
            {#if spotlight}
              <p class="text-xs text-ink-400 mt-0.5 leading-snug">USB SSD or stick — host a bigger model library off the SD card.</p>
            {/if}
          </div>
        </div>
        <button class="btn btn-xs shrink-0" on:click={loadDisks} title="Rescan USB / SATA disks">rescan</button>
      </div>

      {#if adopted?.adopted}
        <div class="flex items-center justify-between gap-2 rounded-sm border border-live-500/30 bg-live-500/10 px-3 py-2">
          <div class="text-xs font-mono text-live-400">
            ● Using external drive
            <span class="text-ink-400">· {(adopted.free_bytes / 1e9).toFixed(0)} GB free of {(adopted.total_bytes / 1e9).toFixed(0)} GB</span>
          </div>
          <button class="text-[11px] font-mono text-ink-400 hover:text-red-400 shrink-0" on:click={releaseDisk} disabled={!!diskBusy}>revert to SD</button>
        </div>
      {/if}

      {#if !disks.length}
        {#if spotlight && !adopted?.adopted}
          <div class="flex-1 rounded-sm border border-dashed border-cursed-500/35 bg-ink-950/40 px-3 py-4 text-center space-y-2">
            <p class="text-sm text-ink-200">No drive attached</p>
            <p class="text-[11px] text-ink-500 leading-snug">Plug a USB SSD or stick into the Orb, then rescan.</p>
            <button class="btn-primary btn-sm" on:click={loadDisks} disabled={!!diskBusy}>
              <Icon name="drive" class="w-3.5 h-3.5" /> Rescan for a drive
            </button>
          </div>
        {:else}
          <p class="text-xs text-ink-500">No external drives detected. Plug a USB SSD/stick into the Orb, then <button class="text-cursed-300 hover:underline" on:click={loadDisks}>rescan</button>.</p>
        {/if}
      {:else}
        <div class="space-y-2">
          {#each disks as d (d.path)}
            <div class="rounded-sm border border-steel-700 bg-ink-950/40 p-2.5 space-y-2">
              <div class="flex items-center justify-between gap-2">
                <div class="min-w-0">
                  <div class="text-sm font-mono text-ink-100 truncate">{d.model || d.vendor || d.path} <span class="text-ink-500">· {(d.size_bytes / 1e9).toFixed(0)} GB</span></div>
                  <div class="text-[11px] font-mono text-ink-500">{d.path}{d.fs.length ? ' · ' + d.fs.join('/') : ' · unformatted'}{d.label ? ' · ' + d.label : ''}</div>
                </div>
                <span class="shrink-0 text-[10px] font-mono px-2 py-0.5 rounded uppercase tracking-wide
                  {d.status === 'in_use' ? 'bg-emerald-900/50 text-emerald-300'
                   : d.status === 'available' ? 'bg-cursed-900/40 text-cursed-300'
                   : 'bg-amber-900/40 text-amber-300'}">
                  {d.status === 'in_use' ? 'in use' : d.status === 'available' ? 'available' : 'needs prep'}
                </span>
              </div>

              {#if d.status === 'in_use'}
                <div class="text-[11px] text-emerald-400/80 font-mono">Backing the Orb's IPFS store + model library.</div>
              {:else}
                {#if d.status === 'available'}
                  <button class="btn-primary w-full btn-sm" on:click={() => useDisk(d.is_aeon ? d.path : d.part)} disabled={!!diskBusy}>
                    {diskBusy === d.path ? 'adopting…' : 'Use for model storage'}
                  </button>
                  <button class="w-full text-center text-[11px] font-mono text-ink-500 hover:text-amber-300"
                          on:click={() => (showFmt[d.path] = !showFmt[d.path])}>
                    {showFmt[d.path] ? '× cancel reformat' : '⚙ or erase + reformat fresh (ext4)'}
                  </button>
                {/if}
                {#if d.status === 'needs_prepare' || showFmt[d.path]}
                  <div class="rounded-sm border border-amber-500/30 bg-amber-900/10 p-2 space-y-1.5">
                    <p class="text-[11px] text-amber-300/90 leading-snug">This <b>erases everything</b> on the drive and lays down a fresh <b>GPT + ext4</b> filesystem labelled <span class="font-mono text-amber-200">AEON-DATA</span> — the optimal layout for the Orb's store. Type <span class="font-mono text-amber-200">FORMAT</span> to confirm.</p>
                    <div class="flex gap-2">
                      <input class="flex-1 min-w-0 bg-ink-800 border border-steel-600 rounded-sm px-2 py-1 text-ink-100 text-xs font-mono tracking-widest"
                             placeholder="FORMAT" bind:value={prepArm[d.path]} />
                      <button class="btn-danger btn-sm shrink-0" on:click={() => prepareDisk(d.path)} disabled={prepArm[d.path] !== 'FORMAT' || !!diskBusy}>
                        {diskBusy === d.path ? 'formatting…' : 'Prepare Device'}
                      </button>
                    </div>
                  </div>
                {/if}
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <!-- LAN NAS (Samba) — always open in spotlight, with the enable form visible -->
    <div class="relative {spotlight ? (nas?.running === 'active' ? 'panel-live' : 'panel-cursed') : 'panel'} p-4 space-y-3 flex flex-col">
      {#if spotlight}<div class="status-strip {nas?.running === 'active' ? 'status-strip-live' : 'status-strip-cursed'} absolute inset-x-0 top-0"></div>{/if}
      <div class="flex items-start justify-between gap-3">
        <div class="flex items-start gap-3 min-w-0">
          {#if spotlight}
            <div class="w-9 h-9 rounded-sm border {nas?.running === 'active' ? 'border-live-500/40 bg-live-500/10 text-live-400' : 'border-cursed-500/40 bg-cursed-900/40 text-cursed-300'} flex items-center justify-center shrink-0">
              <Icon name="nas" class="w-5 h-5" />
            </div>
          {/if}
          <div class="min-w-0">
            <h2 class="font-mono {spotlight ? 'inscription-sm text-zinc-100' : 'text-cursed-300 text-sm'}">
              {spotlight ? 'LAN file sharing' : 'LAN file sharing (NAS)'}
            </h2>
            {#if spotlight}
              <p class="text-xs text-ink-400 mt-0.5 leading-snug">Open the library from Finder, Explorer, or Files on this network.</p>
            {/if}
          </div>
        </div>
        <span class="pill shrink-0 {nas?.running === 'active' ? 'pill-live' : 'pill-idle'}">{nas?.running === 'active' ? 'on' : 'off'}</span>
      </div>

      {#if !spotlight}
        <p class="text-[11px] text-ink-400 leading-relaxed">
          Share the model library + an <code class="text-ink-500">Aeon Share</code> folder over your LAN, read/write,
          protected by the <code class="text-ink-500">{nas?.user ?? 'admin'}</code> account. Reach it from Finder
          (<code class="text-ink-500">smb://{nas?.host || '<orb-ip>'}</code>), Windows
          (<code class="text-ink-500">\\{nas?.host || '<orb-ip>'}\aeon-share</code>) or Linux.
        </p>
      {/if}

      {#if nas?.running === 'active'}
        <div class="rounded-sm border border-live-500/30 bg-live-500/10 px-3 py-2 space-y-1">
          <div class="text-xs font-mono text-live-400 break-all">smb://{nas.host}/aeon-share</div>
          <div class="text-xs font-mono text-live-400/80 break-all">smb://{nas.host}/models</div>
          {#if spotlight}
            <div class="text-[11px] text-ink-500 font-mono">Windows · \\{nas.host}\aeon-share · user {nas.user}</div>
          {/if}
        </div>
        <button class="btn btn-sm self-start" on:click={disableNas} disabled={nasBusy}>Turn off LAN share</button>
      {:else}
        {#if spotlight}
          <p class="text-[11px] text-ink-500 leading-snug">
            Read/write as <code class="text-ink-400">{nas?.user ?? 'admin'}</code>.
            Finder <code class="text-ink-400">smb://{nas?.host || 'orb'}/aeon-share</code>
            · Windows <code class="text-ink-400">\\{nas?.host || 'orb'}\aeon-share</code>
          </p>
        {/if}
        <div class="flex gap-2 mt-auto">
          <input type="password" autocomplete="new-password" placeholder="set a share password"
                 bind:value={nasPw}
                 class="flex-1 min-w-0 bg-ink-800 border border-steel-600 rounded-sm px-2 py-1.5 text-ink-100 text-sm" />
          <button class="btn-primary btn-sm shrink-0" on:click={enableNas} disabled={nasBusy || nasPw.length < 4}>
            {nasBusy ? '…' : 'Enable LAN share'}
          </button>
        </div>
      {/if}
      {#if nasMsg}
        <div class="text-xs font-mono {nasMsg.startsWith('error') ? 'text-red-400' : nasMsg.startsWith('✓') ? 'text-emerald-400' : 'text-amber-300'}">{nasMsg}</div>
      {/if}
    </div>
  </div>

  {#if spotlight}
    <!-- Allocation stays visible but secondary to the two tiles. -->
    <div class="panel p-3 space-y-2">
      <div class="flex items-center justify-between gap-3">
        <h2 class="font-mono text-xs text-cursed-300">IPFS allocation</h2>
        <span class="text-ink-300 text-xs font-mono">{storageGB} GB · {fmtBytes(status.repo_bytes)} used</span>
      </div>
      <input type="range" min="1" max={maxAllocGB} step="1" bind:value={storageGB}
             on:input={() => (storageDirty = true)} class="w-full accent-cursed-500" />
      <div class="flex items-center justify-between gap-2">
        <span class="text-[11px] text-ink-500 font-mono">{(((status.disk_free_bytes ?? 0) / 1e9)).toFixed(1)} GB free on disk</span>
        <div class="flex items-center gap-2">
          {#if storageGB !== appliedGB}<span class="text-[11px] text-amber-300/80 font-mono">unsaved — was {appliedGB} GB</span>{/if}
          <button class="btn-primary btn-xs" on:click={applyStorage} disabled={applyBusy || storageGB === appliedGB}>Apply</button>
        </div>
      </div>
    </div>
  {/if}
</div>
{/if}
