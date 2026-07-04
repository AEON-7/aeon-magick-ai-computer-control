<script lang="ts">
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { onMount, onDestroy } from 'svelte';
  import qrcode from 'qrcode-generator';

  type Status = {
    ok: boolean; enabled: boolean; installed: boolean; daemon: string;
    version: string; peer_id: string; peers: number; repo_bytes: number;
    storage_max: string; gateway_port: number;
  };

  let status: Status | null = null;
  let pins: string[] = [];
  let err = '';
  let busy = '';
  let poll: ReturnType<typeof setInterval>;

  let storageGB = 10;
  let cid = '';
  let copied = '';

  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/ipfs${path}`, { credentials: 'same-origin', ...opts }).then((r) => r.json());

  async function load() {
    try {
      status = await api('/status');
      err = '';
      const m = (status?.storage_max ?? '10GB').match(/(\d+)/);
      if (m && document.activeElement?.tagName !== 'INPUT') storageGB = parseInt(m[1]);
      if (status?.enabled && status?.daemon === 'active') {
        const p = await api('/pins');
        pins = p?.pins ?? [];
      }
    } catch (e: any) {
      err = e?.message ?? 'failed to load';
    }
  }

  async function enable() {
    busy = 'Installing kubo + starting…';
    try { await api('/enable', { method: 'POST' }); } catch {}
    busy = ''; await load();
  }
  async function disable() {
    busy = 'Stopping…';
    try { await api('/disable', { method: 'POST' }); } catch {}
    busy = ''; await load();
  }

  async function applyStorage() {
    busy = 'Applying…';
    try {
      await api('/storage', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ size: `${storageGB}GB` }),
      });
    } catch {}
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
        class="font-mono text-[10px] uppercase tracking-wider px-2 py-1 rounded {status?.daemon === 'active'
          ? 'bg-emerald-900/50 text-emerald-300'
          : status?.enabled
            ? 'bg-amber-900/50 text-amber-300'
            : 'bg-ink-800 text-ink-400'}"
      >{status?.daemon === 'active' ? 'running' : status?.enabled ? 'starting' : 'off'}</span>
      {#if status?.daemon === 'active'}
        <span class="text-ink-400 text-xs"
          >{status.peers} peers · {fmtBytes(status.repo_bytes)} / {status.storage_max} · kubo {status.version}</span>
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

      <div class="rounded-lg border border-ink-700 bg-ink-900 p-4 space-y-2">
        <div class="flex items-center justify-between">
          <h2 class="font-mono text-cursed-300 text-sm">Storage allocation</h2>
          <span class="text-ink-300 text-sm font-mono">{storageGB} GB</span>
        </div>
        <input type="range" min="1" max="200" bind:value={storageGB} class="w-full accent-cursed-500" />
        <button class="font-mono text-xs px-3 py-1 rounded bg-cursed-700 hover:bg-cursed-600 text-white disabled:opacity-50" on:click={applyStorage} disabled={!!busy}>Apply</button>
      </div>

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
    {:else if status?.enabled}
      <div class="text-ink-400 text-sm text-center py-6">{busy || 'Starting IPFS… (first run downloads kubo, ~30 MB)'}</div>
    {:else}
      <div class="text-ink-500 text-sm text-center py-6">IPFS is off. Enable it to stand up your node + gateway.</div>
    {/if}
  </main>
</div>
