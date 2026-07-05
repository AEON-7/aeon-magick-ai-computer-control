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

  import PageHeader from '$lib/components/PageHeader.svelte';
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import VpnLogo from '$lib/components/VpnLogo.svelte';

  const PROVIDER_IDS = ['mullvad', 'ivpn', 'airvpn'];

  // AirVPN connection modes. The two "stealth" modes are the headline
  // feature: they disguise the VPN so a network/ISP can't tell it's a VPN.
  // The `note` text is shown verbatim under each toggle.
  const AIRVPN_MODES = [
    { id: 'wireguard',   label: 'WireGuard',          badge: 'recommended', badgeClass: 'bg-live-500/15 text-live-300',
      note: 'Fastest, most modern. Best for everyday use. AirVPN shares one key network-wide, so you can hop servers instantly.' },
    { id: 'openvpn',     label: 'OpenVPN',            badge: 'compatible',  badgeClass: 'bg-zinc-500/15 text-zinc-300',
      note: 'Classic OpenVPN. More compatible with locked-down networks; on TCP port 443 it already loosely resembles HTTPS.' },
    { id: 'openvpn_ssl', label: 'OpenVPN over SSL',   badge: 'stealth',     badgeClass: 'bg-cursed-500/20 text-cursed-200',
      note: 'Stealth — wraps the tunnel in TLS so on the wire it looks like ordinary HTTPS web-browsing traffic. Defeats most VPN-blocking and deep-packet inspection.' },
    { id: 'openvpn_ssh', label: 'OpenVPN over SSH',   badge: 'stealth',     badgeClass: 'bg-cursed-500/20 text-cursed-200',
      note: 'Stealth — carries the tunnel inside an SSH connection so it looks like a normal remote-management shell session.' },
  ];

  // "Get <provider>" sign-up links. AirVPN uses our referral link (supports
  // the project at no cost to you); Mullvad + IVPN have no referral program.
  const SIGNUP_URLS: Record<string, string> = {
    airvpn: 'https://airvpn.org/?referred_by=832389',
    mullvad: 'https://mullvad.net/',
    ivpn: 'https://www.ivpn.net',
  };

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
    airvpnApiKey = '';
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
  // AirVPN setup inputs (it uses an API key + a pasted config, not a
  // single credential like the account-based providers).
  let airvpnMode = 'wireguard';
  let airvpnApiKey = '';
  // v77: AirVPN is "ready" once the API key is saved (server list fetched);
  // from there the user picks a server + mode and we auto-generate. Other
  // providers gate on full `configured`.
  $: airvpnReady = active === 'airvpn' ? !!state?.has_api_key : !!state?.configured;
  $: airvpnGenerated = (state?.generated ?? {}) as Record<string, any>;
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
    // Build the per-provider setup body. AirVPN is the odd one out: it
    // doesn't mint credentials. Setup just stores the API key + fetches the
    // server list — the actual configs are auto-pulled per mode from AirVPN's
    // generator (generateConfig below), so there's no manual paste anywhere.
    let body: any;
    if (active === 'airvpn') {
      if (!airvpnApiKey.trim()) {
        error = 'paste your AirVPN API key first (member area → Client Area → API)';
        return;
      }
      body = { api_key: airvpnApiKey.trim() };
    } else {
      if (!credentialInput.trim()) {
        error = 'paste your account number / token first';
        return;
      }
      body = {
        credential: credentialInput.trim(),
        device_name: deviceName.trim() || 'aeon-magick',
      };
    }
    busy = true;
    error = '';
    msg = '';
    try {
      const r = await fetch(`/api/network/vpn/providers/${active}/setup`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      }).then(r => r.json());
      if (r.ok) {
        const ip = r.peer_ipv4 ? `, peer IP ${r.peer_ipv4}` : '';
        msg = active === 'airvpn'
          ? `✓ API key saved — ${r.server_count} servers. Now pick a server + mode below and generate.`
          : `✓ setup complete — ${r.server_count} servers in catalog${ip}`;
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

  // v77: auto-pull a mode's config package from AirVPN's generator for the
  // selected server. Each mode is generated + stored independently, so you can
  // set up WireGuard + OpenVPN + SSL + SSH and switch freely on /network.
  async function generateConfig(mode: string) {
    if (!state?.selected_server) { error = 'pick a server first'; return; }
    busy = true;
    error = '';
    msg = `generating ${mode} config for ${state.selected_server}…`;
    try {
      const r = await fetch('/api/network/vpn/providers/airvpn/generate', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mode, server_id: state.selected_server }),
      }).then(r => r.json());
      if (r.ok) {
        airvpnMode = mode;
        msg = `✓ ${mode} generated for ${r.server} (${r.files?.length ?? 0} files) — select AirVPN on /network to apply`;
        await refresh();
      } else {
        error = `generate failed: ${r.err}`;
        msg = '';
      }
    } catch (e: any) {
      error = e?.message ?? 'generate failed';
      msg = '';
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

  async function pickFastest(noEyes = false) {
    busy = true;
    error = '';
    msg = noEyes ? 'probing No-Eyes servers (may take ~10s)…' : 'probing servers (may take ~10s)…';
    try {
      const url = `/api/network/vpn/providers/${active}/pick-fastest${noEyes ? '?eyes=none' : ''}`;
      const r = await fetch(url, { method: 'POST' }).then(r => r.json());
      if (r.ok) {
        ranking = r.ranking;
        const reachable = ranking.filter((x: any) => x.rtt_ms !== null);
        if (noEyes && ranking.length === 0) {
          msg = '✗ this provider has no servers outside the 14-Eyes alliances';
        } else {
          msg = `✓ probed ${ranking.length}${noEyes ? ' No-Eyes' : ''} servers, ${reachable.length} reachable`;
        }
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
  <PageHeader title="VPN provider setup" backHref="/network" backLabel="NETWORK" />

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
          <div class="flex items-center gap-2 flex-wrap">
            <a class="inline-flex items-center gap-1.5 text-xs font-mono px-3 py-1.5 rounded
                      border border-cursed-500/50 text-cursed-200
                      hover:bg-cursed-500/10 transition-colors"
               href={SIGNUP_URLS[active] ?? meta.website} target="_blank" rel="noreferrer">
              Get {meta.label} →
            </a>
            <span class="text-[10px] text-zinc-600">
              No account yet? Sign up, then run the setup below.
              {#if active === 'airvpn'}
                <span class="text-cursed-300/70">(referral link — supports this project)</span>
              {:else}
                <span class="text-zinc-700">(direct link — not a referral)</span>
              {/if}
            </span>
          </div>
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

        {#if !airvpnReady || forceResetup}
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
                  {#if active === 'airvpn'}
                    Paste your AirVPN API key (member area → Client Area → API). We
                    fetch the server list — then you pick a server + mode below and the
                    Pi <strong>auto-generates</strong> the config straight from AirVPN
                    (WireGuard / OpenVPN / SSL / SSH). No manual download or paste.
                  {:else if state?.configured}
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
              {#if active === 'airvpn'}
                <!-- AirVPN: just the API key. Server pick + per-mode auto-generate
                     happen in the picker view once the key is saved. -->
                <p class="text-[11px] uppercase tracking-wider text-zinc-400">AirVPN API key</p>
                <input type="password" bind:value={airvpnApiKey}
                       placeholder={state?.has_api_key ? '•••••••• (stored — paste again to change)' : 'AirVPN API key (64-char)'}
                       class="w-full bg-ink-800 border border-ink-700 rounded
                              px-3 py-2 text-sm text-zinc-200 font-mono" />
                <p class="text-[10px] text-zinc-600 leading-relaxed">
                  Get it at
                  <a class="underline hover:text-cursed-200" href="https://airvpn.org/apisettings/"
                     target="_blank" rel="noopener">airvpn.org → API settings</a>.
                  After saving, pick a server + connection mode and we auto-generate the config.
                </p>
              {:else}
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
              {/if}
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
                  {#if active === 'airvpn' && state.mode && state.mode !== 'wireguard'}
                    mode {state.mode} · {servers.length} servers cached
                  {:else}
                    {#if state.peer_ipv4}peer IP {state.peer_ipv4} · {/if}{servers.length} servers cached
                  {/if}
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
                      on:click={() => pickFastest(false)}>
                {busy ? 'probing…' : 'pick fastest now'}
              </button>
              <button class="btn text-xs" disabled={busy}
                      on:click={() => pickFastest(true)}
                      title="Probe + rank only servers in countries OUTSIDE the 5/9/14-Eyes intelligence-sharing alliances">
                {busy ? 'probing…' : '🛡 fastest · No Eyes'}
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
              {#if active === 'airvpn'}
                Pick a server above, then generate a connection mode below. Each mode
                is stored independently — switch any time on
                <a href="/network" class="text-cursed-300 hover:underline">/network</a>.
              {:else}
                After picking a server, head to <a href="/network" class="text-cursed-300 hover:underline">/network</a>,
                select <code class="text-cursed-300">{active}</code> as the VPN provider, and save.
                The supervisor renders the WireGuard config from the chosen server and
                brings up wg-quick@aeon0.
              {/if}
            </p>
          </section>

          {#if active === 'airvpn'}
            <!-- v77: per-mode auto-generate. Each mode is pulled from AirVPN's
                 generator + stored independently, so all four can be set up. -->
            <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
              <header class="space-y-1">
                <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">Connection mode</h2>
                <p class="text-xs text-zinc-500">
                  Generate the config for any mode — it's auto-pulled from AirVPN for your
                  selected server and stored on the Pi. The <strong>active</strong> mode
                  is what applies when you select AirVPN on /network.
                </p>
              </header>
              {#if !state.selected_server}
                <p class="text-[11px] text-amber-300/80">⤴ Pick a server above first.</p>
              {/if}
              <div class="space-y-2">
                {#each AIRVPN_MODES as m}
                  {@const g = airvpnGenerated[m.id]}
                  <div class="p-3 rounded border {state.mode === m.id
                                ? 'border-cursed-500 bg-cursed-500/10'
                                : 'border-ink-700 bg-ink-800'}">
                    <div class="flex items-center gap-2 flex-wrap">
                      <span class="text-sm text-zinc-200 font-mono">{m.label}</span>
                      <span class="text-[9px] px-1.5 py-0.5 rounded uppercase tracking-wide {m.badgeClass}">{m.badge}</span>
                      {#if g}<span class="text-[10px] text-live-300" title={g.server}>✓ generated · {g.server_label || g.server}</span>{/if}
                      {#if state.mode === m.id && g}<span class="text-[10px] text-cursed-300 font-mono">active</span>{/if}
                      <button class="btn text-xs ml-auto" disabled={busy || !state.selected_server}
                              on:click={() => generateConfig(m.id)}>
                        {busy ? '…' : (g ? '↻ re-generate' : '⚡︎ generate')}
                      </button>
                    </div>
                    <p class="text-[11px] text-zinc-500 leading-snug mt-1">{m.note}</p>
                  </div>
                {/each}
              </div>
            </section>
          {/if}
        {/if}
      {/if}
    </div>
  </main>
</div>
