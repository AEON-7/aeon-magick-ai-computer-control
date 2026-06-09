<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  const REFERRAL = 'https://mystnodes.co/?referral_code=jatlDOBziPRa3fD4mCfqWeltRHubQcTv40HwjGKb';

  type Status = {
    ok: boolean; enabled: boolean; installed: boolean; daemon: string;
    version?: string; uptime?: string; identity?: string; registration?: string;
    earnings_myst?: string; earnings_total_myst?: string; balance_myst?: string;
    beneficiary?: string; country?: string; region?: string; city?: string; ip?: string;
    data_bytes_30d?: number; sessions_30d?: number; consumers_30d?: number;
  };

  let status: Status | null = null;
  let busy = '';
  let copied = '';
  let poll: ReturnType<typeof setInterval>;

  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/mysterium${path}`, { credentials: 'same-origin', ...opts }).then((r) => r.json());

  async function load() { try { status = await api('/status'); } catch {} }
  async function enable() { busy = 'Installing the node…'; try { await api('/enable', { method: 'POST' }); } catch {} busy = ''; await load(); }
  async function disable() { busy = 'Stopping…'; try { await api('/disable', { method: 'POST' }); } catch {} busy = ''; await load(); }

  async function copy(t: string, id: string) {
    try { await navigator.clipboard.writeText(t); copied = id; setTimeout(() => (copied = ''), 1200); } catch {}
  }

  function fmtBytes(n?: number): string {
    if (!n) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0, x = n;
    while (x >= 1024 && i < u.length - 1) { x /= 1024; i++; }
    return `${x.toFixed(1)} ${u[i]}`;
  }

  $: registered = status?.registration === 'Registered';
  $: running = status?.daemon === 'active';
  $: noPayout = !status?.beneficiary || /^0x0+$/.test(status.beneficiary);

  onMount(() => { load(); poll = setInterval(load, 8000); });
  onDestroy(() => clearInterval(poll));
</script>

<div class="min-h-screen bg-ink-950 text-ink-100">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <a href="/orbnet" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← ORBNET</a>
    <h1 class="font-mono text-lg text-cursed-300">🌐 Mysterium</h1>
    <div class="w-24"></div>
  </header>

  <main class="max-w-3xl mx-auto px-5 py-6 space-y-6">
    <p class="text-ink-300 text-sm leading-relaxed">
      Earn by sharing your bandwidth and helping decentralize internet access on the Mysterium network.
    </p>

    <div class="flex items-center gap-3 flex-wrap">
      <span
        class="font-mono text-[10px] uppercase tracking-wider px-2 py-1 rounded {running
          ? registered
            ? 'bg-emerald-900/50 text-emerald-300'
            : 'bg-amber-900/50 text-amber-300'
          : 'bg-ink-800 text-ink-400'}"
      >{running ? (registered ? 'earning' : 'running · unclaimed') : status?.enabled ? 'starting' : 'off'}</span>
      {#if running && status?.version}
        <span class="text-ink-400 text-xs">myst {status.version}{status.country ? ` · ${status.city || status.region || status.country}` : ''}</span>
      {/if}
      <div class="flex-1"></div>
      {#if busy}<span class="text-xs text-cursed-300">{busy}</span>{/if}
      {#if status?.enabled}
        <button class="text-xs text-ink-400 hover:text-ink-200" on:click={disable} disabled={!!busy}>Disable</button>
      {:else}
        <button class="text-xs text-cursed-300 hover:text-cursed-200" on:click={enable} disabled={!!busy}>Enable</button>
      {/if}
    </div>

    {#if !status?.enabled}
      <div class="text-ink-500 text-sm text-center py-6">Off. Enable to install + run a Mysterium node (first run downloads ~19 MB).</div>
    {:else if !running}
      <div class="text-ink-400 text-sm text-center py-6">{busy || 'Starting the node… (first run installs the myst package)'}</div>
    {:else}
      {#if !registered}
        <div class="rounded-lg border border-amber-700/50 bg-amber-950/20 p-4 space-y-3">
          <h2 class="font-mono text-amber-300 text-sm">Claim your node to start earning</h2>
          <ol class="text-sm text-ink-300 space-y-2 list-decimal list-inside">
            <li>Create your Mysterium account: <a href={REFERRAL} target="_blank" rel="noreferrer" class="text-cursed-300 underline hover:text-cursed-200">open mystnodes.co →</a></li>
            <li>Add + claim this node there, then connect your wallet to set your payout — <span class="text-ink-400">your keys stay in your wallet (non-custodial)</span>.</li>
          </ol>
          <div class="text-xs text-ink-400">
            This node's identity (it'll show up in your mystnodes.co dashboard once online):
            <div class="flex items-center gap-2 mt-1">
              <code class="text-cursed-300 break-all">{status.identity}</code>
              <button class="text-ink-400 hover:text-cursed-300 shrink-0" on:click={() => copy(status.identity || '', 'id')}>{copied === 'id' ? '✓' : 'copy'}</button>
            </div>
          </div>
        </div>
      {/if}

      <div class="grid grid-cols-2 sm:grid-cols-3 gap-3">
        <div class="rounded-lg border border-ink-700 bg-ink-900 p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Earnings (unsettled)</div>
          <div class="font-mono text-cursed-300 text-lg">{status.earnings_myst ?? '0'} <span class="text-xs text-ink-400">MYST</span></div>
        </div>
        <div class="rounded-lg border border-ink-700 bg-ink-900 p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Settled balance</div>
          <div class="font-mono text-ink-200 text-lg">{status.balance_myst ?? '0'} <span class="text-xs text-ink-400">MYST</span></div>
        </div>
        <div class="rounded-lg border border-ink-700 bg-ink-900 p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Lifetime earned</div>
          <div class="font-mono text-ink-200 text-lg">{status.earnings_total_myst ?? '0'} <span class="text-xs text-ink-400">MYST</span></div>
        </div>
        <div class="rounded-lg border border-ink-700 bg-ink-900 p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Data served (30d)</div>
          <div class="font-mono text-ink-200">{fmtBytes(status.data_bytes_30d)}</div>
        </div>
        <div class="rounded-lg border border-ink-700 bg-ink-900 p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Sessions (30d)</div>
          <div class="font-mono text-ink-200">{status.sessions_30d ?? 0} · {status.consumers_30d ?? 0} users</div>
        </div>
        <div class="rounded-lg border border-ink-700 bg-ink-900 p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Region</div>
          <div class="font-mono text-ink-200 text-sm">{status.city || status.region || status.country || '—'}{status.country ? ` (${status.country})` : ''}</div>
        </div>
      </div>

      <div class="rounded-lg border border-ink-700 bg-ink-900 p-4 space-y-2 text-sm">
        <div class="flex items-center justify-between">
          <span class="text-ink-400 text-xs uppercase tracking-wider">Payout address</span>
          <a href={REFERRAL} target="_blank" rel="noreferrer" class="text-xs text-cursed-300 hover:text-cursed-200">manage on mystnodes.co →</a>
        </div>
        {#if noPayout}
          <div class="text-amber-300 text-xs">Not set — connect your wallet on mystnodes.co to receive earnings.</div>
        {:else}
          <code class="text-cursed-300 text-xs break-all">{status.beneficiary}</code>
        {/if}
      </div>
    {/if}
  </main>
</div>
