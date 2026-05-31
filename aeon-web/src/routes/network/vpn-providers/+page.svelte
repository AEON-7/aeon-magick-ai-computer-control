<script lang="ts">
  // VPN-provider wizards for Mullvad / IVPN / AzireVPN (v59+).
  //
  // Same shape for all three:
  //   1. paste credential (account # / token / etc)
  //   2. supervisor talks to provider's REST API, registers a local
  //      WG pubkey, fetches the server list, returns
  //   3. user picks a server (manual or auto-fastest)
  //   4. save → /etc/wireguard/aeon0.conf gets regenerated and
  //      wg-quick@aeon0 (re)starts
  //
  // The legacy VPN provider radio on /network points users here.

  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import VpnLogo from '$lib/components/VpnLogo.svelte';

  const PROVIDER_IDS = ['mullvad', 'ivpn'];

  // v66: which provider tab is showing. Defaults to mullvad, but the
  // deep-link from /network ("open setup wizard →" on the IVPN/Azire
  // banner) passes ?provider=<id> — without honoring it, every link
  // landed on the Mullvad tab regardless of which provider you clicked.
  let active: string = 'mullvad';

  /// Select a provider tab + keep the URL query in sync so the choice
  /// survives a reload / bookmark and the back button does the right
  /// thing. Resets the transient per-tab UI state (probe ranking,
  /// banners) so stale output from the previous provider doesn't bleed
  /// across.
  function selectProvider(id: string, pushUrl = true) {
    if (!PROVIDER_IDS.includes(id)) return;
    active = id;
    ranking = [];
    error = '';
    msg = '';
    credentialInput = '';
    forceResetup = false;
    if (pushUrl && typeof window !== 'undefined') {
      const url = new URL(window.location.href);
      url.searchParams.set('provider', id);
      history.replaceState(history.state, '', url);
    }
  }
  let catalog: any[] = [];
  let states: Record<string, any> = {};
  let credentialInput = '';
  // v74: when a provider is already configured, this reveals the setup form
  // again so the user can change the account ID / re-submit (switched to a new
  // account, or re-running setup to repair a broken session).
  let forceResetup = false;
  let deviceName = 'aeon-magick';
  let busy = false;
  let msg = '';
  let error = '';
  let countryFilter = '';
  let ranking: any[] = [];
  let poll: ReturnType<typeof setInterval>;

  async function refresh() {
    try {
      const c = await fetch('/api/network/vpn/providers/catalog').then(r => r.json());
      catalog = c.providers ?? [];
      for (const id of PROVIDER_IDS) {
        const s = await fetch(`/api/network/vpn/providers/${id}/state`).then(r => r.json());
        states[id] = s;
      }
      states = { ...states };
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
    }
  }

  onMount(() => {
    // v66: honor ?provider=<id> deep-link from the /network banners.
    if (typeof window !== 'undefined') {
      const want = new URLSearchParams(window.location.search).get('provider');
      if (want && PROVIDER_IDS.includes(want)) {
        active = want; // set directly — no URL rewrite needed, it's already there
      }
    }
    refresh();
    poll = setInterval(refresh, 10000);
  });
  onDestroy(() => { if (poll) clearInterval(poll); });

  $: meta = catalog.find(p => p.id === active);
  $: state = states[active];
  $: servers = (state?.servers ?? []) as any[];
  $: filteredServers = servers.filter(s =>
    !countryFilter.trim()
    || s.country.toLowerCase().includes(countryFilter.toLowerCase())
    || s.country_name.toLowerCase().includes(countryFilter.toLowerCase())
    || s.city.toLowerCase().includes(countryFilter.toLowerCase())
  );

  async function runSetup() {
    if (!credentialInput.trim()) {
      error = 'paste your account number / token first';
      return;
    }
    busy = true;
    error = '';
    msg = '';
    try {
      const r = await fetch(`/api/network/vpn/providers/${active}/setup`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          credential: credentialInput.trim(),
          device_name: deviceName.trim() || 'aeon-magick',
        }),
      }).then(r => r.json());
      if (r.ok) {
        msg = `✓ setup complete — ${r.server_count} servers in catalog, peer IP ${r.peer_ipv4}`;
        credentialInput = '';
        forceResetup = false;
        await refresh();
      } else {
        error = `setup failed: ${r.err}`;
      }
    } catch (e: any) {
      error = e?.message ?? 'setup failed';
    } finally {
      busy = false;
    }
  }

  async function selectServer(id: string) {
    busy = true;
    try {
      const r = await fetch(`/api/network/vpn/providers/${active}/select`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ server_id: id, mode: 'manual' }),
      }).then(r => r.json());
      if (r.ok) {
        msg = `✓ selected ${id} — apply via the VPN section on /network`;
        await refresh();
      } else {
        error = `select failed: ${r.err}`;
      }
    } catch (e: any) {
      error = e?.message ?? 'select failed';
    } finally {
      busy = false;
    }
  }

  // v67.7: re-fetch the provider's server list without re-running the
  // whole setup (no device/session churn). Uses the stored credentials.
  async function refreshServers() {
    busy = true;
    error = '';
    msg = 'refreshing server list…';
    try {
      const r = await fetch(`/api/network/vpn/providers/${active}/refresh`, {
        method: 'POST',
      }).then(r => r.json());
      if (r.ok) {
        msg = `✓ refreshed — ${r.server_count} servers cached`;
        ranking = [];
        await refresh();
      } else {
        error = `refresh failed: ${r.err}`;
        msg = '';
      }
    } catch (e: any) {
      error = e?.message ?? 'refresh failed';
      msg = '';
    } finally {
      busy = false;
    }
  }

  async function pickFastest() {
    busy = true;
    error = '';
    msg = 'probing servers (may take ~10s)…';
    try {
      const r = await fetch(`/api/network/vpn/providers/${active}/pick-fastest`, {
        method: 'POST',
      }).then(r => r.json());
      if (r.ok) {
        ranking = r.ranking;
        const reachable = ranking.filter((x: any) => x.rtt_ms !== null);
        msg = `✓ probed ${ranking.length} servers, ${reachable.length} reachable`;
      } else {
        error = `probe failed: ${r.err}`;
      }
    } catch (e: any) {
      error = e?.message ?? 'probe failed';
    } finally {
      busy = false;
    }
  }

  function eyesClass(e: string) {
    if (e === 'none') return 'bg-live-500/15 text-live-300';
    if (e === 'fourteen') return 'bg-amber-500/15 text-amber-300';
    if (e === 'nine') return 'bg-orange-500/15 text-orange-300';
    if (e === 'five') return 'bg-red-500/15 text-red-300';
    return 'bg-zinc-500/15 text-zinc-400';
  }

  function credentialPlaceholder(id: string) {
    if (id === 'mullvad') return '16-digit Mullvad account number';
    if (id === 'ivpn') return 'IVPN account ID (ivpn-XXXX-XXXX-XXXX)';
    return 'AzireVPN API token (NOT your account ID)';
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/network" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← NETWORK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">
        VPN provider setup
      </span>
    </div>
  </header>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-3xl mx-auto w-full space-y-6">

      <!-- Provider tabs -->
      <div class="flex gap-2 border-b border-ink-700 pb-2">
        {#each PROVIDER_IDS as id}
          <button class="flex items-center gap-2 px-4 py-2 text-sm font-mono uppercase tracking-wider transition-colors
                         {active === id
                           ? 'bg-cursed-500/10 text-cursed-300 border-b-2 border-cursed-500'
                           : 'text-zinc-500 hover:text-zinc-300'}"
                  on:click={() => selectProvider(id)}>
            <VpnLogo provider={id} size={18} muted={active !== id} />
            {id}
            {#if states[id]?.configured}
              <span class="text-live-400 ml-1" title="configured">●</span>
            {/if}
          </button>
        {/each}
      </div>

      {#if error}<p class="text-red-400 text-sm">{error}</p>{/if}
      {#if msg}<p class="text-live-400 text-sm">{msg}</p>{/if}

      {#if meta}
        <!-- Provider info card -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-2">
          <header class="flex items-baseline justify-between gap-2">
            <h2 class="flex items-center gap-2 font-mono text-sm uppercase tracking-wider text-zinc-300">
              <VpnLogo provider={active} size={22} />
              {meta.label}
            </h2>
            <div class="flex items-center gap-2 text-[10px]">
              <span class="px-1.5 py-0.5 rounded {eyesClass(meta.headquarters_eyes)}">
                HQ {meta.headquarters_country}
                {meta.headquarters_eyes === 'none' ? '— outside Eyes' : `— ${meta.headquarters_eyes} Eyes`}
              </span>
              {#if meta.audited}
                <span class="px-1.5 py-0.5 rounded bg-live-500/15 text-live-300">
                  audited {meta.last_audit_year} ({meta.last_audit_firm})
                </span>
              {:else}
                <span class="px-1.5 py-0.5 rounded bg-amber-500/15 text-amber-300">
                  no 3rd-party audit
                </span>
              {/if}
              <span class="font-mono text-cursed-300">trust {meta.trust_score}/5</span>
            </div>
          </header>
          <p class="text-xs text-zinc-500 leading-relaxed">{meta.notes}</p>
          <div class="flex gap-3 text-[10px] text-zinc-600 pt-1 border-t border-ink-800">
            {#if meta.anonymous_signup}<span>✓ anonymous signup</span>{:else}<span>email required</span>{/if}
            {#if meta.accepts_cash}<span>✓ cash payments</span>{/if}
            {#if meta.accepts_crypto}<span>✓ crypto payments</span>{/if}
            <a class="ml-auto text-cursed-300 hover:underline"
               href={meta.website} target="_blank" rel="noreferrer">
              {meta.website}
            </a>
          </div>
        </section>

        {#if !state?.configured || forceResetup}
          <!-- Setup wizard. Also reachable when already configured via the
               "change account" button (forceResetup) — for switching to a new
               account or repairing a stuck session. -->
          <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
            <header class="flex items-start justify-between gap-3">
              <div class="space-y-1">
                <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
                  {state?.configured ? 'Change account / re-run setup' : 'Setup'}
                </h2>
                <p class="text-xs text-zinc-500">
                  {#if state?.configured}
                    Submitting an account ID (a new one, or the same) registers a
                    fresh WireGuard key + session and re-fetches the server list —
                    use this for a new {meta.label} account or to repair a stuck
                    session.
                  {:else}
                    Paste your credential below. The Pi generates a fresh
                    WireGuard keypair locally and registers the public key with
                    {meta.label}'s API — your account is never sent to anyone else.
                  {/if}
                </p>
              </div>
              {#if state?.configured}
                <button class="btn text-xs flex-shrink-0" on:click={() => (forceResetup = false)}>
                  cancel
                </button>
              {/if}
            </header>
            <div class="space-y-2">
              <input type="password" bind:value={credentialInput}
                     placeholder={credentialPlaceholder(active)}
                     class="w-full bg-ink-800 border border-ink-700 rounded
                            px-3 py-2 text-sm text-zinc-200 font-mono" />
              {#if active === 'azirevpn'}
                <p class="text-[11px] text-amber-300/80 leading-relaxed">
                  This is your <strong>API token</strong> — <em>not</em> your account ID.
                  While signed in to AzireVPN, create one at
                  <a class="underline hover:text-amber-200"
                     href="https://manager.azirevpn.com/account/token"
                     target="_blank" rel="noopener"
                     >manager.azirevpn.com/account/token</a>.
                </p>
              {/if}
              <input type="text" bind:value={deviceName}
                     placeholder="device name (default: aeon-magick)"
                     class="w-full bg-ink-800 border border-ink-700 rounded
                            px-3 py-2 text-sm text-zinc-200 font-mono" />
              <button class="btn-primary text-sm" disabled={busy}
                      on:click={runSetup}>
                {busy ? 'setting up…' : 'Validate + fetch servers'}
              </button>
            </div>
          </section>
        {:else}
          <!-- Configured + server picker -->
          <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
            <header class="flex items-baseline justify-between gap-2 flex-wrap">
              <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
                Pick a server
              </h2>
              <div class="flex items-center gap-3">
                <p class="text-[10px] text-zinc-500 font-mono">
                  peer IP {state.peer_ipv4} · {servers.length} servers cached
                </p>
                <button class="btn text-xs"
                        on:click={() => { forceResetup = true; error = ''; msg = ''; }}
                        title="Change the account ID or re-run setup — for a new {meta.label} account or to repair a stuck session">
                  ↻ change account
                </button>
              </div>
            </header>

            <div class="flex flex-wrap gap-2">
              <input type="text" bind:value={countryFilter}
                     placeholder="filter by country / city…"
                     class="flex-1 min-w-0 bg-ink-800 border border-ink-700 rounded
                            px-3 py-1.5 text-xs text-zinc-200 font-mono" />
              <button class="btn text-xs" disabled={busy}
                      on:click={refreshServers}
                      title="Re-pull the provider's latest server list (no new device/session)">
                ⟳ refresh server list
              </button>
              <button class="btn-primary text-xs" disabled={busy}
                      on:click={pickFastest}>
                {busy ? 'probing…' : 'pick fastest now'}
              </button>
            </div>

            {#if ranking.length > 0}
              <div class="space-y-1 p-3 rounded bg-cursed-500/5 border border-cursed-500/30">
                <p class="text-[11px] uppercase tracking-wider text-cursed-300">
                  Latency ranking
                </p>
                {#each ranking.slice(0, 8) as r}
                  <button class="w-full flex items-center gap-2 text-xs
                                 p-1.5 rounded hover:bg-ink-800/50
                                 {state.selected_server === r.id ? 'bg-cursed-500/10' : ''}"
                          on:click={() => selectServer(r.id)}>
                    <span class="text-zinc-300 font-mono truncate flex-1 text-left">{r.label}</span>
                    <span class="text-[10px] text-zinc-500">{r.country}</span>
                    <span class="text-[10px] font-mono
                                 {r.rtt_ms === null ? 'text-red-400' :
                                  r.rtt_ms < 50  ? 'text-live-300' :
                                  r.rtt_ms < 150 ? 'text-amber-300' :
                                  'text-orange-300'}">
                      {r.rtt_ms === null ? 'unreachable' : `${r.rtt_ms}ms`}
                    </span>
                    <span class="text-[10px] text-cursed-300 font-mono">★{r.server_score}</span>
                  </button>
                {/each}
              </div>
            {/if}

            <div class="max-h-96 overflow-y-auto space-y-1
                        border border-ink-800 rounded p-2 bg-ink-950/40">
              {#each filteredServers.slice(0, 100) as s}
                <button class="w-full flex items-center gap-2 text-xs
                               p-1.5 rounded hover:bg-ink-800/50
                               {state.selected_server === s.id ? 'bg-cursed-500/10 border border-cursed-500/40' : ''}"
                        on:click={() => selectServer(s.id)}>
                  <span class="text-zinc-300 font-mono truncate flex-1 text-left">{s.label}</span>
                  <span class="text-[10px] font-mono text-zinc-500">{s.country}</span>
                  <span class="text-[10px] px-1.5 py-0.5 rounded {eyesClass(s.eyes)}">
                    {s.eyes === 'none' ? 'no eyes' :
                     s.eyes === 'unknown' ? '?' : s.eyes + ' eyes'}
                  </span>
                  <span class="text-[10px] text-cursed-300 font-mono">★{s.server_score}/5</span>
                </button>
              {/each}
              {#if filteredServers.length > 100}
                <p class="text-[10px] text-zinc-600 italic text-center pt-2">
                  Showing first 100 of {filteredServers.length}. Refine the filter to see more.
                </p>
              {/if}
            </div>

            <p class="text-[11px] text-zinc-500 leading-relaxed pt-2 border-t border-ink-800">
              After picking a server, head to <a href="/network" class="text-cursed-300 hover:underline">/network</a>,
              select <code class="text-cursed-300">{active}</code> as the
              VPN provider, and save. The supervisor will render the
              WireGuard config from the chosen server and bring up
              wg-quick@aeon0.
            </p>
          </section>
        {/if}
      {/if}
    </div>
  </main>
</div>
