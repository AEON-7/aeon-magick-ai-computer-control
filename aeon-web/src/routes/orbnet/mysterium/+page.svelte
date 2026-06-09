<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  const REFERRAL = 'https://mystnodes.co/?referral_code=jatlDOBziPRa3fD4mCfqWeltRHubQcTv40HwjGKb';

  type Status = {
    ok: boolean; enabled: boolean; installed: boolean; daemon: string;
    version?: string; uptime?: string; identity?: string; registration?: string;
    mmn_linked?: boolean;
    earnings_myst?: string; earnings_total_myst?: string; balance_myst?: string;
    beneficiary?: string; country?: string; region?: string; city?: string; ip?: string;
    data_bytes_30d?: number; sessions_30d?: number; consumers_30d?: number;
  };

  let status: Status | null = null;
  let busy = '';
  let copied = '';
  let apiKey = '';
  let claimErr = '';
  let claiming = false;
  let poll: ReturnType<typeof setInterval>;

  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/mysterium${path}`, { credentials: 'same-origin', ...opts }).then((r) => r.json());

  async function load() { try { status = await api('/status'); } catch {} }
  async function enable() { busy = 'Installing the node…'; try { await api('/enable', { method: 'POST' }); } catch {} busy = ''; await load(); }
  async function disable() { busy = 'Stopping…'; try { await api('/disable', { method: 'POST' }); } catch {} busy = ''; await load(); }

  async function claim() {
    claimErr = '';
    if (apiKey.trim().length < 40) { claimErr = 'Key must be at least 40 characters.'; return; }
    claiming = true;
    try {
      const r = await api('/claim', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ api_key: apiKey.trim() }),
      });
      if (r.ok) { apiKey = ''; await load(); } else { claimErr = r.err || 'Claim failed.'; }
    } catch { claimErr = 'Claim failed.'; }
    claiming = false;
  }

  async function unclaim() { try { await api('/unclaim', { method: 'POST' }); } catch {} await load(); }

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
  $: mmn_linked = status?.mmn_linked === true;
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
      {#if !mmn_linked}
        <div class="rounded-lg border border-amber-700/50 bg-amber-950/20 p-4 space-y-4">
          <h2 class="font-mono text-amber-300 text-sm">Claim this node to your MystNodes account</h2>

          <div class="text-sm text-ink-300">
            <span class="text-ink-400">New to Mysterium?</span>
            <a href={REFERRAL} target="_blank" rel="noreferrer" class="text-cursed-300 underline hover:text-cursed-200">Create an account →</a>
            then grab your API key below. <span class="text-ink-500">(Uses our referral.)</span>
          </div>

          <div class="space-y-2">
            <span class="text-sm text-ink-300 block">
              Already have an account? Paste your API key from
              <a href="https://my.mystnodes.com/me" target="_blank" rel="noreferrer" class="text-cursed-300 underline hover:text-cursed-200">my.mystnodes.com →</a>
            </span>
            <div class="flex gap-2">
              <input
                type="password"
                autocomplete="off"
                spellcheck="false"
                bind:value={apiKey}
                placeholder="MystNodes API key (40+ characters)"
                class="flex-1 bg-ink-950 border border-ink-700 rounded px-3 py-2 font-mono text-xs text-ink-100 focus:border-cursed-600 focus:outline-none"
              />
              <button
                class="px-3 py-2 rounded bg-cursed-700 text-ink-50 text-sm font-mono hover:bg-cursed-600 disabled:opacity-50"
                on:click={claim}
                disabled={claiming || apiKey.trim().length < 40}
              >{claiming ? 'Linking…' : 'Claim node'}</button>
            </div>
            {#if claimErr}<div class="text-xs text-red-400">{claimErr}</div>{/if}
          </div>

          <p class="text-[11px] text-ink-500 leading-relaxed">
            Your API key is sent to the node and stored in its config to keep the link — the Orb's supervisor passes it
            through without logging or persisting it. It's an account token, not a wallet key; your funds and payout stay
            on mystnodes.co. Unlink anytime.
          </p>

          <div class="text-xs text-ink-400">
            Node identity:
            <span class="inline-flex items-center gap-2 align-middle">
              <code class="text-cursed-300 break-all">{status.identity}</code>
              <button class="text-ink-400 hover:text-cursed-300 shrink-0" on:click={() => copy(status.identity || '', 'id')}>{copied === 'id' ? '✓' : 'copy'}</button>
            </span>
          </div>
        </div>
      {:else}
        <div class="rounded-lg border border-emerald-700/50 bg-emerald-950/20 p-4 flex items-center justify-between gap-3">
          <div class="text-sm text-emerald-300">✓ Linked to your MystNodes account{registered ? ' · registered & earning' : ' · finishing registration…'}</div>
          <button class="text-xs text-ink-400 hover:text-ink-200 shrink-0" on:click={unclaim}>Unlink</button>
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
