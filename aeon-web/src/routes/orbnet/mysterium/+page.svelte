<script lang="ts">
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { onMount, onDestroy } from 'svelte';

  const REFERRAL = 'https://mystnodes.co/?referral_code=jatlDOBziPRa3fD4mCfqWeltRHubQcTv40HwjGKb';

  type Status = {
    ok: boolean; enabled: boolean; installed: boolean; daemon: string;
    version?: string; uptime?: string; identity?: string; registration?: string;
    mmn_linked?: boolean;
    earnings_myst?: string; earnings_total_myst?: string; balance_myst?: string;
    beneficiary?: string; country?: string; region?: string; city?: string; ip?: string;
    data_bytes_30d?: number; sessions_30d?: number; consumers_30d?: number;
    ui_port?: number; ui_password?: string;
  };

  let status: Status | null = null;
  let busy = '';
  let copied = '';
  let apiKey = '';
  let claimErr = '';
  let claiming = false;
  let registering = false;
  type Services = { vpn: boolean; scraping: boolean; data_transfer: boolean; public: boolean };
  let svc: Services | null = null;
  let svcDraft: Services | null = null;
  let svcBusy = '';
  let uiHost = '';
  let benDraft = '';
  let benBusy = false;
  let benErr = '';
  let benOk = false;
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

  async function register() {
    registering = true;
    try { await api('/register', { method: 'POST' }); } catch {}
    // on-chain confirmation takes ~1-2 min; the status poll flips it to Registered
    setTimeout(() => { registering = false; load(); }, 5000);
  }

  async function setBeneficiary() {
    benErr = ''; benOk = false; benBusy = true;
    try {
      const r = await api('/beneficiary', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ address: benDraft.trim() }) });
      if (r.ok) { benOk = true; benDraft = ''; } else { benErr = r.err || 'Update failed.'; }
    } catch { benErr = 'Update failed.'; }
    benBusy = false;
  }

  async function loadServices() {
    try {
      const r = await api('/services');
      if (r.ok) {
        svc = { vpn: !!r.vpn, scraping: !!r.scraping, data_transfer: !!r.data_transfer, public: !!r.public };
        svcDraft = { ...svc };
      }
    } catch {}
  }
  async function applyServices() {
    if (!svcDraft) return;
    svcBusy = 'Applying… (restarting node)';
    try {
      await api('/services', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(svcDraft) });
    } catch {}
    setTimeout(async () => { svc = null; await loadServices(); await load(); svcBusy = ''; }, 7000);
  }

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
  $: benValid = /^0x[0-9a-fA-F]{40}$/.test(benDraft.trim());
  $: svcChanged = !!(svc && svcDraft && (svc.vpn !== svcDraft.vpn || svc.scraping !== svcDraft.scraping || svc.data_transfer !== svcDraft.data_transfer || svc.public !== svcDraft.public));
  $: if (running && svc === null && !svcBusy) loadServices();

  onMount(() => { uiHost = window.location.hostname; load(); poll = setInterval(load, 8000); });
  onDestroy(() => clearInterval(poll));
</script>

<div class="page-void min-h-screen">
  <PageHeader title="Mysterium dVPN" subtitle="decentralized VPN node" backHref="/orbnet" backLabel="ORBNET" index="03.4" />

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
      >{running ? (registered ? 'earning' : mmn_linked ? 'running · linked' : 'running · unclaimed') : status?.enabled ? 'starting' : 'off'}</span>
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
        <div class="rounded-sm border border-amber-700/50 bg-amber-950/20 p-4 space-y-4">
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
                class="flex-1 bg-ink-950 border border-steel-700 rounded px-3 py-2 font-mono text-xs text-ink-100 focus:border-cursed-600 focus:outline-none"
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
        <div class="rounded-sm border border-emerald-700/50 bg-emerald-950/20 p-4 flex items-center justify-between gap-3">
          <div class="text-sm text-emerald-300">✓ Linked to your MystNodes account{registered ? ' · registered & earning' : ' · not registered yet'}</div>
          <div class="flex items-center gap-3 shrink-0">
            {#if !registered}<button class="text-xs text-cursed-300 hover:text-cursed-200" on:click={register} disabled={registering}>{registering ? 'Registering…' : 'Register'}</button>{/if}
            <button class="text-xs text-ink-400 hover:text-ink-200" on:click={unclaim}>Unlink</button>
          </div>
        </div>
      {/if}

      <div class="grid grid-cols-2 sm:grid-cols-3 gap-3">
        <div class="panel p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Earnings (unsettled)</div>
          <div class="font-mono text-cursed-300 text-lg">{status.earnings_myst ?? '0'} <span class="text-xs text-ink-400">MYST</span></div>
        </div>
        <div class="panel p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Settled balance</div>
          <div class="font-mono text-ink-200 text-lg">{status.balance_myst ?? '0'} <span class="text-xs text-ink-400">MYST</span></div>
        </div>
        <div class="panel p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Lifetime earned</div>
          <div class="font-mono text-ink-200 text-lg">{status.earnings_total_myst ?? '0'} <span class="text-xs text-ink-400">MYST</span></div>
        </div>
        <div class="panel p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Data served (30d)</div>
          <div class="font-mono text-ink-200">{fmtBytes(status.data_bytes_30d)}</div>
        </div>
        <div class="panel p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Sessions (30d)</div>
          <div class="font-mono text-ink-200">{status.sessions_30d ?? 0} · {status.consumers_30d ?? 0} users</div>
        </div>
        <div class="panel p-3">
          <div class="text-[10px] uppercase tracking-wider text-ink-500">Region</div>
          <div class="font-mono text-ink-200 text-sm">{status.city || status.region || status.country || '—'}{status.country ? ` (${status.country})` : ''}</div>
        </div>
      </div>

      <div class="panel p-4 space-y-3 text-sm">
        <div class="space-y-1">
          <span class="text-ink-400 text-xs uppercase tracking-wider">Node ID</span>
          <div class="flex items-center gap-2">
            <code class="text-cursed-300 text-xs break-all">{status.identity || '—'}</code>
            {#if status.identity}<button class="text-ink-400 hover:text-cursed-300 shrink-0 text-xs" on:click={() => copy(status.identity || '', 'nid')}>{copied === 'nid' ? '✓' : 'copy'}</button>{/if}
          </div>
        </div>
        <div class="space-y-2 pt-3 border-t border-ink-800">
          <span class="text-ink-400 text-xs uppercase tracking-wider">Beneficiary (payout wallet)</span>
          {#if noPayout}
            <div class="text-amber-300 text-xs">Not set — earnings accrue to the node's channel until you set this.</div>
          {:else}
            <div class="flex items-center gap-2">
              <code class="text-cursed-300 text-xs break-all">{status.beneficiary}</code>
              <button class="text-ink-400 hover:text-cursed-300 shrink-0 text-xs" on:click={() => copy(status.beneficiary || '', 'ben')}>{copied === 'ben' ? '✓' : 'copy'}</button>
            </div>
          {/if}
          <div class="flex gap-2">
            <input type="text" autocomplete="off" spellcheck="false" bind:value={benDraft} placeholder="0x… Polygon (MATIC) wallet" class="flex-1 bg-ink-950 border border-steel-700 rounded px-2 py-1.5 font-mono text-xs text-ink-100 focus:border-cursed-600 focus:outline-none" />
            <button class="px-3 py-1.5 rounded bg-cursed-700 text-ink-50 text-xs font-mono hover:bg-cursed-600 disabled:opacity-40" on:click={setBeneficiary} disabled={benBusy || !benValid}>{benBusy ? 'Saving…' : 'Update'}</button>
          </div>
          {#if benErr}<div class="text-xs text-red-400">{benErr}</div>{/if}
          {#if benOk}<div class="text-xs text-emerald-300">Queued ✓ — applies on the node's next settlement (it needs some earnings to cover the fee first).</div>{/if}
          <p class="text-[11px] text-ink-500 leading-relaxed">Must be a Polygon (MATIC / ERC-20-on-Polygon) wallet — an incompatible address loses funds. Changes apply when the node next settles earnings.</p>
        </div>
        {#if status.ui_port && uiHost}
          <div class="space-y-1 pt-3 border-t border-ink-800">
            <div class="flex items-center justify-between">
              <span class="text-ink-400 text-xs uppercase tracking-wider">Node UI (advanced)</span>
              <a href={`http://${uiHost}:${status.ui_port}`} target="_blank" rel="noreferrer" class="text-xs text-cursed-300 hover:text-cursed-200">open →</a>
            </div>
            <div class="text-xs text-ink-400">
              Settlements, withdrawals &amp; node internals. Reachable from your LAN / Tailscale only.
              Sign in as <code class="text-ink-300">myst</code>
              {#if status.ui_password}
                · <button class="text-cursed-300 hover:text-cursed-200" on:click={() => copy(status.ui_password || '', 'uipw')}>{copied === 'uipw' ? '✓ password copied' : 'copy password'}</button>
              {/if}
            </div>
          </div>
        {/if}
      </div>

      {#if svcDraft}
        <div class="panel p-4 space-y-3">
          <div class="flex items-center justify-between">
            <h2 class="font-mono text-cursed-300 text-sm">Traffic you share</h2>
            <a href="https://my.mystnodes.com/me" target="_blank" rel="noreferrer" class="text-xs text-ink-400 hover:text-cursed-300">manage on mystnodes.com →</a>
          </div>
          <label class="flex items-center justify-between gap-3 cursor-pointer">
            <span><span class="text-ink-200 text-sm">VPN</span><span class="block text-xs text-ink-500">Encrypted internet access for consumers</span></span>
            <input type="checkbox" bind:checked={svcDraft.vpn} class="accent-cursed-500 w-4 h-4 shrink-0" />
          </label>
          <label class="flex items-center justify-between gap-3 cursor-pointer">
            <span><span class="text-ink-200 text-sm">Data scraping</span><span class="block text-xs text-ink-500">Residential proxy for B2B data scraping</span></span>
            <input type="checkbox" bind:checked={svcDraft.scraping} class="accent-cursed-500 w-4 h-4 shrink-0" />
          </label>
          <label class="flex items-center justify-between gap-3 cursor-pointer">
            <span><span class="text-ink-200 text-sm">Data transfer</span><span class="block text-xs text-ink-500">Streaming & data transfer for B2B clients</span></span>
            <input type="checkbox" bind:checked={svcDraft.data_transfer} class="accent-cursed-500 w-4 h-4 shrink-0" />
          </label>
          <label class="flex items-center justify-between gap-3 cursor-pointer pt-2 border-t border-ink-800">
            <span><span class="text-amber-300 text-sm">Public</span><span class="block text-xs text-ink-500">Open to the whole network — not just vetted B2B clients. Higher exposure of your home IP.</span></span>
            <input type="checkbox" bind:checked={svcDraft.public} class="accent-amber-500 w-4 h-4 shrink-0" />
          </label>
          <div class="flex items-center justify-end gap-3 pt-1">
            {#if svcBusy}<span class="text-xs text-cursed-300">{svcBusy}</span>{/if}
            <button class="px-3 py-1.5 rounded bg-cursed-700 text-ink-50 text-xs font-mono hover:bg-cursed-600 disabled:opacity-40" on:click={applyServices} disabled={!svcChanged || !!svcBusy}>Apply</button>
          </div>
        </div>
      {/if}
    {/if}
  </main>
</div>
