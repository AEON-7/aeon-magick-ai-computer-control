<script lang="ts">
  // USB ethernet passthrough + DNSCrypt + VPN.
  //
  // The "simple" surface up top covers what 90% of users want: turn USB
  // ethernet on/off, pick an isolation mode. The "Advanced" expander
  // hides DNSCrypt and VPN controls so the main page stays uncluttered.

  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import TipJar from '$lib/components/TipJar.svelte';
  import RulesEditor from '$lib/components/RulesEditor.svelte';

  // ── USB ethernet ──
  let usbState: api.UsbNetState | null = null;
  let usbEnabled = false;
  let usbMode: api.UsbNetMode = 'isolation';
  let usbSaving = false;
  let usbMsg = '';

  // ── DNSCrypt ──
  let dnsState: api.DnscryptState | null = null;
  let dnsEnabled = false;
  let dnsProvider = 'quad9';
  let dnsLocation = 'auto';
  let dnsCustomStamp = '';
  let dnsCustomLabel = '';
  let dnsSaving = false;
  let dnsMsg = '';

  // ── Server selection (v55+) ──
  // v56: defaults to "auto" + all-criteria-on + trust>=4. dnscrypt-
  // proxy's lb_strategy=p2 routes per-query to the lowest-latency
  // server matching criteria. When anonymized relay routing is on,
  // the latency probe goes through the relay → so picks reflect the
  // real end-to-end round trip, not just the resolver hop.
  let srvMode: 'specific' | 'auto' = 'auto';
  let srvCritNoLogs = true;
  let srvCritDnssec = true;
  let srvCritNoFilter = true;
  let srvCritOutside5 = true;
  let srvCritOutside14 = true;
  let srvCritMinTrust = 4;
  // Search + filter state for the resolver catalog (auto mode).
  let srvSearch = '';
  let srvShowOnlyMatching = false;

  // ── Anonymized DNSCrypt (v51+) ──
  // Off by default; opt-in adds 30-100ms latency per query but routes
  // through a relay so the resolver never sees the client IP.
  let anonEnabled = false;
  let anonMode: 'auto' | 'specific' = 'auto';
  let anonNoLogs = true;
  let anonOutsideFiveEyes = true;
  let anonOutsideFourteenEyes = false;
  let anonDnssec = true;
  let anonSpecificRelays: string[] = [];
  // Search box for the full ~190-relay catalog (specific mode only).
  let anonSearch = '';
  let anonShowOnlySelected = false;

  // ── Reactive derived state for resolver catalog (auto mode) ──
  // v62: svelte-check's flow analysis was narrowing srvMode away from
  // 'specific' once any prior `=== 'auto'` comparison fired, breaking
  // the new clickable-catalog mode. Lift the comparison into a
  // reactive boolean so the template uses a plain bool instead of a
  // narrowed union compare.
  $: srvModeIsSpecific = srvMode === 'specific';
  $: srvModeIsAuto = srvMode === 'auto';
  $: srvCatalog = dnsState?.servers?.catalog ?? [];
  // v55.1: the supervisor auto-applies a port-443-only filter when
  // Tor is the active VPN. We mirror that filter in the UI's
  // "matching" preview so the count + highlighting matches what
  // the backend actually picks.
  // v73: Tor is "active" when its overlay toggle is on — it's no longer a VPN
  // provider. Drives the DNSCrypt :443-only server filter (Tor exits block
  // other ports) below.
  $: srvTorActive = torEnabled;
  // Resolvers that pass the user's privacy/trust criteria. Mirrors
  // ResolverCriteria::passes() on the supervisor side.
  $: srvMatching = srvCatalog.filter((r) => {
    if (srvCritNoLogs && !r.no_logs) return false;
    if (srvCritDnssec && !r.dnssec) return false;
    if (srvCritNoFilter && !r.no_filter) return false;
    if (srvCritOutside5 && r.eyes === 'five') return false;
    if (srvCritOutside14 && (r.eyes === 'five' || r.eyes === 'nine' || r.eyes === 'fourteen')) return false;
    if (srvCritMinTrust > 0 && r.trust_score < srvCritMinTrust) return false;
    if (srvTorActive && r.port !== 443) return false;  // Tor exits only permit :443
    return true;
  });
  $: srvMatchingNames = new Set(srvMatching.map((r) => r.name));
  $: srvFiltered = srvCatalog.filter((r) => {
    if (srvShowOnlyMatching && !srvMatchingNames.has(r.name)) return false;
    if (!srvSearch.trim()) return true;
    const q = srvSearch.toLowerCase().trim();
    return r.name.toLowerCase().includes(q)
        || r.operator.toLowerCase().includes(q)
        || r.country.toLowerCase().includes(q)
        || r.label.toLowerCase().includes(q)
        || (r.description ?? '').toLowerCase().includes(q);
  });

  // Reactive derived state for the specific-mode multi-select.
  // Svelte 4 forbids non-assignment expressions in {@const} so we
  // compute these in a reactive block instead of inline in the template.
  $: anonCatalog = dnsState?.anonymized?.catalog ?? [];
  $: anonFiltered = anonCatalog.filter((r) => {
    if (anonShowOnlySelected && !anonSpecificRelays.includes(r.name)) return false;
    if (!anonSearch.trim()) return true;
    const q = anonSearch.toLowerCase().trim();
    return r.name.toLowerCase().includes(q)
        || r.operator.toLowerCase().includes(q)
        || r.country.toLowerCase().includes(q)
        || r.label.toLowerCase().includes(q)
        || (r.description ?? '').toLowerCase().includes(q);
  });
  $: anonGrouped = anonFiltered.reduce<Record<string, typeof anonFiltered>>(
    (acc, r) => {
      (acc[r.operator] ??= []).push(r);
      return acc;
    },
    {},
  );

  // ── VPN ──
  let vpnState: api.VpnState | null = null;
  let vpnStatus: api.VpnStatus | null = null;
  let vpnStatusPollTimer: ReturnType<typeof setInterval> | null = null;
  let rotating = false;
  let rotateMsg = '';
  let vpnEnabled = false;
  let vpnProvider: api.VpnProvider = 'none';
  let vpnKillSwitch = false;
  let vpnLanBypass = '192.168.0.0/16';
  // v80: Tailscale is an independent toggle now (mirrors tor/i2p) — its
  // enable flag + fields live at the top level, not under the VPN
  // provider. Split-tunnel mesh only; exit-node traffic rides the Pi's
  // normal egress (no toggle — see apply_tailscale_exit_node).
  let tsEnabled = false;
  let tsAuthKey = '';
  let tsHostname = '';
  let tsExitNode = false;
  let tsAdvertiseExit = false;
  let wgConfig = '';
  let ovConfig = '';
  let ovUser = '';
  let ovPass = '';
  let torBridges = '';
  let torPreset = 'direct';
  let i2pOutproxy = '';
  let vpnSaving = false;
  let vpnMsg = '';

  // v61: wizard-provider configuration status (mullvad/ivpn/azirevpn).
  // We poll the per-provider state endpoints so we know whether to
  // show "needs setup → wizard" vs "configured, ready to apply". The
  // user can still _select_ a wizard provider from the radio without
  // configuring it, but Save & Apply is gated until configuration is
  // present (otherwise wg-quick@aeon0 would start with an empty
  // config and the device would lose its tunnel).
  let wizardProviderConfigured: Record<string, boolean> = {
    mullvad: false,
    ivpn: false,
    azirevpn: false,
    airvpn: false,
  };
  let wizardProviderServer: Record<string, string> = {
    mullvad: '',
    ivpn: '',
    azirevpn: '',
    airvpn: '',
  };
  // v74: cached server list per provider (from the /state endpoint) so the
  // VPN section can offer an INLINE server picker + "pick fastest" once a
  // provider is configured — no trip to the wizard just to change server.
  let wizardProviderServers: Record<string, any[]> = {
    mullvad: [],
    ivpn: [],
    azirevpn: [],
    airvpn: [],
  };
  let serverBusy = ''; // provider id currently selecting/probing
  let fastestMsg = '';

  // v58.1: independent Tor / I2P toggles. They can run alongside any
  // clearnet VPN provider — when both are enabled, .onion goes through
  // Tor, .i2p through the I2P proxy, clearnet through the VPN (or
  // straight WAN if no VPN).
  let torEnabled = false;
  let torMode: 'split_tunnel' | 'transparent' = 'split_tunnel';
  let torOverVpn = false;
  let i2pEnabled = false;
  let i2pOverVpn = false;
  // ExitCountry pulled out so the new Tor block can read/write it
  // even when the legacy provider radio isn't on "tor".
  let torExitCountry = '';

  let loading = true;
  let error = '';
  let advancedOpen = false;

  // v62: each major section is now its own top-level <details>.
  // Default open state mirrors what the user typically looks at first.
  let dnsOpen = false;
  let overlaysOpen = false;
  let vpnOpen = false;

  // v62: status lights for the top-of-page service summary.
  //
  //   green  — service is enabled AND its runtime check passes (DNS:
  //            dnscrypt-proxy listening, Tor: bootstrap=100, I2P:
  //            active peers > 0, VPN: state=connected)
  //   amber  — service enabled but bootstrapping / reconnecting
  //   red    — service enabled but explicitly failed
  //   grey   — service not enabled
  //
  // The light's title attribute carries the one-liner so a hover gives
  // the same info the section header used to dump verbatim.
  type LightTone = 'live' | 'amber' | 'red' | 'grey';
  interface ServiceLight {
    key: 'dns' | 'tor' | 'i2p' | 'vpn';
    label: string;
    tone: LightTone;
    detail: string;
    onClick: () => void;
  }

  // v62: derive the per-overlay status array from VpnStatus. New
  // supervisor builds populate `overlays` directly; older builds
  // only fill the legacy flat fields, so we synthesize a single
  // overlay from those as a backward-compat shim. The render path
  // then iterates one list regardless of supervisor version.
  // (Order matters: serviceLights below reads statusOverlays directly
  //  so Svelte's reactivity tracker can see the dependency. The bug
  //  this fixes was the entire /network page going black: serviceLights
  //  used to call findOverlay() — a normal function — to read
  //  statusOverlays, which hid the dependency from Svelte's static
  //  analyzer, so serviceLights ran before statusOverlays was
  //  initialized and crashed on statusOverlays.find of undefined.)
  $: statusOverlays = (() => {
    if (!vpnStatus || !vpnStatus.enabled) return [] as api.VpnStatusOverlay[];
    if (Array.isArray(vpnStatus.overlays) && vpnStatus.overlays.length > 0) {
      return vpnStatus.overlays;
    }
    if (vpnStatus.provider === 'none') return [];
    // Legacy shape — synthesize a single overlay from flat fields so
    // the new rendering path covers both supervisor versions.
    const kind: api.VpnStatusOverlay['kind'] =
      vpnStatus.provider === 'tor'
        ? 'tor'
        : vpnStatus.provider === 'i2p'
        ? 'i2p'
        : 'vpn';
    return [{
      kind,
      provider: vpnStatus.provider,
      enabled: vpnStatus.enabled,
      state: vpnStatus.state,
      bootstrap_percent: vpnStatus.bootstrap_percent,
      summary: vpnStatus.summary,
      public_ip: vpnStatus.public_ip,
      public_country: vpnStatus.public_country,
      detail: vpnStatus.detail,
    }];
  })();

  // v73: the single live Tor overlay (bootstrap % + circuit relays), polled
  // every 4s. Drives the Tor section's status panel — the bootstrap progress
  // bar + relay-node listing that the v62 section restructure dropped even
  // though aeon-vpn-status.py still emits the data (GETINFO circuit-status).
  $: torOverlay = statusOverlays.find((o) => o.kind === 'tor');
  // v80: live Tailscale overlay — drives the online/IP/peer status box in
  // the new independent Tailscale section. aeon-vpn-status emits this
  // whenever tailscale.enabled is true, regardless of the active VPN.
  $: tsOverlay = statusOverlays.find((o) => o.kind === 'tailscale');

  // Labels for the overlay header — "Mullvad VPN" beats "wireguard"
  // when we know the user configured a wizard provider.
  function overlayLabel(o: api.VpnStatusOverlay | undefined, fallbackProvider = ''): string {
    if (!o) {
      switch (fallbackProvider) {
        case 'mullvad': return 'Mullvad VPN';
        case 'ivpn': return 'IVPN';
        case 'azirevpn': return 'AzireVPN';
        case 'airvpn': return 'AirVPN';
        case 'tailscale': return 'Tailscale';
        case 'wireguard': return 'WireGuard';
        case 'openvpn': return 'OpenVPN';
        default: return fallbackProvider || 'VPN';
      }
    }
    if (o.kind === 'tor') return 'Tor';
    if (o.kind === 'i2p') return 'I2P';
    switch (o.provider) {
      case 'mullvad': return 'Mullvad VPN';
      case 'ivpn': return 'IVPN';
      case 'azirevpn': return 'AzireVPN';
      case 'airvpn': return 'AirVPN';
      case 'tailscale': return 'Tailscale';
      case 'wireguard': return 'WireGuard';
      case 'openvpn': return 'OpenVPN';
      default: return o.provider;
    }
  }

  $: serviceLights = (() => {
    // Read statusOverlays once at the top so Svelte's static dependency
    // analyzer sees we depend on it. Without this, the previous findOverlay-
    // based lookup hid the reference inside a function call and the
    // reactive ran with statusOverlays still undefined → crash.
    const overlays = statusOverlays;
    const findOverlay = (k: 'tor' | 'i2p' | 'vpn') =>
      (overlays ?? []).find((o) => o.kind === k);
    const lights: ServiceLight[] = [];

    // DNS — dnscrypt-proxy's "are we encrypted" state.
    if (dnsEnabled) {
      const label =
        srvMode === 'auto'
          ? `Encrypted DNS · auto (${dnsState?.servers?.auto_picked?.length ?? 0} live)`
          : `Encrypted DNS · ${dnsProvider}`;
      lights.push({
        key: 'dns',
        label,
        tone: 'live',
        detail: anonEnabled
          ? 'DNSCrypt active with anonymized relay routing'
          : 'DNSCrypt active',
        onClick: () => (dnsOpen = true),
      });
    } else {
      lights.push({
        key: 'dns',
        label: 'DNS · plain',
        tone: 'grey',
        detail:
          'Encrypted DNS is off — queries leave the device in cleartext',
        onClick: () => (dnsOpen = true),
      });
    }

    // Tor — independent toggle, look it up in the overlay list.
    const torOv = findOverlay('tor');
    if (torEnabled) {
      const pct = torOv?.bootstrap_percent;
      const tone: LightTone =
        torOv?.state === 'connected'
          ? 'live'
          : torOv?.state === 'failed'
          ? 'red'
          : 'amber';
      lights.push({
        key: 'tor',
        label:
          torOv?.state === 'connected'
            ? `Tor · ${torMode === 'split_tunnel' ? '.onion only' : 'all traffic'}`
            : `Tor · ${pct !== null && pct !== undefined ? pct + '%' : torOv?.state ?? 'starting'}`,
        tone,
        detail: torOv?.summary ?? 'Tor enabled',
        onClick: () => (overlaysOpen = true),
      });
    } else {
      lights.push({
        key: 'tor',
        label: 'Tor · off',
        tone: 'grey',
        detail: 'Tor disabled',
        onClick: () => (overlaysOpen = true),
      });
    }

    // I2P — same shape as Tor.
    const i2pOv = findOverlay('i2p');
    if (i2pEnabled) {
      const tone: LightTone =
        i2pOv?.state === 'connected'
          ? 'live'
          : i2pOv?.state === 'failed'
          ? 'red'
          : 'amber';
      const peers = i2pOv?.detail?.active_peers;
      lights.push({
        key: 'i2p',
        label:
          peers !== undefined ? `I2P · ${peers} peers` : 'I2P · starting',
        tone,
        detail: i2pOv?.summary ?? 'I2P enabled',
        onClick: () => (overlaysOpen = true),
      });
    } else {
      lights.push({
        key: 'i2p',
        label: 'I2P · off',
        tone: 'grey',
        detail: 'I2P disabled',
        onClick: () => (overlaysOpen = true),
      });
    }

    // Clearnet VPN.
    const vpnOv = findOverlay('vpn');
    if (vpnEnabled && vpnProvider !== 'none') {
      const tone: LightTone =
        vpnOv?.state === 'connected'
          ? 'live'
          : vpnOv?.state === 'failed'
          ? 'red'
          : 'amber';
      const exitCountry = vpnOv?.public_country;
      lights.push({
        key: 'vpn',
        label: `${overlayLabel(vpnOv, vpnProvider)}${exitCountry ? ' · ' + exitCountry : ''}`,
        tone,
        detail: vpnOv?.summary ?? `${vpnProvider} enabled`,
        onClick: () => (vpnOpen = true),
      });
    } else {
      lights.push({
        key: 'vpn',
        label: 'VPN · off',
        tone: 'grey',
        detail: 'No clearnet VPN active',
        onClick: () => (vpnOpen = true),
      });
    }

    return lights;
  })();

  async function refresh() {
    loading = true;
    error = '';
    try {
      const [u, d, v] = await Promise.all([
        api.getUsbNet(),
        // This page renders the resolver/relay picker, so it needs the
        // full catalog — opt in explicitly. Fetched once here (and via
        // the manual "Refresh list" button), never on a timer.
        api.getDnscrypt({ catalog: true }),
        api.getVpn(),
      ]);
      usbState = u;
      usbEnabled = u.enabled;
      usbMode = u.mode;
      dnsState = d;
      dnsEnabled = d.enabled;
      dnsProvider = d.provider;
      dnsLocation = d.location;
      dnsCustomStamp = d.custom_stamp ?? '';
      dnsCustomLabel = d.custom_label ?? '';
      if (d.anonymized) {
        anonEnabled = d.anonymized.enabled;
        anonMode = d.anonymized.mode;
        anonNoLogs = d.anonymized.criteria.no_logs ?? true;
        anonOutsideFiveEyes = d.anonymized.criteria.outside_five_eyes ?? true;
        anonOutsideFourteenEyes = d.anonymized.criteria.outside_fourteen_eyes ?? false;
        anonDnssec = d.anonymized.criteria.dnssec ?? true;
        anonSpecificRelays = [...(d.anonymized.specific_relays ?? [])];
      }
      if (d.servers) {
        srvMode = d.servers.mode;
        // v56: defaults match supervisor-side ResolverCriteria::default()
        // — every privacy lever on, min_trust_score=4.
        const c = d.servers.auto_criteria ?? {};
        srvCritNoLogs = c.no_logs ?? true;
        srvCritDnssec = c.dnssec ?? true;
        srvCritNoFilter = c.no_filter ?? true;
        srvCritOutside5 = c.outside_five_eyes ?? true;
        srvCritOutside14 = c.outside_fourteen_eyes ?? true;
        srvCritMinTrust = c.min_trust_score ?? 4;
      }
      vpnState = v;
      vpnEnabled = v.enabled;
      // v73: Tor/I2P are configured in the Privacy Overlay section now, not as
      // VPN providers. Migrate any legacy vpnProvider=tor/i2p to 'none' so the
      // old VPN-section blocks never render and there's a single enable path.
      vpnProvider = (v.provider === 'tor' || v.provider === 'i2p') ? 'none' : v.provider;
      vpnKillSwitch = v.kill_switch;
      vpnLanBypass = v.lan_bypass;
      // v80: Tailscale state comes from the top-level [tailscale] block
      // now (independent of vpnProvider). Loaded here so the Tailscale
      // section renders its toggle + fields. has_auth_key drives the
      // "already saved" hint; the secret itself is never echoed.
      tsEnabled = v.tailscale.enabled ?? false;
      tsHostname = v.tailscale.hostname;
      tsExitNode = v.tailscale.exit_node;
      tsAdvertiseExit = v.tailscale.advertise_exit_node;
      ovUser = v.openvpn.auth_username;
      i2pOutproxy = v.i2p.outproxy;
      // Secrets are NOT echoed by the GET — start with empty inputs;
      // user types only what they want to change.
      tsAuthKey = '';
      wgConfig = '';
      ovConfig = '';
      ovPass = '';
      torBridges = '';
      torPreset = v.tor?.preset ?? 'direct';
      // v58.1: independent toggles. The API now surfaces these as
      // top-level fields on v.tor and v.i2p (see api.ts shapes).
      torEnabled = v.tor?.enabled ?? false;
      torMode = (v.tor?.mode as 'split_tunnel' | 'transparent') ?? 'split_tunnel';
      torOverVpn = v.tor?.over_vpn ?? false;
      torExitCountry = v.tor?.exit_country ?? '';
      i2pEnabled = v.i2p?.enabled ?? false;
      i2pOverVpn = v.i2p?.over_vpn ?? false;

      // v61: refresh wizard-provider state so the inline banner can
      // show "configured + which server" or "needs setup". These
      // endpoints exist regardless of which provider is currently
      // active — we hit all three so toggling the radio doesn't need
      // an extra round-trip. Cheap; the state files are tiny.
      await Promise.all(
        ['mullvad', 'ivpn', 'azirevpn', 'airvpn'].map(async (id) => {
          try {
            const r = await fetch(
              `/api/network/vpn/providers/${id}/state`,
            ).then((rr) => rr.json());
            wizardProviderConfigured[id] = !!r?.configured;
            wizardProviderServer[id] = r?.selected_server ?? '';
            wizardProviderServers[id] = Array.isArray(r?.servers) ? r.servers : [];
          } catch {
            wizardProviderConfigured[id] = false;
            wizardProviderServer[id] = '';
            wizardProviderServers[id] = [];
          }
        }),
      );
      wizardProviderConfigured = { ...wizardProviderConfigured };
      wizardProviderServer = { ...wizardProviderServer };
      wizardProviderServers = { ...wizardProviderServers };
    } catch (e: any) {
      error = e?.message ?? 'failed to load network state';
    } finally {
      loading = false;
    }
  }

  async function pollVpnStatus() {
    try {
      vpnStatus = await api.getVpnStatus();
    } catch (e) {
      // Silent on poll errors — surfacing a banner every 3s would be noisy
      console.warn('vpn status poll failed', e);
    }
  }

  // v74: inline server picker. Both actions persist the selection (POST
  // /select); the tunnel comes up on Save & Apply, consistent with the rest
  // of the VPN section. pick-fastest probes TCP RTT to each cached server and
  // selects the lowest.
  async function selectVpnServer(id: string, serverId: string) {
    serverBusy = id;
    try {
      await fetch(`/api/network/vpn/providers/${id}/select`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ server_id: serverId }),
      });
      wizardProviderServer[id] = serverId;
      wizardProviderServer = { ...wizardProviderServer };
    } catch (e: any) {
      fastestMsg = `✗ ${e?.message ?? 'select failed'}`;
    } finally {
      serverBusy = '';
    }
  }

  async function pickFastestVpnServer(id: string, noEyes = false) {
    serverBusy = id;
    fastestMsg = noEyes
      ? 'probing latency to No-Eyes servers…'
      : 'probing latency to each server…';
    try {
      const url = `/api/network/vpn/providers/${id}/pick-fastest${noEyes ? '?eyes=none' : ''}`;
      const r = await fetch(url, { method: 'POST' }).then((rr) => rr.json());
      const ranking = r?.ranking ?? [];
      const top = ranking.find((e: any) => e.rtt_ms != null);
      if (top?.id) {
        await selectVpnServer(id, top.id);
        fastestMsg = `fastest${noEyes ? ' No-Eyes' : ''}: ${top.label} (${top.rtt_ms} ms) — Save & Apply to connect`;
      } else if (noEyes && ranking.length === 0) {
        fastestMsg = '✗ this provider has no servers outside the 14-Eyes alliances';
      } else {
        fastestMsg = 'no servers responded to the probe — try "refresh" in the wizard';
      }
    } catch (e: any) {
      fastestMsg = `✗ ${e?.message ?? 'probe failed'}`;
    } finally {
      serverBusy = '';
      setTimeout(() => (fastestMsg = ''), 8000);
    }
  }

  async function rotateIdentity() {
    if (rotating) return;
    rotating = true;
    rotateMsg = 'rotating…';
    try {
      const result = await api.rotateVpnIdentity();
      rotateMsg = result.ok
        ? `✓ ${result.action}`
        : `✗ ${result.detail || 'rotate failed'}`;
      // Force a fresh status poll right after the rotation completes
      setTimeout(pollVpnStatus, 1500);
    } catch (e: any) {
      rotateMsg = `✗ ${e?.message ?? 'rotate failed'}`;
    } finally {
      rotating = false;
      setTimeout(() => (rotateMsg = ''), 4000);
    }
  }

  onMount(() => {
    refresh();
    // Poll status every 4s. Slower than once-a-second to keep the
    // public-IP curl from hammering ifconfig.co; fast enough to track
    // bootstrap progress smoothly.
    vpnStatusPollTimer = setInterval(pollVpnStatus, 4000);
    pollVpnStatus();
    // Auto-expand the Advanced (firewall rules) section if we arrived
    // via the /security page's "allow this traffic" deep-link. Without
    // this the RulesEditor doesn't mount, so its onMount can't read the
    // ?createRule=1 params.
    try {
      const sp = new URLSearchParams(window.location.search);
      if (sp.get('createRule') === '1') {
        advancedOpen = true;
      }
    } catch { /* ignore */ }
  });

  onDestroy(() => {
    if (vpnStatusPollTimer) clearInterval(vpnStatusPollTimer);
  });

  async function saveUsb() {
    if (!usbState) return;
    if (usbEnabled === usbState.enabled && usbMode === usbState.mode) {
      usbMsg = 'no changes';
      setTimeout(() => (usbMsg = ''), 2000);
      return;
    }
    if (
      usbEnabled !== usbState.enabled &&
      !confirm(
        `${usbEnabled ? 'Enable' : 'Disable'} USB ethernet?\n\n` +
        `This rebuilds the USB gadget composite — the connected host ` +
        `will see a brief USB disconnect/reconnect (~1 second).`,
      )
    ) {
      usbEnabled = usbState.enabled;
      return;
    }
    if (
      usbMode === 'restricted' &&
      usbState.mode !== 'restricted' &&
      !confirm(
        `Switch to RESTRICTED mode?\n\n` +
        `In restricted mode the connected host has WAN access only — ` +
        `it CANNOT reach this Pi's web UI, SSH, or any other service ` +
        `over the USB-C link. You will need WiFi or LAN access to ` +
        `manage this device from now on.`,
      )
    ) {
      usbMode = usbState.mode;
      return;
    }
    usbSaving = true;
    error = '';
    try {
      await api.setUsbNet({ enabled: usbEnabled, mode: usbMode });
      usbMsg = '✓ saved + applied';
      setTimeout(() => (usbMsg = ''), 3000);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'save failed';
    } finally {
      usbSaving = false;
    }
  }

  async function saveDns() {
    dnsSaving = true;
    error = '';
    try {
      const patch: Parameters<typeof api.setDnscrypt>[0] = {
        enabled: dnsEnabled,
        provider: dnsProvider,
        location: dnsLocation,
        servers: {
          mode: srvMode,
          auto_criteria: {
            no_logs: srvCritNoLogs,
            dnssec: srvCritDnssec,
            no_filter: srvCritNoFilter,
            outside_five_eyes: srvCritOutside5,
            outside_fourteen_eyes: srvCritOutside14,
            min_trust_score: srvCritMinTrust,
          },
        },
        anonymized: {
          enabled: anonEnabled,
          mode: anonMode,
          criteria: {
            no_logs: anonNoLogs,
            outside_five_eyes: anonOutsideFiveEyes,
            outside_fourteen_eyes: anonOutsideFourteenEyes,
            dnssec: anonDnssec,
          },
          specific_relays: anonSpecificRelays,
        },
      };
      if (dnsProvider === 'custom') {
        if (dnsCustomStamp) patch.custom_stamp = dnsCustomStamp.trim();
        if (dnsCustomLabel) patch.custom_label = dnsCustomLabel.trim();
      }
      await api.setDnscrypt(patch);
      dnsMsg = '✓ saved + applied';
      setTimeout(() => (dnsMsg = ''), 3000);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'save failed';
    } finally {
      dnsSaving = false;
    }
  }

  /// One-click enable from the Tor section's "DNSCrypt recommended"
  /// warning banner. Picks Cloudflare auto-region as a sensible default
  /// + commits immediately (no second "save" step needed). After this
  /// runs the warning auto-disappears because dnsEnabled flips true.
  async function enableDefaultDnscrypt() {
    dnsSaving = true;
    error = '';
    try {
      await api.setDnscrypt({
        enabled: true,
        // v50: Quad9 is the only safe one-click default — it's a true
        // DNSCrypt v2 resolver with no SNI to leak, audited zero-log
        // policy, and a malware filter. Cloudflare/NextDNS/Mullvad
        // were removed from the list because they only run DoH.
        provider: 'quad9',
        location: 'auto',
      });
      dnsMsg = '✓ DNSCrypt enabled — Quad9 via Tor';
      setTimeout(() => (dnsMsg = ''), 4000);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'enable failed';
    } finally {
      dnsSaving = false;
    }
  }

  // Map machine tags to human-readable lozenges. Keeps the radio rows
  // scannable rather than dumping "log_policy: anonymized" raw at the
  // user.
  function logBadge(policy: string): { text: string; classes: string } {
    switch (policy) {
      case 'no_logs':    return { text: 'no logs', classes: 'bg-live-500/20 text-live-300 border-live-500/40' };
      case 'anonymized': return { text: 'anonymized', classes: 'bg-cursed-500/15 text-cursed-300 border-cursed-500/40' };
      case 'self_logs':  return { text: 'your dashboard', classes: 'bg-amber-500/15 text-amber-300 border-amber-500/40' };
      default:           return { text: policy, classes: 'bg-ink-700 text-zinc-400 border-ink-600' };
    }
  }
  function secBadge(sec: string): { text: string; classes: string } {
    switch (sec) {
      case 'basic':      return { text: 'basic', classes: 'bg-ink-700 text-zinc-300 border-ink-600' };
      case 'filtered':   return { text: 'malware filter', classes: 'bg-cursed-500/15 text-cursed-300 border-cursed-500/40' };
      case 'family':     return { text: 'family filter', classes: 'bg-fuchsia-500/15 text-fuchsia-300 border-fuchsia-500/40' };
      case 'ad_block':   return { text: 'ads + trackers', classes: 'bg-live-500/15 text-live-300 border-live-500/40' };
      default:           return { text: sec, classes: 'bg-ink-700 text-zinc-400 border-ink-600' };
    }
  }

  async function saveVpn() {
    // v61: gate the apply on wizard-provider configuration. Mullvad/
    // IVPN/AzireVPN need a registered account + selected server in
    // their state file before wg-quick@aeon0 can bring up the tunnel.
    // Without that gate the user clicks Save & Apply, the supervisor
    // writes an empty WireGuard config, and wg-quick fails silently —
    // they see "off" in the status panel with no clear reason why.
    if (
      vpnEnabled &&
      api.WIZARD_PROVIDERS.has(vpnProvider) &&
      !wizardProviderConfigured[vpnProvider]
    ) {
      error =
        `${vpnProvider} isn't configured yet. Open the setup wizard at ` +
        `/network/vpn-providers and register your account first.`;
      return;
    }
    if (
      vpnKillSwitch &&
      !vpnState?.kill_switch &&
      !confirm(
        `Enable VPN kill-switch?\n\n` +
        `Non-VPN outbound traffic will be DROPPED whenever the tunnel is ` +
        `down or stalled. Loopback and the LAN bypass subnet ` +
        `(${vpnLanBypass || 'none'}) stay reachable so you can still ` +
        `manage this device from your LAN.\n\n` +
        `If you change network providers or the tunnel fails to come up, ` +
        `the device will appear offline from anything outside ${vpnLanBypass || 'the LAN'}.`,
      )
    ) {
      vpnKillSwitch = vpnState?.kill_switch ?? false;
      return;
    }
    vpnSaving = true;
    error = '';
    try {
      const patch: api.VpnPatch = {
        enabled: vpnEnabled,
        provider: vpnProvider,
        kill_switch: vpnKillSwitch,
        lan_bypass: vpnLanBypass,
      };
      if (vpnProvider === 'wireguard' && wgConfig) {
        patch.wireguard = { config: wgConfig };
      } else if (vpnProvider === 'openvpn') {
        patch.openvpn = { auth_username: ovUser };
        if (ovConfig) patch.openvpn.config = ovConfig;
        if (ovPass) patch.openvpn.auth_password = ovPass;
      }
      // v58.1/v73: Tor + I2P are configured in the Privacy Overlay section,
      // not as VPN providers. Their toggles — and Tor's bridge preset, custom
      // bridges, mode + exit country — are the single source of truth, saved
      // on every request regardless of which clearnet VPN provider is picked.
      patch.tor = {
        ...(patch.tor ?? {}),
        enabled: torEnabled,
        mode: torMode,
        over_vpn: torOverVpn,
        preset: torPreset,
        bridges: torBridges || undefined,
        exit_country: torExitCountry,
      };
      patch.i2p = {
        ...(patch.i2p ?? {}),
        enabled: i2pEnabled,
        outproxy: i2pOutproxy,
        over_vpn: i2pOverVpn,
      };
      // v80: Tailscale is an independent toggle now — its fields are
      // saved on EVERY request (not gated on vpnProvider), exactly like
      // the Tor + I2P overlays. The auth_key is only sent when the user
      // typed a new one (blank keeps the saved key — the backend treats
      // "" as an explicit clear).
      patch.tailscale = {
        enabled: tsEnabled,
        hostname: tsHostname,
        exit_node: tsExitNode,
        advertise_exit_node: tsAdvertiseExit,
      };
      if (tsAuthKey) patch.tailscale.auth_key = tsAuthKey;
      await api.setVpn(patch);
      vpnMsg = '✓ saved + applied (tunnel may take a few seconds)';
      setTimeout(() => (vpnMsg = ''), 4000);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'save failed';
    } finally {
      vpnSaving = false;
    }
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← AEON MAGICK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">network</span>
    </div>
  </header>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-3xl mx-auto w-full space-y-6">
    {#if loading}
      <p class="text-zinc-500 text-sm">loading…</p>
    {:else if error}
      <p class="text-red-400 text-sm">{error}</p>
    {/if}

    {#if usbState}
      <!-- ──────────────────────────────────────────────────────────── -->
      <!-- USB ethernet passthrough                                      -->
      <!-- ──────────────────────────────────────────────────────────── -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-6 space-y-5">
        <header class="space-y-1">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            USB ethernet passthrough
          </h2>
          <p class="text-zinc-400 text-sm">
            Present this device to the connected USB-C host as a USB
            ethernet adapter alongside the HID functions. The host gets a
            DHCP lease in the <code class="text-cursed-300">{usbState.subnet}</code> range
            and reaches this device at <code class="text-cursed-300">{usbState.pi_addr}</code>
            (unless restricted mode is on — see below).
          </p>
        </header>

        <!-- Enable toggle -->
        <label class="flex items-center gap-3 cursor-pointer">
          <input type="checkbox" bind:checked={usbEnabled}
                 class="w-4 h-4 accent-cursed-500" />
          <span class="text-zinc-200 text-sm">Enable USB ethernet</span>
        </label>

        <!-- Mode selector -->
        <div class="space-y-3 pl-7" class:opacity-40={!usbEnabled} class:pointer-events-none={!usbEnabled}>
          <p class="text-xs uppercase tracking-wider text-zinc-500">Network mode</p>

          <label class="flex items-start gap-3 cursor-pointer">
            <input type="radio" bind:group={usbMode} value="isolation"
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1">
              <div class="text-zinc-200 text-sm font-medium">
                Isolation
                <span class="text-xs text-cursed-400 ml-1">(recommended)</span>
              </div>
              <p class="text-xs text-zinc-500">
                Host can reach this device + internet (NAT'd through this
                device's upstream + any Tailscale routes). Host CANNOT see
                other devices on the LAN. Use for guest laptops, untrusted
                machines, or any time you want hard separation between the
                USB-connected host and your home network.
              </p>
            </div>
          </label>

          <label class="flex items-start gap-3 cursor-pointer">
            <input type="radio" bind:group={usbMode} value="restricted"
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1">
              <div class="text-zinc-200 text-sm font-medium">
                Restricted
                <span class="text-xs text-red-400 ml-1">(hardened)</span>
              </div>
              <p class="text-xs text-zinc-500">
                Host has WAN access only. CANNOT see this device at all —
                no web UI, no SSH, no ping, no port scan. Only DHCP and
                DNS pass through to the Pi (the bare minimum to get an
                address and resolve names). Use when the connected host
                should treat this device as an invisible network gateway
                with zero management surface.
                <span class="block mt-1 text-red-300/80">
                  You will need WiFi or LAN access to manage this device
                  when in restricted mode.
                </span>
              </p>
            </div>
          </label>

          <label class="flex items-start gap-3 cursor-pointer">
            <input type="radio" bind:group={usbMode} value="sharing"
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1">
              <div class="text-zinc-200 text-sm font-medium">Sharing</div>
              <p class="text-xs text-zinc-500">
                Host has full LAN access through this device. Use only on
                trusted networks. The host effectively gains all the
                routing privileges this Pi has.
              </p>
            </div>
          </label>
        </div>

        <!-- Save row -->
        <div class="flex items-center gap-3 pt-2 border-t border-ink-700">
          <button class="btn-primary" on:click={saveUsb} disabled={usbSaving}>
            {usbSaving ? 'applying…' : 'save & apply'}
          </button>
          {#if usbMsg}
            <span class="text-xs text-live-400 font-mono">{usbMsg}</span>
          {/if}
        </div>
      </section>

      <!-- Status -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-2 text-sm">
        <h3 class="font-mono text-xs uppercase tracking-wider text-zinc-500">Current</h3>
        <dl class="grid grid-cols-2 gap-x-4 gap-y-1 text-xs font-mono">
          <dt class="text-zinc-500">enabled</dt>
          <dd class="text-zinc-200">{usbState.enabled}</dd>
          <dt class="text-zinc-500">mode</dt>
          <dd class="text-zinc-200">{usbState.mode}</dd>
          <dt class="text-zinc-500">subnet</dt>
          <dd class="text-zinc-200">{usbState.subnet}</dd>
          <dt class="text-zinc-500">pi address</dt>
          <dd class="text-zinc-200">{usbState.pi_addr}</dd>
          <dt class="text-zinc-500">DHCP range</dt>
          <dd class="text-zinc-200">{usbState.dhcp_range}</dd>
          {#if dnsState}
            <dt class="text-zinc-500">DNSCrypt</dt>
            <dd class="text-zinc-200">
              {dnsState.enabled ? `${dnsState.provider} (${dnsState.location})` : 'off'}
            </dd>
          {/if}
          {#if vpnState}
            <dt class="text-zinc-500">VPN</dt>
            <dd class="text-zinc-200">
              {vpnState.enabled && vpnState.provider !== 'none' ? vpnState.provider : 'off'}
            </dd>
          {/if}
        </dl>
      </section>

      <!-- ──────────────────────────────────────────────────────────── -->
      <!-- v62: per-service status light row. One pill per overlay     -->
      <!--      with a colored dot. Click jumps to (+opens) the         -->
      <!--      relevant configuration section below.                   -->
      <!-- ──────────────────────────────────────────────────────────── -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-4 space-y-2">
        <h3 class="font-mono text-[10px] uppercase tracking-wider text-zinc-500">
          Privacy stack
        </h3>
        <div class="flex flex-wrap gap-2">
          {#each serviceLights as l (l.key)}
            <button type="button" on:click={l.onClick}
                    title={l.detail}
                    class="inline-flex items-center gap-2 px-3 py-1.5 rounded-full
                           border text-xs font-mono transition-colors
                           {l.tone === 'live' ? 'bg-live-500/10 border-live-500/40 text-live-200 hover:bg-live-500/20' :
                            l.tone === 'amber' ? 'bg-amber-500/10 border-amber-500/40 text-amber-200 hover:bg-amber-500/20' :
                            l.tone === 'red' ? 'bg-red-500/10 border-red-500/40 text-red-200 hover:bg-red-500/20' :
                            'bg-ink-950/60 border-ink-800 text-zinc-500 hover:border-ink-700 hover:text-zinc-300'}">
              <span class="h-2 w-2 rounded-full
                           {l.tone === 'live' ? 'bg-live-400 animate-pulse' :
                            l.tone === 'amber' ? 'bg-amber-400 animate-pulse' :
                            l.tone === 'red' ? 'bg-red-400' :
                            'bg-zinc-600'}"></span>
              {l.label}
            </button>
          {/each}
        </div>
      </section>

      <!-- ──────────────────────────────────────────────────────────── -->
      <!-- v62: each major service is now its own top-level collapsible -->
      <!--      section. Previously all three were nested inside a       -->
      <!--      single "Standard network settings" wrapper which made    -->
      <!--      the page a giant unscrollable wall.                      -->
      <!-- ──────────────────────────────────────────────────────────── -->

      <!-- ─── Encrypted DNS ─── -->
      <details bind:open={dnsOpen}
               class="bg-ink-900 border border-ink-700 rounded-xl">
        <summary class="cursor-pointer select-none px-6 py-4
                        flex items-center justify-between gap-3">
          <div class="flex items-center gap-3 min-w-0">
            <span class="h-2 w-2 rounded-full flex-shrink-0
                         {dnsEnabled ? 'bg-live-400 animate-pulse' : 'bg-zinc-600'}"></span>
            <span class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              Encrypted DNS
            </span>
            <span class="text-xs text-zinc-500 truncate">
              {dnsEnabled
                ? srvMode === 'auto'
                  ? `auto · ${dnsState?.servers?.auto_picked?.length ?? 0} live`
                  : `pinned · ${dnsProvider}`
                : 'plain DNS — cleartext on the wire'}
            </span>
          </div>
          <span class="text-[10px] font-mono uppercase tracking-wider text-zinc-500 flex-shrink-0">
            {dnsOpen ? '▾ close' : '▸ expand'}
          </span>
        </summary>
        <div class="border-t border-ink-700 p-6 space-y-8">

          <!-- ─── DNSCrypt body ─── -->
          <section class="space-y-4">
            <header class="space-y-1">
              <h3 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
                Encrypted DNS (DNSCrypt)
              </h3>
              <p class="text-zinc-400 text-sm">
                Route this device's DNS — and any DHCP client's DNS over
                USB ethernet — through a local <code class="text-cursed-300">dnscrypt-proxy</code>
                instance that talks <strong>true DNSCrypt v2</strong> to your chosen upstream.
                The Pi sees only encrypted DNS traffic; the local network sees
                no resolver hostname (DNSCrypt has no TLS layer, so there's no
                SNI to leak the way DoH would). Custom-slot users can paste a
                DoH/DoT stamp if they want — the protocol is then labelled
                honestly.
              </p>
            </header>

            <label class="flex items-center gap-3 cursor-pointer">
              <input type="checkbox" bind:checked={dnsEnabled}
                     class="w-4 h-4 accent-cursed-500" />
              <span class="text-zinc-200 text-sm">Enable DNSCrypt</span>
            </label>

            <!-- v54: when Tor + a port-8443 resolver are both selected,
                 warn about Tor exit policies. We DO set force_tcp on
                 the dnscrypt-proxy side automatically, so UDP queries
                 can't leak — but Tor exit nodes frequently block TCP
                 to non-standard ports like 8443, which manifests as
                 silent timeouts. Recommend AdGuard / OpenDNS / etc. -->
            {#if dnsEnabled && torEnabled
                 && (dnsProvider === 'quad9' || dnsProvider === 'quad9-unfiltered'
                     || dnsProvider === 'cleanbrowsing')}
              <div class="p-3 rounded border border-amber-500/40 bg-amber-500/10 ml-7
                          flex items-start gap-2">
                <span class="text-amber-400 text-sm leading-none">⚠</span>
                <div class="space-y-1">
                  <p class="text-xs text-amber-200">
                    <strong>{dnsProvider === 'cleanbrowsing' ? 'CleanBrowsing' : 'Quad9'}
                    DNSCrypt runs on port 8443</strong>, which most Tor exit nodes
                    refuse to forward. Queries will time out silently after a few
                    retries. We force every DNSCrypt query to TCP when Tor is on
                    (so nothing leaks via UDP), but the exit policy is outside our
                    control.
                  </p>
                  <p class="text-[11px] text-amber-100/70 leading-relaxed">
                    <strong>Works well over Tor:</strong> AdGuard (any flavour),
                    OpenDNS, and Anonymized DNSCrypt setups that exit on port 443.
                    Switch to one of those above if DNS isn't resolving.
                  </p>
                </div>
              </div>
            {/if}

            <div class="space-y-4 pl-7" class:opacity-40={!dnsEnabled} class:pointer-events-none={!dnsEnabled}>

              <!-- v55: Server-selection mode. Specific = pin to one
                   provider. Auto = use the full ~226-entry catalog
                   filtered by criteria, with dnscrypt-proxy's
                   lb_strategy=p2 routing per-query by live latency. -->
              <div class="space-y-2" role="radiogroup" aria-label="Server selection mode">
                <p class="text-xs uppercase tracking-wider text-zinc-500">
                  Server selection
                </p>
                <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
                  <label class="flex items-start gap-3 cursor-pointer
                                p-3 rounded-lg border transition-colors
                                {srvMode === 'specific'
                                  ? 'bg-cursed-500/10 border-cursed-500/50'
                                  : 'bg-ink-950/40 border-ink-800 hover:border-ink-700'}">
                    <input type="radio" bind:group={srvMode} value="specific"
                           class="mt-1 w-4 h-4 accent-cursed-500" />
                    <div class="space-y-1 flex-1 min-w-0">
                      <div class="text-zinc-200 text-sm font-medium">Pin a specific provider</div>
                      <p class="text-[11px] text-zinc-500">
                        Pick exactly one resolver from the curated list below.
                        Simple + predictable.
                      </p>
                    </div>
                  </label>
                  <label class="flex items-start gap-3 cursor-pointer
                                p-3 rounded-lg border transition-colors
                                {srvMode === 'auto'
                                  ? 'bg-cursed-500/10 border-cursed-500/50'
                                  : 'bg-ink-950/40 border-ink-800 hover:border-ink-700'}">
                    <input type="radio" bind:group={srvMode} value="auto"
                           class="mt-1 w-4 h-4 accent-cursed-500" />
                    <div class="space-y-1 flex-1 min-w-0">
                      <div class="text-zinc-200 text-sm font-medium">
                        Auto (criteria-based, lowest-latency live)
                      </div>
                      <p class="text-[11px] text-zinc-500">
                        Filter the full ~226-server catalog by your criteria;
                        dnscrypt-proxy probes each + routes per-query to the
                        fastest match.
                      </p>
                    </div>
                  </label>
                </div>
              </div>

              {#if srvMode === 'auto'}
                <!-- v55.1: Tor-active port-443 filter notice. -->
                {#if srvTorActive}
                  <div class="p-3 rounded border border-cursed-500/40 bg-cursed-500/10
                              flex items-start gap-2">
                    <span class="text-cursed-300 text-sm leading-none">🧅</span>
                    <div class="space-y-1">
                      <p class="text-xs text-cursed-200">
                        <strong>Tor is active</strong> — auto-pick is restricted
                        to <strong>port-443 resolvers only</strong>. Tor exit
                        nodes universally permit 443 (looks like HTTPS) but
                        commonly block alternates like 8443 / 5443 / 5353,
                        which would silently time out even with
                        <code class="text-cursed-300">force_tcp</code> on.
                      </p>
                      <p class="text-[11px] text-cursed-100/70 leading-relaxed">
                        199 of 226 catalog entries use port 443, so the
                        filter doesn't shrink your pool much. Excluded
                        outright: Quad9 (8443), AdGuard (5443), CleanBrowsing
                        family (5443), a handful of others.
                      </p>
                    </div>
                  </div>
                {/if}
                <!-- ── Criteria checkboxes ── -->
                <div class="space-y-2 pt-2">
                  <p class="text-xs uppercase tracking-wider text-zinc-500">
                    Privacy + trust criteria
                  </p>
                  <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
                    <label class="flex items-center gap-2 cursor-pointer text-xs
                                  p-2 rounded border border-ink-800 bg-ink-950/40
                                  hover:border-ink-700">
                      <input type="checkbox" bind:checked={srvCritNoLogs}
                             class="w-3 h-3 accent-cursed-500" />
                      <span class="text-zinc-300">No-logs policy</span>
                    </label>
                    <label class="flex items-center gap-2 cursor-pointer text-xs
                                  p-2 rounded border border-ink-800 bg-ink-950/40
                                  hover:border-ink-700">
                      <input type="checkbox" bind:checked={srvCritDnssec}
                             class="w-3 h-3 accent-cursed-500" />
                      <span class="text-zinc-300">DNSSEC validating</span>
                    </label>
                    <label class="flex items-center gap-2 cursor-pointer text-xs
                                  p-2 rounded border border-ink-800 bg-ink-950/40
                                  hover:border-ink-700">
                      <input type="checkbox" bind:checked={srvCritNoFilter}
                             class="w-3 h-3 accent-cursed-500" />
                      <span class="text-zinc-300">No filtering (raw answers)</span>
                    </label>
                    <label class="flex items-center gap-2 cursor-pointer text-xs
                                  p-2 rounded border border-ink-800 bg-ink-950/40
                                  hover:border-ink-700">
                      <input type="checkbox" bind:checked={srvCritOutside5}
                             class="w-3 h-3 accent-cursed-500" />
                      <span class="text-zinc-300">Outside Five Eyes</span>
                    </label>
                    <label class="flex items-center gap-2 cursor-pointer text-xs
                                  p-2 rounded border border-ink-800 bg-ink-950/40
                                  hover:border-ink-700">
                      <input type="checkbox" bind:checked={srvCritOutside14}
                             class="w-3 h-3 accent-cursed-500" />
                      <span class="text-zinc-300">Outside Fourteen Eyes</span>
                    </label>
                    <label class="flex items-center gap-2 text-xs
                                  p-2 rounded border border-ink-800 bg-ink-950/40">
                      <span class="text-zinc-300">Min trust:</span>
                      <select bind:value={srvCritMinTrust}
                              class="bg-ink-800 border border-ink-700 rounded
                                     px-2 py-0.5 text-xs text-zinc-200">
                        <option value={0}>any</option>
                        <option value={2}>2+ (small ops)</option>
                        <option value={3}>3+ (commercial)</option>
                        <option value={4}>4+ (audited)</option>
                        <option value={5}>5 (highest)</option>
                      </select>
                    </label>
                  </div>
                  <p class="text-[11px] text-zinc-500 leading-relaxed">
                    Privacy: <code class="text-zinc-400">no_logs + DNSSEC + no_filter + jurisdiction</code>.
                    Trust: <code class="text-zinc-400">operator reputation tier + audit + DNSCrypt-vs-DoH</code>.
                    Scores are derived from the stamp's operator-declared
                    properties plus a hand-curated operator table — they're
                    heuristics, not ground truth.
                  </p>
                </div>

                <!-- ── Currently auto-picked list ── -->
                {#if dnsState?.servers?.auto_picked?.length}
                  <div class="space-y-1 p-3 rounded bg-cursed-500/5
                              border border-cursed-500/30">
                    <div class="flex items-baseline justify-between gap-2">
                      <p class="text-[11px] uppercase tracking-wider text-cursed-300">
                        Currently picked servers
                      </p>
                      <p class="text-[10px] text-zinc-500 font-mono">
                        {dnsState.servers.auto_picked.length} matching · live-routed by latency
                      </p>
                    </div>
                    <p class="text-[10px] text-zinc-600 leading-relaxed pt-1">
                      dnscrypt-proxy probes all of these on startup, then routes
                      each query to the lowest-latency one. Switching automatically
                      if one slows down — no UI refresh needed.
                      {#if anonEnabled}
                        <strong class="text-cursed-300">When anonymized relay routing
                        is on (it is, below), the latency probe goes through the
                        relay path</strong> — so the picked server is the fastest
                        end-to-end choice, not just the resolver hop.
                      {/if}
                    </p>
                  </div>
                {/if}

                <!-- ── Full catalog browser ── -->
                <!-- v62: rows are now clickable in specific mode so the
                     user can pick any of the 226 upstream resolvers
                     directly. The old curated-provider radio (Quad9 /
                     custom / etc.) was removed — every resolver in that
                     list also lives in the catalog, so the radio was
                     redundant and made the section twice as tall. In
                     auto mode the click is disabled (the criteria do
                     the selection); in specific mode the picked row is
                     highlighted cursed-purple. -->
                <div class="space-y-2 pt-3 border-t border-ink-800">
                  <div class="flex flex-wrap items-center justify-between gap-2">
                    <p class="text-[11px] uppercase tracking-wider text-zinc-500">
                      {srvModeIsSpecific ? 'Pick a resolver' : 'Full upstream catalog'}
                      ({srvCatalog.length} servers)
                    </p>
                    <p class="text-[11px] font-mono text-cursed-300">
                      {#if srvModeIsSpecific && dnsProvider}
                        currently pinned: {dnsProvider}
                      {:else}
                        {srvMatching.length} match your criteria
                      {/if}
                    </p>
                  </div>
                  <div class="flex flex-wrap items-center gap-2">
                    <input type="text" bind:value={srvSearch}
                           placeholder="search: name, operator, country code…"
                           class="flex-1 min-w-0 bg-ink-800 border border-ink-700
                                  rounded px-3 py-1.5 text-xs text-zinc-200 font-mono" />
                    <label class="flex items-center gap-1.5 cursor-pointer text-[11px]
                                  text-zinc-400 hover:text-zinc-200">
                      <input type="checkbox" bind:checked={srvShowOnlyMatching}
                             class="w-3 h-3 accent-cursed-500" />
                      only matching
                    </label>
                  </div>

                  <div class="max-h-96 overflow-y-auto space-y-2
                              border border-ink-800 rounded p-3 bg-ink-950/40">
                    {#if srvFiltered.length === 0}
                      <p class="text-xs text-zinc-500 italic text-center py-4">
                        {srvSearch ? `no servers match “${srvSearch}”` : 'no servers'}
                      </p>
                    {:else}
                      {#each srvFiltered.slice(0, 60) as r}
                        {@const matches = srvMatchingNames.has(r.name)}
                        {@const picked = srvModeIsSpecific && dnsProvider === r.name}
                        {@const hoverable = srvModeIsSpecific
                          ? (matches
                              ? 'hover:bg-cursed-500/10 hover:border-cursed-500/40 cursor-pointer'
                              : 'hover:bg-ink-900 hover:border-ink-700 cursor-pointer')
                          : 'cursor-default'}
                        {@const baseClass = picked
                          ? 'bg-cursed-500/15 border border-cursed-500/60'
                          : matches
                          ? 'bg-live-500/10 border border-live-500/30'
                          : 'bg-ink-950/60 border border-ink-800'}
                        <button type="button"
                                disabled={!srvModeIsSpecific}
                                on:click={() => { if (srvModeIsSpecific) dnsProvider = r.name; }}
                                title={r.description}
                                class="w-full text-left p-2 rounded text-xs transition-colors {baseClass} {hoverable}">
                          <div class="flex items-center gap-2 flex-wrap">
                            {#if picked}
                              <span class="text-cursed-300 text-xs">●</span>
                            {/if}
                            <span class="font-mono text-zinc-200 truncate">{r.label}</span>
                            <span class="text-[10px] text-zinc-500">{r.operator}</span>
                            {#if r.country}
                              <span class="text-[10px] font-mono text-zinc-500">{r.country}</span>
                            {/if}
                            <span class="text-[10px] px-1.5 py-0.5 rounded
                                         {r.eyes === 'none' ? 'bg-live-500/15 text-live-300' :
                                          r.eyes === 'fourteen' ? 'bg-amber-500/15 text-amber-300' :
                                          r.eyes === 'nine' ? 'bg-orange-500/15 text-orange-300' :
                                          r.eyes === 'five' ? 'bg-red-500/15 text-red-300' :
                                          'bg-zinc-500/15 text-zinc-400'}">
                              {r.eyes === 'none' ? 'no eyes' :
                               r.eyes === 'unknown' ? '?' : r.eyes + ' eyes'}
                            </span>
                          </div>
                          <div class="flex items-center gap-3 mt-1 text-[10px] text-zinc-400">
                            <span title="Privacy score 0-5">
                              priv <span class="font-mono text-cursed-300">{r.privacy_score}</span>
                            </span>
                            <span title="Trust score 0-5">
                              trust <span class="font-mono text-cursed-300">{r.trust_score}</span>
                            </span>
                            <span class="font-mono text-zinc-600">{r.name}</span>
                            {#if r.no_logs}
                              <span class="px-1 rounded bg-zinc-700/40 text-zinc-300">no-logs</span>
                            {/if}
                            {#if r.dnssec}
                              <span class="px-1 rounded bg-zinc-700/40 text-zinc-300">dnssec</span>
                            {/if}
                            {#if r.no_filter}
                              <span class="px-1 rounded bg-zinc-700/40 text-zinc-300">unfiltered</span>
                            {/if}
                            {#each r.filters as f}
                              <span class="px-1 rounded bg-amber-500/15 text-amber-300">
                                blocks {f}
                              </span>
                            {/each}
                          </div>
                        </button>
                      {/each}
                      {#if srvFiltered.length > 60}
                        <p class="text-[10px] text-zinc-600 italic text-center pt-2">
                          Showing first 60 of {srvFiltered.length}. Narrow with
                          search to see more.
                        </p>
                      {/if}
                    {/if}
                  </div>
                </div>
              {/if}

              <!-- v62: custom DNSCrypt stamp now lives in a collapsed
                   sub-section instead of being gated on a "custom"
                   radio option. The catalog row picker above replaced
                   the radio entirely. Custom stamp stays an option for
                   self-hosted resolvers or pre-release stamps not yet
                   in the upstream list. -->
              <details class="pt-3 border-t border-ink-800">
                <summary class="cursor-pointer text-[11px] uppercase tracking-wider
                                text-zinc-500 hover:text-zinc-300">
                  ▸ Use a custom sdns:// stamp instead
                </summary>
                <div class="mt-3 space-y-2 border-l-2 border-cursed-500/40 pl-4">
                  <p class="text-xs text-zinc-500 leading-relaxed">
                    Override the catalog with a single hand-pasted stamp.
                    Picking a custom stamp implicitly switches the server
                    mode to <strong>specific</strong> and overrides the row
                    selection above.
                  </p>
                  <input type="text" bind:value={dnsCustomLabel}
                         placeholder="Friendly label (e.g. mycorp-dns)"
                         class="w-full bg-ink-800 border border-ink-700 rounded
                                px-3 py-2 text-sm text-zinc-200" />
                  <textarea bind:value={dnsCustomStamp} rows="3"
                            placeholder="sdns://AgcAAAAAAAAAAAAQZG5zLmV4YW1wbGUuY29tCi9kbnMtcXVlcnk"
                            class="w-full bg-ink-800 border border-ink-700 rounded
                                   px-3 py-2 text-xs text-zinc-200 font-mono break-all"
                  ></textarea>
                  <p class="text-xs text-zinc-500 leading-relaxed">
                    Paste an <code class="text-cursed-300">sdns://</code> stamp from
                    <a class="text-cursed-300 hover:underline"
                       href="https://dnscrypt.info/stamps/" target="_blank" rel="noreferrer">
                      dnscrypt.info/stamps</a>
                    or any provider's documentation. Stamps encode DNSCrypt v2,
                    DoH (DNS-over-HTTPS), DoT (DNS-over-TLS), or ODoH endpoints
                    along with their public key + hash pin.
                  </p>
                  <p class="text-[11px] text-amber-200/80 leading-relaxed">
                    ⚠ <strong>Heads-up:</strong> if your stamp is a DoH/DoT URL
                    (prefix <code>sdns://Ag…</code> or <code>sdns://Aw…</code>),
                    the resolver's hostname will be exposed via the TLS SNI
                    extension on every query. Picking from the catalog above
                    sticks to true DNSCrypt v2 stamps (prefix <code>sdns://AQ…</code>)
                    which have no SNI to leak.
                  </p>
                  <button type="button"
                          on:click={() => { dnsProvider = 'custom'; srvMode = 'specific'; }}
                          class="btn-secondary text-xs">
                    use this stamp →
                  </button>
                </div>
              </details>
            </div>

            <!-- ─── Anonymized DNSCrypt (v51+) ─── -->
            <!-- Separates the resolver-sees-queries half from the
                 relay-sees-IP half. Relay never sees queries (they're
                 encrypted to the resolver); resolver never sees client
                 IP (queries arrive from the relay). Real privacy
                 upgrade — and it's opt-in because each query takes one
                 extra hop. -->
            <div class="space-y-3 pt-4 border-t border-ink-800">
              <header class="space-y-1">
                <h4 class="font-mono text-xs uppercase tracking-wider text-zinc-300">
                  Anonymized DNSCrypt
                </h4>
                <p class="text-xs text-zinc-500 leading-relaxed">
                  Route queries through a relay so the resolver never
                  sees your IP — and the relay never sees your queries
                  (they're sealed to the resolver's key). Splits trust
                  between two operators. Adds ~30-100ms per uncached
                  lookup; first-load can feel slower, browsing stays
                  snappy thanks to dnscrypt-proxy's cache.
                </p>
              </header>

              <label class="flex items-center gap-3 cursor-pointer">
                <input type="checkbox" bind:checked={anonEnabled}
                       class="w-4 h-4 accent-cursed-500" />
                <span class="text-zinc-200 text-sm">
                  Enable anonymized relay routing
                </span>
              </label>

              <div class="space-y-3 pl-7"
                   class:opacity-40={!anonEnabled}
                   class:pointer-events-none={!anonEnabled}>

                <!-- Mode -->
                <div class="space-y-1">
                  <p class="text-[11px] uppercase tracking-wider text-zinc-500">
                    Mode
                  </p>
                  <div class="flex gap-3">
                    <label class="flex items-center gap-2 cursor-pointer text-xs">
                      <input type="radio" bind:group={anonMode} value="auto"
                             class="w-3 h-3 accent-cursed-500" />
                      <span class="text-zinc-300">Auto (pick 3 by criteria)</span>
                    </label>
                    <label class="flex items-center gap-2 cursor-pointer text-xs">
                      <input type="radio" bind:group={anonMode} value="specific"
                             class="w-3 h-3 accent-cursed-500" />
                      <span class="text-zinc-300">Specific relays</span>
                    </label>
                  </div>
                </div>

                {#if anonMode === 'auto'}
                  <!-- Criteria -->
                  <div class="space-y-2">
                    <p class="text-[11px] uppercase tracking-wider text-zinc-500">
                      Relay criteria
                    </p>
                    <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
                      <label class="flex items-center gap-2 cursor-pointer text-xs
                                    p-2 rounded border border-ink-800 bg-ink-950/40
                                    hover:border-ink-700">
                        <input type="checkbox" bind:checked={anonNoLogs}
                               class="w-3 h-3 accent-cursed-500" />
                        <span class="text-zinc-300">No-logs policy</span>
                      </label>
                      <label class="flex items-center gap-2 cursor-pointer text-xs
                                    p-2 rounded border border-ink-800 bg-ink-950/40
                                    hover:border-ink-700">
                        <input type="checkbox" bind:checked={anonDnssec}
                               class="w-3 h-3 accent-cursed-500" />
                        <span class="text-zinc-300">DNSSEC pass-through</span>
                      </label>
                      <label class="flex items-center gap-2 cursor-pointer text-xs
                                    p-2 rounded border border-ink-800 bg-ink-950/40
                                    hover:border-ink-700">
                        <input type="checkbox" bind:checked={anonOutsideFiveEyes}
                               class="w-3 h-3 accent-cursed-500" />
                        <span class="text-zinc-300">Outside Five Eyes</span>
                      </label>
                      <label class="flex items-center gap-2 cursor-pointer text-xs
                                    p-2 rounded border border-ink-800 bg-ink-950/40
                                    hover:border-ink-700">
                        <input type="checkbox" bind:checked={anonOutsideFourteenEyes}
                               class="w-3 h-3 accent-cursed-500" />
                        <span class="text-zinc-300">Outside Fourteen Eyes</span>
                      </label>
                    </div>
                    <p class="text-[11px] text-zinc-500 leading-relaxed">
                      Auto-pick chooses 3 relays from <strong>3 different
                      operators</strong> in <strong>3 different
                      jurisdictions</strong>. The resolver's own operator
                      (e.g. Quad9, AdGuard) is excluded automatically so
                      the same org never holds both halves of the
                      anonymization split.
                    </p>
                  </div>

                  <!-- Currently picked (preview) -->
                  {#if dnsState?.anonymized?.currently_picked?.length}
                    <div class="space-y-1 p-3 rounded bg-cursed-500/5
                                border border-cursed-500/30">
                      <p class="text-[11px] uppercase tracking-wider text-cursed-300">
                        Auto-picked relays
                      </p>
                      <div class="space-y-1">
                        {#each dnsState.anonymized.currently_picked as relayName}
                          {@const meta = dnsState.anonymized.catalog.find(r => r.name === relayName)}
                          <div class="flex items-center gap-2 text-xs font-mono">
                            <span class="text-cursed-200">{relayName}</span>
                            {#if meta}
                              <span class="text-zinc-500">
                                ({meta.operator}, {meta.country})
                              </span>
                            {/if}
                          </div>
                        {/each}
                      </div>
                      <p class="text-[10px] text-zinc-500 pt-1">
                        Selection updates whenever you change criteria + save.
                        dnscrypt-proxy round-robins among them so a single
                        relay outage doesn't break DNS.
                      </p>
                    </div>
                  {/if}

                {:else}
                  <!-- Manual relay multi-select — full upstream catalog -->
                  <div class="space-y-2">
                    <div class="flex flex-wrap items-center justify-between gap-2">
                      <p class="text-[11px] uppercase tracking-wider text-zinc-500">
                        Pick 1-8 relays manually
                      </p>
                      <p class="text-[11px] font-mono
                                {anonSpecificRelays.length === 0 ? 'text-amber-400' :
                                 anonSpecificRelays.length > 8 ? 'text-red-400' :
                                 'text-cursed-300'}">
                        {anonSpecificRelays.length} / 8 selected
                      </p>
                    </div>

                    <!-- Search + filter toggle -->
                    <div class="flex flex-wrap items-center gap-2">
                      <input type="text" bind:value={anonSearch}
                             placeholder="search: name, operator, country code…"
                             class="flex-1 min-w-0 bg-ink-800 border border-ink-700
                                    rounded px-3 py-1.5 text-xs text-zinc-200 font-mono" />
                      <label class="flex items-center gap-1.5 cursor-pointer text-[11px]
                                    text-zinc-400 hover:text-zinc-200">
                        <input type="checkbox" bind:checked={anonShowOnlySelected}
                               class="w-3 h-3 accent-cursed-500" />
                        only selected
                      </label>
                      {#if anonSpecificRelays.length > 0}
                        <button class="text-[10px] text-zinc-500 hover:text-red-400
                                       border border-ink-700 hover:border-red-500/50
                                       rounded px-2 py-1 transition-colors"
                                on:click={() => { anonSpecificRelays = []; }}>
                          clear all
                        </button>
                      {/if}
                    </div>

                    <!-- Grouped relay list (by operator) -->
                    <div class="max-h-96 overflow-y-auto space-y-3
                                border border-ink-800 rounded p-3 bg-ink-950/40">
                      {#if anonFiltered.length === 0}
                        <p class="text-xs text-zinc-500 italic text-center py-4">
                          {anonSearch ? `no relays match “${anonSearch}”` : 'no relays'}
                        </p>
                      {:else}
                        {#each Object.entries(anonGrouped) as [op, relays]}
                          <div class="space-y-1">
                            <div class="flex items-baseline gap-2 sticky top-0
                                        bg-ink-950/95 py-1 -mx-1 px-1">
                              <span class="font-mono text-[10px] uppercase tracking-wider
                                           text-cursed-300">{op}</span>
                              <span class="text-[10px] text-zinc-600">
                                {relays.length} {relays.length === 1 ? 'relay' : 'relays'}
                              </span>
                            </div>
                            {#each relays as r}
                              <label class="flex items-center gap-2 cursor-pointer text-xs
                                            p-1.5 rounded hover:bg-ink-800/50
                                            {anonSpecificRelays.includes(r.name) ? 'bg-cursed-500/10' : ''}"
                                     title={r.description}>
                                <input type="checkbox"
                                       checked={anonSpecificRelays.includes(r.name)}
                                       on:change={(e) => {
                                         if (e.currentTarget.checked) {
                                           anonSpecificRelays = [...anonSpecificRelays, r.name];
                                         } else {
                                           anonSpecificRelays = anonSpecificRelays.filter(n => n !== r.name);
                                         }
                                       }}
                                       class="w-3 h-3 accent-cursed-500 flex-shrink-0" />
                                <span class="text-zinc-300 font-mono truncate flex-1">{r.label}</span>
                                <span class="text-[10px] flex items-center gap-1.5 flex-shrink-0">
                                  <span class="text-zinc-500 font-mono">{r.country || '??'}</span>
                                  <span class="px-1.5 py-0.5 rounded
                                               {r.eyes === 'none' ? 'bg-live-500/15 text-live-300' :
                                                r.eyes === 'fourteen' ? 'bg-amber-500/15 text-amber-300' :
                                                r.eyes === 'nine' ? 'bg-orange-500/15 text-orange-300' :
                                                r.eyes === 'five' ? 'bg-red-500/15 text-red-300' :
                                                'bg-zinc-500/15 text-zinc-400'}">
                                    {r.eyes === 'none' ? 'no eyes' :
                                     r.eyes === 'unknown' ? '?' : r.eyes + ' eyes'}
                                  </span>
                                  {#if r.no_logs}
                                    <span class="px-1.5 py-0.5 rounded bg-zinc-700/30 text-zinc-400">
                                      no-logs
                                    </span>
                                  {/if}
                                </span>
                              </label>
                            {/each}
                          </div>
                        {/each}
                      {/if}
                    </div>

                    <p class="text-[11px] text-zinc-500 leading-relaxed">
                      {anonFiltered.length} of {anonCatalog.length} relays shown. Eyes tier
                      colors: <span class="text-live-300">no eyes</span> &gt;
                      <span class="text-amber-300">14 eyes</span> &gt;
                      <span class="text-orange-300">9 eyes</span> &gt;
                      <span class="text-red-300">5 eyes</span>. Pick relays from
                      <strong>multiple operators</strong> for real anonymization
                      — 3 relays all run by the same outfit aren't more private
                      than 1.
                    </p>
                  </div>
                {/if}

              </div>
            </div>

            <div class="flex items-center gap-3 pt-2 border-t border-ink-700">
              <button class="btn-primary" on:click={saveDns} disabled={dnsSaving}>
                {dnsSaving ? 'applying…' : 'save & apply'}
              </button>
              {#if dnsMsg}
                <span class="text-xs text-live-400 font-mono">{dnsMsg}</span>
              {/if}
            </div>
          </section>
        </div>
      </details>

      <!-- ─── VPN ─── -->
      <details bind:open={vpnOpen}
               class="bg-ink-900 border border-ink-700 rounded-xl">
        <summary class="cursor-pointer select-none px-6 py-4
                        flex items-center justify-between gap-3">
          <div class="flex items-center gap-3 min-w-0 flex-wrap">
            <span class="h-2 w-2 rounded-full flex-shrink-0
                         {vpnEnabled && vpnProvider !== 'none' ? 'bg-live-400 animate-pulse' : 'bg-zinc-600'}"></span>
            <span class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              VPN (WAN tunnel)
            </span>
            <span class="text-xs text-zinc-500 truncate">
              {vpnEnabled && vpnProvider !== 'none'
                ? `via ${vpnProvider}${vpnKillSwitch ? ' · kill-switch on' : ''}`
                : 'off — clearnet uses default route'}
            </span>
          </div>
          <span class="text-[10px] font-mono uppercase tracking-wider text-zinc-500 flex-shrink-0">
            {vpnOpen ? '▾ close' : '▸ expand'}
          </span>
        </summary>
        <div class="border-t border-ink-700 p-6 space-y-4">
          <section class="space-y-4">
            <header class="space-y-2">
              <p class="text-zinc-400 text-sm">
                Route this device's WAN traffic — including anything
                NAT'd through the USB ethernet — over an outbound VPN
                tunnel. Combine with <em>restricted</em> mode to make the
                tunnel the only exit path for the connected host.
              </p>
              <!-- v74: always-visible entry to the provider setup wizard.
                   Mullvad/IVPN need account registration there before they can
                   connect; the only links before were buried inside per-provider
                   conditionals you couldn't see until you'd already selected one. -->
              <a href="/network/vpn-providers"
                 class="inline-flex items-center gap-1.5 text-xs font-mono px-2.5 py-1.5 rounded
                        border border-cursed-500/40 text-cursed-300
                        hover:bg-cursed-500/10 transition-colors">
                ⚙ VPN provider setup wizard
                <span class="text-zinc-500 normal-case">— register / manage Mullvad · IVPN · AirVPN</span>
                →
              </a>
              <!-- v76: sign-up links for users without an account yet. AirVPN
                   is our referral link (supports the project); Mullvad + IVPN
                   have no referral program. -->
              <div class="flex items-center gap-2 flex-wrap text-[11px] text-zinc-500">
                <span>No account yet? Get one:</span>
                <a class="text-cursed-300 hover:underline" href="https://mullvad.net/" target="_blank" rel="noreferrer">Mullvad ↗</a>
                <span class="text-zinc-700">·</span>
                <a class="text-cursed-300 hover:underline" href="https://www.ivpn.net" target="_blank" rel="noreferrer">IVPN ↗</a>
                <span class="text-zinc-700">·</span>
                <a class="text-cursed-300 hover:underline" href="https://airvpn.org/?referred_by=832389" target="_blank" rel="noreferrer">AirVPN ↗</a>
              </div>
              <p class="text-[10px] text-zinc-600 leading-relaxed">
                AirVPN is a <span class="text-cursed-300/70">referral link</span> — it supports this project at no
                extra cost to you. Mullvad and IVPN are plain direct links (those providers have no referral program).
              </p>
            </header>

            <label class="flex items-center gap-3 cursor-pointer">
              <input type="checkbox" bind:checked={vpnEnabled}
                     class="w-4 h-4 accent-cursed-500" />
              <span class="text-zinc-200 text-sm">Enable VPN</span>
            </label>

            <div class="space-y-3 pl-7" class:opacity-40={!vpnEnabled} class:pointer-events-none={!vpnEnabled}>
              <p class="text-xs uppercase tracking-wider text-zinc-500">Provider</p>
              {#if vpnState}
                {#each vpnState.providers.filter((p) => p.id !== 'tor' && p.id !== 'i2p' && p.id !== 'tailscale') as p}
                  <label class="flex items-start gap-3 cursor-pointer">
                    <input type="radio" bind:group={vpnProvider} value={p.id}
                           class="mt-1 w-4 h-4 accent-cursed-500" />
                    <div class="space-y-1">
                      <div class="text-zinc-200 text-sm font-medium">{p.label}</div>
                      <p class="text-xs text-zinc-500">{p.blurb}</p>
                    </div>
                  </label>
                {/each}
              {/if}
            </div>

            <!-- v80: the Tailscale config block moved OUT of the VPN
                 provider conditional into its own toggle-gated section
                 in the Privacy Overlay Networks panel above (it's an
                 independent overlay now, like Tor + I2P — it runs
                 alongside any VPN). -->

            {#if vpnEnabled && vpnProvider === 'wireguard'}
              <div class="space-y-2 pl-7">
                <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="wg-conf">
                  WireGuard config
                  {#if vpnState?.wireguard.has_config}
                    <span class="text-cursed-400 normal-case ml-1 text-[10px]">
                      (one saved — paste to replace, leave blank to keep)
                    </span>
                  {/if}
                </label>
                <textarea id="wg-conf" bind:value={wgConfig} rows="10"
                          placeholder={`[Interface]
PrivateKey = …
Address = 10.0.0.2/24
DNS = 1.1.1.1

[Peer]
PublicKey = …
Endpoint = vpn.example.com:51820
AllowedIPs = 0.0.0.0/0`}
                          class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-2 text-xs text-zinc-200 font-mono"></textarea>
                <p class="text-xs text-zinc-500">
                  Paste the full contents of a working
                  <code>.conf</code> file. We run it via
                  <code>wg-quick@aeon0</code>. AllowedIPs of
                  <code>0.0.0.0/0</code> sends everything through the tunnel.
                </p>
              </div>
            {/if}

            {#if vpnEnabled && vpnProvider === 'openvpn'}
              <div class="space-y-3 pl-7">
                <div class="space-y-1">
                  <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="ov-conf">
                    OpenVPN config (.ovpn)
                    {#if vpnState?.openvpn.has_config}
                      <span class="text-cursed-400 normal-case ml-1 text-[10px]">
                        (one saved — paste to replace, leave blank to keep)
                      </span>
                    {/if}
                  </label>
                  <textarea id="ov-conf" bind:value={ovConfig} rows="10"
                            placeholder="client&#10;dev tun&#10;proto udp&#10;remote …&#10;…"
                            class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-2 text-xs text-zinc-200 font-mono"></textarea>
                </div>
                <div class="grid grid-cols-2 gap-3">
                  <div class="space-y-1">
                    <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="ov-user">
                      Username (optional)
                    </label>
                    <input id="ov-user" type="text" bind:value={ovUser}
                           autocomplete="off"
                           class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
                  </div>
                  <div class="space-y-1">
                    <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="ov-pass">
                      Password (optional)
                      {#if vpnState?.openvpn.has_auth_password}
                        <span class="text-cursed-400 normal-case ml-1 text-[10px]">
                          (saved — leave blank to keep)
                        </span>
                      {/if}
                    </label>
                    <input id="ov-pass" type="password" bind:value={ovPass}
                           autocomplete="off"
                           class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
                  </div>
                </div>
                <p class="text-xs text-zinc-500">
                  Only needed if your .ovpn references
                  <code>auth-user-pass</code> without inline credentials.
                </p>
              </div>
            {/if}

            {#if vpnEnabled && (vpnProvider === 'mullvad' || vpnProvider === 'ivpn' || vpnProvider === 'azirevpn' || vpnProvider === 'airvpn')}
              <!-- v61: inline banner pointing at the wizard. The
                   per-provider state endpoint tells us whether the
                   user has registered an account + selected a server
                   yet. If not, we hard-gate "Save & Apply" above
                   (see saveVpn) so the user gets a clear "go to the
                   wizard first" message instead of an opaque
                   wg-quick failure. -->
              <div class="space-y-3 pl-7">
                {#if wizardProviderConfigured[vpnProvider]}
                  <div class="p-4 rounded-lg border border-live-500/40 bg-live-500/10 space-y-3">
                    <div class="flex items-center gap-2 text-live-200 text-sm font-medium">
                      <span class="text-live-400">✓</span> {vpnProvider} is configured
                    </div>
                    <!-- v74: inline server picker — choose a server or auto-pick
                         the fastest right here; no trip to the wizard to switch. -->
                    <div class="space-y-1.5">
                      <label class="text-[11px] uppercase tracking-wider text-zinc-500 block" for="vpn-srv">
                        Server <span class="text-zinc-600">({(wizardProviderServers[vpnProvider] ?? []).length} available)</span>
                      </label>
                      <div class="flex flex-wrap items-center gap-2">
                        <select id="vpn-srv"
                                class="flex-1 min-w-[14rem] bg-ink-800 border border-ink-700 rounded px-2 py-1.5 text-sm text-zinc-200 disabled:opacity-50"
                                value={wizardProviderServer[vpnProvider]}
                                on:change={(e) => selectVpnServer(vpnProvider, e.currentTarget.value)}
                                disabled={serverBusy === vpnProvider}>
                          <option value="">— none pinned (provider picks) —</option>
                          {#each (wizardProviderServers[vpnProvider] ?? []) as sv}
                            <option value={sv.id}>{sv.label} · {sv.hostname}{sv.eyes === 'none' ? ' · 🛡 No Eyes' : ''}</option>
                          {/each}
                        </select>
                        <button class="btn text-xs whitespace-nowrap"
                                on:click={() => pickFastestVpnServer(vpnProvider)}
                                disabled={serverBusy === vpnProvider}>
                          {serverBusy === vpnProvider ? 'probing…' : '⚡ Pick fastest now'}
                        </button>
                        <button class="btn text-xs whitespace-nowrap"
                                title="Fastest server in a country OUTSIDE the 5/9/14-Eyes intelligence-sharing alliances"
                                on:click={() => pickFastestVpnServer(vpnProvider, true)}
                                disabled={serverBusy === vpnProvider}>
                          {serverBusy === vpnProvider ? 'probing…' : '🛡 Fastest · No Eyes'}
                        </button>
                      </div>
                      {#if fastestMsg}<p class="text-[11px] text-live-300 leading-snug">{fastestMsg}</p>{/if}
                    </div>
                    <p class="text-[11px] text-zinc-500 leading-relaxed">
                      Pick a server (or the fastest), then <strong>Save &amp; Apply</strong> below to bring the tunnel up.
                      <a class="text-cursed-300 hover:underline ml-1"
                         href="/network/vpn-providers?provider={vpnProvider}">advanced / refresh keys →</a>
                    </p>
                  </div>
                {:else}
                  <div class="p-4 rounded-lg border border-amber-500/40
                              bg-amber-500/10 space-y-3">
                    <div class="flex items-start gap-3">
                      <span class="text-amber-400 text-lg leading-none mt-0.5">⚠</span>
                      <div class="space-y-1">
                        <div class="text-amber-200 text-sm font-medium">
                          {vpnProvider} needs setup
                        </div>
                        <p class="text-xs text-amber-100/70 leading-relaxed">
                          {#if vpnProvider === 'mullvad'}
                            Paste your 16-digit Mullvad account number into the
                            wizard. The supervisor generates a WireGuard keypair,
                            registers the pubkey with Mullvad's API, and caches
                            their server list. No email, no password — Mullvad's
                            anonymous-account model is one of the reasons it's the
                            trust-rating leader of the three.
                          {:else if vpnProvider === 'ivpn'}
                            Paste your IVPN account ID (starts with <code>ivpn</code>)
                            into the wizard. Same flow as Mullvad: keypair generated
                            on-device, pubkey registered with IVPN's API, server list
                            cached. IVPN's HQ is Gibraltar — outside 14-Eyes — and
                            their no-logs policy was Cure53-audited.
                          {:else if vpnProvider === 'airvpn'}
                            Open the wizard, pick a connection mode — WireGuard, plain
                            OpenVPN, or a <strong>stealth</strong> mode
                            (OpenVPN-over-SSL looks like HTTPS, OpenVPN-over-SSH looks
                            like a remote shell) — paste your AirVPN API key, and paste
                            the matching config from AirVPN's Config Generator. The key
                            drives the server list + No-Eyes / fastest picks.
                          {:else}
                            Paste your AzireVPN <strong>API token</strong> into the
                            wizard — this is <em>not</em> your account ID. While logged
                            in, generate one at
                            <a class="underline text-amber-200 hover:text-amber-100"
                               href="https://manager.azirevpn.com/account/token"
                               target="_blank" rel="noopener"
                               >manager.azirevpn.com/account/token</a>.
                            Same on-device WireGuard keypair flow: the supervisor
                            registers your pubkey via AzireVPN's <code>/v3/ips</code>
                            endpoint and caches the server list. Boutique pick —
                            long-running no-log claim, but no formal 3rd-party audit yet.
                          {/if}
                        </p>
                      </div>
                    </div>
                    <a class="btn-primary text-xs ml-7 inline-block"
                       href="/network/vpn-providers?provider={vpnProvider}">
                      open setup wizard →
                    </a>
                  </div>
                {/if}
              </div>
            {/if}

            {#if vpnEnabled && vpnProvider === 'tor'}
              <div class="space-y-3 pl-7">

                <!-- Hard-to-miss warning: Tor + plain DNS = a bad time.
                     Tor's TransPort only carries TCP, so UDP DNS gets
                     dropped. Tor's own DNSPort can resolve A/AAAA but
                     not MX/TXT/SRV/etc., which breaks plenty of apps.
                     DNSCrypt-over-Tor is the fix — true DNSCrypt v2
                     is TCP-friendly and leaks no resolver SNI.
                     One click installs the recommended default. -->
                {#if !dnsEnabled}
                  <div class="p-4 rounded-lg border border-amber-500/40
                              bg-amber-500/10 space-y-3">
                    <div class="flex items-start gap-3">
                      <span class="text-amber-400 text-lg leading-none mt-0.5">⚠</span>
                      <div class="space-y-1">
                        <div class="text-amber-200 text-sm font-medium">
                          DNSCrypt is recommended when Tor is active
                        </div>
                        <p class="text-xs text-amber-100/70 leading-relaxed">
                          Tor can't carry UDP, so plain DNS-over-UDP gets dropped.
                          Tor's own resolver only handles A/AAAA/CNAME — many apps
                          (mail clients, browsers, certificate validators) also need
                          TXT, MX, SRV, DNSSEC. Enabling DNSCrypt routes encrypted
                          DNS over Tor's TCP TransPort, which makes generic DNS work
                          reliably end-to-end <em>without</em> leaking the resolver's
                          hostname via TLS SNI (the way DoH would). Quad9's audited
                          zero-log resolver is the default — change it any time in
                          the Encrypted DNS panel above.
                        </p>
                      </div>
                    </div>
                    <button
                      class="btn-primary text-xs ml-7"
                      on:click={enableDefaultDnscrypt}
                      disabled={dnsSaving}>
                      {dnsSaving ? 'enabling…' : '✓ enable Quad9 DNSCrypt'}
                    </button>
                  </div>
                {/if}

                <!-- Bridge preset selector -->
                <div class="space-y-2" role="radiogroup" aria-label="Tor bridge preset">
                  <p class="text-xs uppercase tracking-wider text-zinc-500">
                    Bridge preset
                  </p>
                  {#if vpnState?.tor.presets}
                    {#each vpnState.tor.presets as p}
                      <label class="flex items-start gap-3 cursor-pointer">
                        <input type="radio" bind:group={torPreset} value={p.id}
                               class="mt-1 w-4 h-4 accent-cursed-500" />
                        <div class="space-y-1">
                          <div class="text-zinc-200 text-sm font-medium">{p.label}</div>
                          <p class="text-xs text-zinc-500">{p.blurb}</p>
                        </div>
                      </label>
                    {/each}
                  {/if}
                </div>

                <!-- Custom bridge text area — only relevant for preset=custom -->
                {#if torPreset === 'custom'}
                  <div class="space-y-1 pt-2 border-t border-ink-800">
                    <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="tor-br">
                      Custom bridge lines
                      {#if vpnState?.tor.has_bridges}
                        <span class="text-cursed-400 normal-case ml-1 text-[10px]">
                          (saved — paste to replace, leave blank to keep)
                        </span>
                      {/if}
                    </label>
                    <textarea id="tor-br" bind:value={torBridges} rows="4"
                              placeholder={`obfs4 12.34.56.78:443 BB6E…1A2B cert=…  iat-mode=0
obfs4 …`}
                              class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-2 text-xs text-zinc-200 font-mono"></textarea>
                    <p class="text-xs text-zinc-500">
                      Request fresh bridges from
                      <a class="text-cursed-300 hover:underline"
                         href="https://bridges.torproject.org/" target="_blank" rel="noreferrer">
                        bridges.torproject.org</a>. One bridge per line.
                      Most users won't need this — try one of the built-in
                      presets above first.
                    </p>
                  </div>
                {/if}

                <p class="text-xs text-zinc-500 pt-2 border-t border-ink-800">
                  Tor runs a transparent proxy on <code>127.0.0.1:9040</code>
                  and DNS on <code>127.0.0.1:5353</code>; iptables redirects
                  all outbound TCP + DNS through it, including traffic from
                  USB-connected client devices. UDP is dropped (Tor doesn't
                  carry UDP). When DNSCrypt is also enabled, encrypted DNS
                  queries ride through Tor's TransPort too — your ISP sees
                  only Tor traffic, and there's no DoH SNI giving away the
                  resolver brand.
                </p>
              </div>
            {/if}

            {#if vpnEnabled && vpnProvider === 'i2p'}
              <!-- v57: I2P needs more setup than Tor (browser proxy
                   config + .i2p name resolution happens in i2pd's
                   address book, not DNS). Surface that here + link
                   to the dedicated /network/i2p page where the
                   detailed status + browser proxy URLs live. -->
              <div class="space-y-2 pl-7">
                <div class="p-3 rounded border border-amber-500/40 bg-amber-500/10
                            flex items-start gap-2">
                  <span class="text-amber-400 text-sm leading-none">⚠</span>
                  <div class="space-y-1 flex-1">
                    <p class="text-xs text-amber-200">
                      <strong>i2pd must be installed + running for I2P traffic
                      to route.</strong> Pre-installed on AEON images, but the
                      daemon doesn't bind on usb0 until you save this VPN
                      change. Unlike Tor, I2P isn't transparent — browsers
                      have to point at i2pd's HTTP proxy explicitly to reach
                      <code class="text-amber-300">.i2p</code> sites.
                    </p>
                    <a href="/network/i2p"
                       class="inline-flex items-center gap-1 mt-1
                              text-xs px-2 py-1 rounded
                              border border-cursed-500/40 text-cursed-300
                              hover:bg-cursed-500/10 transition-colors font-mono">
                      I2P config →
                    </a>
                  </div>
                </div>

                <label class="text-xs uppercase tracking-wider text-zinc-500 block pt-2" for="i2p-out">
                  Outproxy (optional)
                </label>
                <input id="i2p-out" type="text" bind:value={i2pOutproxy}
                       placeholder="exit.stormycloud.i2p"
                       autocomplete="off"
                       class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
                <p class="text-xs text-zinc-500">
                  Without an outproxy, i2pd only reaches <code>.i2p</code>
                  sites (the safest default). Set an outproxy to also reach
                  the regular internet through I2P — slower than Tor, less
                  anonymous than a real VPN. Proxy URLs + status live on
                  the <a class="text-cursed-300 hover:underline" href="/network/i2p">I2P
                  config page</a>.
                </p>
              </div>
            {/if}

            <!-- Kill-switch + LAN bypass — applies to all providers -->
            {#if vpnEnabled && vpnProvider !== 'none'}
              <div class="space-y-3 pl-7 pt-3 border-t border-ink-800">
                <label class="flex items-start gap-3 cursor-pointer">
                  <input type="checkbox" bind:checked={vpnKillSwitch}
                         class="mt-1 w-4 h-4 accent-cursed-500" />
                  <div class="space-y-1">
                    <div class="text-zinc-200 text-sm font-medium">
                      Kill-switch
                      <span class="text-xs text-red-400 ml-1">(strict)</span>
                    </div>
                    <p class="text-xs text-zinc-500">
                      Drop all outbound traffic that doesn't go through the
                      VPN. If the tunnel is down or fails to come up, the
                      device stops talking to the WAN entirely. Loopback
                      and the LAN-bypass subnet below stay reachable so
                      you can still manage the device from your LAN.
                    </p>
                  </div>
                </label>

                <div class="space-y-1 pl-7">
                  <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="lan-byp">
                    LAN bypass subnet
                  </label>
                  <input id="lan-byp" type="text" bind:value={vpnLanBypass}
                         placeholder="192.168.0.0/16"
                         class="w-full max-w-xs bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
                  <p class="text-xs text-zinc-500">
                    Traffic to this CIDR is allowed even with the
                    kill-switch on. Leave it pointed at your home LAN so
                    you can always reach the device's web UI / SSH from
                    inside the network. Blank string = no bypass (you'd
                    need to manage exclusively via Tailscale or the USB-C
                    link).
                  </p>
                </div>
              </div>
            {/if}

            <div class="flex items-center gap-3 pt-2 border-t border-ink-700">
              <button class="btn-primary" on:click={saveVpn} disabled={vpnSaving}>
                {vpnSaving ? 'applying…' : 'save & apply'}
              </button>
              {#if vpnMsg}
                <span class="text-xs text-live-400 font-mono">{vpnMsg}</span>
              {/if}
            </div>

            <!-- ────────────────────────────────────────────────────── -->
            <!-- Live overlay status panels — polled every 4 s         -->
            <!-- v61: one panel per active overlay (clearnet VPN,      -->
            <!--      Tor, I2P). Previously gated on the legacy        -->
            <!--      vpn.provider field, which silently hid Tor +     -->
            <!--      I2P status after the v58 toggle split.           -->
            <!-- ────────────────────────────────────────────────────── -->
            {#if statusOverlays.length > 0}
              <div class="mt-4 pt-4 border-t border-ink-700 space-y-4">
                {#each statusOverlays as ov (ov.kind + ':' + ov.provider)}
                  <div class="space-y-3 p-3 rounded-lg border border-ink-800 bg-ink-950/40">
                    <header class="flex items-center justify-between gap-2">
                      <div class="flex items-center gap-2 flex-wrap">
                        <span class="font-mono text-xs uppercase tracking-wider text-zinc-300">
                          {overlayLabel(ov)}
                        </span>
                        {#if ov.state === 'connected'}
                          <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                                       bg-live-900/40 border border-live-500/40
                                       text-live-300 text-[10px] font-mono uppercase">
                            <span class="h-1.5 w-1.5 rounded-full bg-live-400 animate-pulse"></span>
                            connected
                          </span>
                        {:else if ov.state === 'establishing'}
                          <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                                       bg-amber-900/40 border border-amber-500/40
                                       text-amber-300 text-[10px] font-mono uppercase">
                            <span class="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"></span>
                            establishing
                          </span>
                        {:else if ov.state === 'reconnecting'}
                          <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                                       bg-amber-900/40 border border-amber-500/40
                                       text-amber-300 text-[10px] font-mono uppercase">
                            reconnecting
                          </span>
                        {:else}
                          <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                                       bg-red-900/40 border border-red-500/40
                                       text-red-300 text-[10px] font-mono uppercase">
                            <span class="h-1.5 w-1.5 rounded-full bg-red-400"></span>
                            {ov.state}
                          </span>
                        {/if}
                      </div>

                      <!-- One rotate button per overlay isn't worth it
                           — Tor's NEWNYM, Tailscale reset, WG reconnect
                           and i2pd tunnel rebuild are all triggered by
                           the single /api/network/vpn/rotate endpoint
                           which knows which to call based on what's
                           active. Show the rotate button only on the
                           first overlay so the row doesn't get noisy. -->
                      {#if ov === statusOverlays[0]}
                        <button class="btn text-xs whitespace-nowrap"
                                on:click={rotateIdentity}
                                disabled={rotating || ov.state !== 'connected'}
                                title="Refresh identity / circuits / keys">
                          {rotating ? 'rotating…' : '↻ change identity'}
                        </button>
                      {/if}
                    </header>

                    {#if ov === statusOverlays[0] && rotateMsg}
                      <p class="text-xs font-mono text-zinc-400">{rotateMsg}</p>
                    {/if}

                    <p class="text-xs text-zinc-400">{ov.summary}</p>

                    <!-- Bootstrap progress bar (Tor / I2P) -->
                    {#if ov.bootstrap_percent !== null && ov.bootstrap_percent !== undefined && ov.bootstrap_percent < 100}
                      <div class="space-y-1">
                        <div class="flex justify-between text-[10px] font-mono text-zinc-500">
                          <span>BOOTSTRAP</span>
                          <span>{ov.bootstrap_percent}%</span>
                        </div>
                        <div class="h-1.5 rounded-full bg-ink-800 overflow-hidden">
                          <div class="h-full bg-cursed-500 transition-all duration-300"
                               style="width: {ov.bootstrap_percent}%"></div>
                        </div>
                      </div>
                    {/if}

                    <!-- Public IP + country -->
                    {#if ov.public_ip}
                      <div class="flex items-center gap-3 text-xs font-mono">
                        <span class="text-zinc-500">Public IP:</span>
                        <span class="text-zinc-200">{ov.public_ip}</span>
                        {#if ov.public_country}
                          <span class="text-cursed-300 uppercase tracking-wider">
                            {ov.public_country}
                          </span>
                        {/if}
                      </div>
                    {/if}

                    <!-- Tor circuit list -->
                    {#if ov.detail.circuits && ov.detail.circuits.length > 0}
                      <div class="space-y-1">
                        <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">
                          Active circuits ({ov.detail.circuits.length})
                        </p>
                        <div class="space-y-0.5 max-h-32 overflow-y-auto font-mono text-[10px] text-zinc-400">
                          {#each ov.detail.circuits.slice(0, 6) as c}
                            <div class="truncate">
                              <span class="text-zinc-600">#{c.id}</span>
                              {c.hops.join(' → ')}
                            </div>
                          {/each}
                        </div>
                      </div>
                    {/if}

                    <!-- Tailscale peer list — Tailscale-only. WireGuard
                         providers (wireguard/mullvad/ivpn) also expose a WG
                         "peer" but with no host/ips, which rendered here as
                         "undefined undefined". Gate strictly to tailscale. -->
                    {#if ov.provider === 'tailscale' && ov.detail.peers && ov.detail.peers.length > 0}
                      <div class="space-y-1">
                        <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">
                          Tailnet peers ({ov.detail.peers.length})
                        </p>
                        <div class="space-y-0.5 max-h-32 overflow-y-auto font-mono text-[10px]">
                          {#each ov.detail.peers as p}
                            <div class="flex items-center gap-2 truncate">
                              <span class={p.online ? 'text-live-400' : 'text-zinc-600'}>●</span>
                              <span class="text-zinc-300">{p.host}</span>
                              <span class="text-zinc-500">{p.ips?.[0]}</span>
                              {#if p.exit_node}
                                <span class="text-cursed-400 text-[9px]">[exit]</span>
                              {/if}
                            </div>
                          {/each}
                        </div>
                      </div>
                    {/if}

                    <!-- I2P peer count -->
                    {#if ov.detail.active_peers !== undefined}
                      <p class="text-xs font-mono text-zinc-400">
                        <span class="text-zinc-500">Active peers:</span>
                        {ov.detail.active_peers}
                      </p>
                    {/if}

                    <!-- WireGuard handshake age -->
                    {#if ov.detail.handshake_age_s !== undefined && ov.detail.handshake_age_s !== null}
                      <p class="text-xs font-mono text-zinc-400">
                        <span class="text-zinc-500">Last handshake:</span>
                        {ov.detail.handshake_age_s}s ago
                      </p>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          </section>
        </div>
      </details>

      <!-- ─── Tailscale ─── -->
      <!-- v90: Tailscale is its own top-level section now (lifted out of
           the Privacy Overlay panel). It's an independent mesh that runs
           ALONGSIDE any VPN; tsEnabled is the single source of truth,
           saved on every network request via saveVpn(). -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-6 space-y-5">
        <header class="space-y-1">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Tailscale
          </h2>
          <p class="text-zinc-400 text-sm">
            Zero-config WireGuard mesh for remote access. Split-tunnel —
            only tailnet addresses go over the mesh; everything else
            (incl. exit-node traffic) uses your normal connection and
            inherits any VPN / DNSCrypt / Tor you've configured.
          </p>
        </header>

        <label class="flex items-center gap-3 cursor-pointer">
          <input type="checkbox" bind:checked={tsEnabled}
                 class="w-4 h-4 accent-cursed-500" />
          <span class="text-zinc-200 text-sm font-medium">Enable Tailscale</span>
          <span class="text-[10px] uppercase tracking-wider text-zinc-500">
            • WireGuard mesh
          </span>
          <a href="/agent/dash" class="ml-auto text-[10px] px-2 py-1 rounded
                    border border-cursed-500/40 text-cursed-300
                    hover:bg-cursed-500/10 transition-colors font-mono">
            Tailnet devices →
          </a>
        </label>

        <!-- live Tailscale status — online state + tailnet IP + peers.
             Polls every few seconds from aeon-vpn-status. Visible
             whenever Tailscale is enabled (independent of the VPN). -->
        {#if tsEnabled && tsOverlay}
          {@const connected = tsOverlay.state === 'connected'}
          <div class="ml-7 p-3 rounded-lg border border-cursed-500/30 bg-cursed-500/5 space-y-1.5">
            <div class="flex items-center justify-between gap-2">
              <span class="text-xs font-mono uppercase tracking-wider
                           {connected ? 'text-live-300' : 'text-amber-300'}">
                {connected ? '● Tailscale connected' : '◐ Tailscale ' + (tsOverlay.state ?? 'starting')}
              </span>
            </div>
            {#if tsOverlay.summary}
              <p class="text-[11px] text-zinc-500 leading-snug">{tsOverlay.summary}</p>
            {/if}
          </div>
        {/if}

        <div class="space-y-3 pl-7"
             class:opacity-40={!tsEnabled}
             class:pointer-events-none={!tsEnabled}>
          <p class="text-[11px] text-zinc-500 leading-relaxed">
            Tailscale is a WireGuard mesh — it joins this Pi to your
            tailnet and stays <strong>split-tunnel</strong> (only the
            tailnet's own 100.64.0.0/10 rides the mesh). Everything else,
            including traffic forwarded for exit-node clients, uses the
            Pi's normal egress and inherits any VPN / DNSCrypt / Tor.
            Runs alongside Tor / I2P / a WAN VPN in any combination.
          </p>

          <div class="space-y-1">
            <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="ts-auth">
              Auth key
              {#if vpnState?.tailscale.has_auth_key}
                <span class="text-cursed-400 normal-case ml-1 text-[10px]">
                  (one already saved — leave blank to keep it)
                </span>
              {/if}
            </label>
            <input id="ts-auth" type="password" bind:value={tsAuthKey}
                   placeholder="tskey-auth-…"
                   autocomplete="off"
                   class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
            <p class="text-xs text-zinc-500">
              Generate from
              <a class="text-cursed-300 hover:underline"
                 href="https://login.tailscale.com/admin/settings/keys"
                 target="_blank" rel="noreferrer">login.tailscale.com</a>
              — Settings → Keys → Generate auth key. One-time use is
              fine; we run <code>tailscale up</code> once with it
              and the daemon keeps the resulting node key.
            </p>
          </div>

          <div class="space-y-1">
            <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="ts-host">
              Hostname (optional)
            </label>
            <input id="ts-host" type="text" bind:value={tsHostname}
                   placeholder="aeon-magick"
                   class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
            <p class="text-xs text-zinc-500">
              Name this device shows up as in your tailnet. Defaults
              to the Pi's hostname.
            </p>
          </div>

          <label class="flex items-start gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={tsAdvertiseExit}
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1">
              <div class="text-zinc-200 text-sm font-medium">Advertise as exit node</div>
              <p class="text-xs text-zinc-500">
                Make this Pi available as a tailnet exit node so
                <em>other</em> machines on your tailnet can route their
                WAN through it. You'll still need to approve the
                offer from the Tailscale admin UI. Exit-node traffic
                rides your normal WAN egress (VPN / DNSCrypt / Tor if
                configured). <span class="text-amber-300">⚠ Verify with a
                live leak-test before relying on it.</span>
              </p>
            </div>
          </label>

          <label class="flex items-start gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={tsExitNode}
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1">
              <div class="text-zinc-200 text-sm font-medium">Use a tailnet exit node</div>
              <p class="text-xs text-zinc-500">
                Route <em>this</em> Pi's outbound traffic through
                another tailnet exit node. After saving, SSH in and
                run <code>tailscale set --exit-node=&lt;host&gt;</code>
                to pick which one.
              </p>
            </div>
          </label>
        </div>

        <!-- Tailscale shares the atomic network save (saveVpn persists
             Tailscale + Tor + I2P + VPN config in one request). -->
        <div class="flex items-center gap-3 pt-2 border-t border-ink-700">
          <button class="btn-primary" on:click={saveVpn} disabled={vpnSaving}>
            {vpnSaving ? 'saving…' : 'Save & Apply'}
          </button>
          {#if vpnMsg}<span class="text-xs text-live-300">{vpnMsg}</span>{/if}
        </div>
      </section>

      <!-- ─── Privacy Overlay Networks (Tor + I2P) ─── -->
      <details bind:open={overlaysOpen}
               class="bg-ink-900 border border-ink-700 rounded-xl">
        <summary class="cursor-pointer select-none px-6 py-4
                        flex items-center justify-between gap-3">
          <div class="flex items-center gap-3 min-w-0 flex-wrap">
            <span class="h-2 w-2 rounded-full flex-shrink-0
                         {torEnabled || i2pEnabled ? 'bg-live-400 animate-pulse' : 'bg-zinc-600'}"></span>
            <span class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              Privacy Overlay Networks
            </span>
            <span class="text-xs text-zinc-500 truncate">
              <!-- v90: Tailscale moved to its own top-level section — this
                   panel summarises only the Tor + I2P overlays again. -->
              {#if torEnabled || i2pEnabled}
                {[
                  torEnabled && `Tor (${torMode === 'split_tunnel' ? '.onion only' : 'all traffic'})`,
                  i2pEnabled && 'I2P',
                ].filter(Boolean).join(' + ')}
              {:else}
                all off
              {/if}
            </span>
          </div>
          <span class="text-[10px] font-mono uppercase tracking-wider text-zinc-500 flex-shrink-0">
            {overlaysOpen ? '▾ close' : '▸ expand'}
          </span>
        </summary>
        <div class="border-t border-ink-700 p-6 space-y-4">
          <section class="space-y-4">
            <header class="space-y-1">
              <p class="text-zinc-400 text-sm">
                Tor and I2P are independent of the VPN — either (or both)
                can run at the same time. <code class="text-cursed-300">.onion</code>
                sites go through Tor, <code class="text-cursed-300">.i2p</code>
                sites through I2P, and clearnet goes through the VPN (or
                straight WAN if no VPN). Tor's "all traffic" mode pulls
                clearnet in too, but I2P always stays independent so that
                network stays reachable.
              </p>
              <!-- v62: browser compatibility tip (was on the user's
                   ask list — common failure mode is "I enabled Tor
                   but my browser can't load anything"). -->
              <div class="mt-2 p-3 rounded border border-ink-800 bg-ink-950/40
                          text-[11px] text-zinc-400 leading-relaxed space-y-1.5">
                <p>
                  <span class="text-cursed-300 font-medium">Browser tuning for Tor mode.</span>
                  Best supported: <strong>Brave, Chrome, Firefox</strong>.
                  Safari + iCloud Private Relay conflict with Tor exits;
                  disable Private Relay if you must use Safari.
                </p>
                <ul class="ml-4 list-disc space-y-1">
                  <li>
                    <strong>Disable the browser's secure DNS</strong> —
                    Brave/Chrome <code>Settings → Security → Use secure DNS = off</code>;
                    Firefox <code>about:preferences#privacy → DNS over HTTPS = off</code>.
                    The browser's DoH conflicts with the Pi's encrypted DNS path.
                  </li>
                  <li>
                    <strong>Allow .onion in non-Tor windows</strong> —
                    Brave <code>brave://settings/privacy → Tor windows → Allow .onion in non-Tor windows = on</code>.
                    Firefox <code>about:config → network.dns.blockDotOnion = false</code>.
                    Without this, .onion URLs are blocked before they reach Tor's resolver.
                  </li>
                  <li>
                    Want hardened anonymity (fingerprinting, anti-tracking,
                    safer defaults)? Use <strong>Tor Browser</strong> on top
                    of split-tunnel mode rather than transparent Tor.
                  </li>
                </ul>
              </div>
            </header>

            <!-- ── Tor ── -->
            <div class="space-y-3 p-4 rounded-lg border border-ink-700 bg-ink-950/30">
              <label class="flex items-center gap-3 cursor-pointer">
                <input type="checkbox" bind:checked={torEnabled}
                       class="w-4 h-4 accent-cursed-500" />
                <span class="text-zinc-200 text-sm font-medium">Enable Tor</span>
                <span class="text-[10px] uppercase tracking-wider text-zinc-500">
                  • TransPort 9040 • DNSPort 5353
                </span>
              </label>

              <!-- v73: live Tor status — bootstrap progress bar + relay
                   circuit listing (restored; v62 dropped this view). Stays
                   visible (not dimmed) whenever Tor is enabled. Data polls
                   every 4s from aeon-vpn-status (GETINFO bootstrap + circuits). -->
              {#if torEnabled}
                <div class="ml-7 p-3 rounded-lg border border-cursed-500/30 bg-cursed-500/5 space-y-2">
                  {#if torOverlay}
                    {@const pct = torOverlay.bootstrap_percent ?? 0}
                    {@const circuits = torOverlay.detail?.circuits ?? []}
                    {@const connected = torOverlay.state === 'connected' || pct >= 100}
                    <div class="flex items-center justify-between gap-2">
                      <span class="text-xs font-mono uppercase tracking-wider
                                   {connected ? 'text-live-300' : 'text-amber-300'}">
                        {connected ? '● Tor connected' : '◐ Bootstrapping Tor'}
                      </span>
                      <span class="text-[11px] font-mono text-zinc-400">{pct}%</span>
                    </div>
                    <div class="h-1.5 rounded-full bg-ink-800 overflow-hidden">
                      <div class="h-full rounded-full transition-[width] duration-500
                                  {connected ? 'bg-live-400' : 'bg-amber-400'}"
                           style="width: {pct}%"></div>
                    </div>
                    {#if torOverlay.summary}
                      <p class="text-[11px] text-zinc-500 leading-snug">{torOverlay.summary}</p>
                    {/if}
                    {#if circuits.length}
                      <div class="pt-1 space-y-1">
                        <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">
                          Relay circuits ({circuits.length}){#if torOverlay.public_country} · exit {torOverlay.public_country}{/if}
                        </p>
                        {#each circuits as c (c.id)}
                          <div class="font-mono text-[11px] text-zinc-400 flex items-baseline gap-2">
                            <span class="text-zinc-600 flex-shrink-0">#{c.id}</span>
                            <span class="truncate">
                              {#each c.hops as hop, i}<span class="{i === 0 ? 'text-cursed-300' : i === c.hops.length - 1 ? 'text-live-300' : 'text-zinc-300'}">{hop}</span>{#if i < c.hops.length - 1}<span class="text-zinc-600"> → </span>{/if}{/each}
                            </span>
                          </div>
                        {/each}
                      </div>
                    {:else if connected}
                      <p class="text-[11px] text-zinc-500">Connected — no circuits built yet (idle / split-tunnel).</p>
                    {/if}
                  {:else}
                    <p class="text-[11px] text-zinc-500 font-mono">Starting Tor… status appears within a few seconds.</p>
                  {/if}
                </div>
              {/if}

              <div class="space-y-3 pl-7"
                   class:opacity-40={!torEnabled}
                   class:pointer-events-none={!torEnabled}>

                <!-- Mode radio -->
                <div class="space-y-2">
                  <p class="text-[11px] uppercase tracking-wider text-zinc-500">
                    Routing mode
                  </p>
                  <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
                    <label class="flex items-start gap-3 cursor-pointer p-3 rounded
                                  border transition-colors
                                  {torMode === 'split_tunnel'
                                    ? 'bg-cursed-500/10 border-cursed-500/50'
                                    : 'bg-ink-950/40 border-ink-800 hover:border-ink-700'}">
                      <input type="radio" bind:group={torMode} value="split_tunnel"
                             class="mt-1 w-4 h-4 accent-cursed-500" />
                      <div class="space-y-1 flex-1 min-w-0">
                        <div class="text-zinc-200 text-sm">Split tunnel (.onion only)</div>
                        <p class="text-[11px] text-zinc-500 leading-relaxed">
                          Only TCP destined for a <code>.onion</code> address rides
                          Tor. Clearnet keeps the default route (or the VPN if
                          one is selected). Recommended default — Tor stays out
                          of your normal browsing.
                        </p>
                      </div>
                    </label>
                    <label class="flex items-start gap-3 cursor-pointer p-3 rounded
                                  border transition-colors
                                  {torMode === 'transparent'
                                    ? 'bg-cursed-500/10 border-cursed-500/50'
                                    : 'bg-ink-950/40 border-ink-800 hover:border-ink-700'}">
                      <input type="radio" bind:group={torMode} value="transparent"
                             class="mt-1 w-4 h-4 accent-cursed-500" />
                      <div class="space-y-1 flex-1 min-w-0">
                        <div class="text-zinc-200 text-sm">All traffic via Tor</div>
                        <p class="text-[11px] text-zinc-500 leading-relaxed">
                          Every TCP connection from the Pi + USB clients goes
                          through Tor (except i2pd's — that always stays
                          independent). Strongest privacy, slowest browsing,
                          many sites break (CAPTCHA loops, geo-blocks).
                        </p>
                      </div>
                    </label>
                  </div>
                </div>

                <!-- Over-VPN -->
                <label class="flex items-start gap-3 cursor-pointer pt-2">
                  <input type="checkbox" bind:checked={torOverVpn}
                         disabled={!vpnEnabled || vpnProvider === 'none'}
                         class="mt-1 w-4 h-4 accent-cursed-500" />
                  <div class="space-y-0.5">
                    <span class="text-zinc-300 text-sm">
                      Nest Tor through the active VPN
                    </span>
                    <p class="text-[11px] text-zinc-500 leading-relaxed">
                      Tor's circuit handshakes with entry guards exit through
                      the VPN tunnel rather than the bare upstream — your ISP
                      sees only VPN traffic. Requires a clearnet VPN to be on
                      ({vpnEnabled && vpnProvider !== 'none'
                        ? `currently: ${vpnProvider}`
                        : 'currently none — toggle locked'}).
                    </p>
                  </div>
                </label>

                <!-- v77: Tor-over-VPN compatibility note. Hard-won across
                     sessions: Tor only bootstraps through a STEALTH OpenVPN
                     tunnel (AirVPN SSL/SSH). WireGuard can't reliably carry
                     Tor, and commercial-VPN exit IPs are widely Tor-blacklisted.
                     Amber when the active provider is a WireGuard/commercial one. -->
                <div class="ml-7 p-2.5 rounded border text-[11px] leading-relaxed
                            {['mullvad','ivpn','wireguard','azirevpn','tailscale'].includes(vpnProvider)
                              ? 'border-amber-500/40 bg-amber-500/10 text-amber-200/90'
                              : 'border-cursed-500/30 bg-cursed-500/5 text-zinc-400'}">
                  <span class="font-medium text-zinc-200">Tor-over-VPN works only over a stealth OpenVPN tunnel.</span>
                  Use <strong>AirVPN's OpenVPN-over-SSL or -SSH</strong> (or a similar custom <code>.ovpn</code>).
                  WireGuard fundamentally struggles to carry Tor traffic, and most commercial-VPN
                  exit-IP ranges are blacklisted by the Tor network — so transparent Tor over
                  WireGuard providers (Mullvad / IVPN / AirVPN-WireGuard) usually won't bootstrap
                  (the watchdog auto-reverts it if it stalls).
                  {#if ['mullvad','ivpn','wireguard','azirevpn','tailscale'].includes(vpnProvider)}
                    <span class="block mt-1">⚠ Your active VPN (<code>{vpnProvider}</code>) is WireGuard / commercial-exit — Tor likely won't bootstrap. Switch to AirVPN SSL/SSH first.</span>
                  {:else if vpnProvider === 'airvpn'}
                    <span class="block mt-1">✓ AirVPN active — make sure it's an <strong>SSL</strong> or <strong>SSH</strong> stealth mode (not WireGuard) for Tor.</span>
                  {/if}
                </div>

                <!-- v73: Tor bridge preset + custom bridges — moved here from
                     the legacy VPN "Tor" provider. Tor is no longer a VPN
                     provider, so this is the single place to configure it.
                     Leave "direct" on open networks; obfs4/meek/snowflake help
                     where Tor is blocked. Dimmed with the rest when Tor is off. -->
                <div class="space-y-2 pt-3 border-t border-ink-800" role="radiogroup" aria-label="Tor bridge preset">
                  <p class="text-[11px] uppercase tracking-wider text-zinc-500">Bridge preset</p>
                  {#if vpnState?.tor.presets}
                    {#each vpnState.tor.presets as p}
                      <label class="flex items-start gap-3 cursor-pointer">
                        <input type="radio" bind:group={torPreset} value={p.id}
                               class="mt-1 w-4 h-4 accent-cursed-500" />
                        <div class="space-y-1 min-w-0">
                          <div class="text-zinc-200 text-sm font-medium">{p.label}</div>
                          <p class="text-[11px] text-zinc-500">{p.blurb}</p>
                        </div>
                      </label>
                    {/each}
                  {/if}
                </div>
                {#if torPreset === 'custom'}
                  <div class="space-y-1 pt-2">
                    <label class="text-[11px] uppercase tracking-wider text-zinc-500 block" for="tor-br-ov">
                      Custom bridge lines
                      {#if vpnState?.tor.has_bridges}
                        <span class="text-cursed-400 normal-case ml-1 text-[10px]">(saved — paste to replace, blank keeps)</span>
                      {/if}
                    </label>
                    <textarea id="tor-br-ov" bind:value={torBridges} rows="4"
                              placeholder="obfs4 12.34.56.78:443 FINGERPRINT cert=… iat-mode=0"
                              class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-2 text-xs text-zinc-200 font-mono"></textarea>
                    <p class="text-[11px] text-zinc-500">
                      Fresh bridges from
                      <a class="text-cursed-300 hover:underline" href="https://bridges.torproject.org/" target="_blank" rel="noreferrer">bridges.torproject.org</a> — one per line. Most users won't need this.
                    </p>
                  </div>
                {/if}
              </div>
            </div>

            <!-- ── I2P ── -->
            <div class="space-y-3 p-4 rounded-lg border border-ink-700 bg-ink-950/30">
              <label class="flex items-center gap-3 cursor-pointer">
                <input type="checkbox" bind:checked={i2pEnabled}
                       class="w-4 h-4 accent-cursed-500" />
                <span class="text-zinc-200 text-sm font-medium">Enable I2P</span>
                <span class="text-[10px] uppercase tracking-wider text-zinc-500">
                  • HTTP proxy 4444 • SOCKS 4447
                </span>
                <a href="/network/i2p" class="ml-auto text-[10px] px-2 py-1 rounded
                          border border-cursed-500/40 text-cursed-300
                          hover:bg-cursed-500/10 transition-colors font-mono">
                  I2P config →
                </a>
              </label>

              <div class="space-y-3 pl-7"
                   class:opacity-40={!i2pEnabled}
                   class:pointer-events-none={!i2pEnabled}>
                <p class="text-[11px] text-zinc-500 leading-relaxed">
                  I2P is always split-tunnel by design — it's a peer-to-peer
                  overlay, not a generic transport. <code>.i2p</code>
                  sites work in any browser pointed at the HTTP proxy;
                  clearnet stays on whatever route the rest of the system
                  uses (Tor / VPN / direct).
                </p>

                <!-- Over-VPN -->
                <label class="flex items-start gap-3 cursor-pointer pt-1">
                  <input type="checkbox" bind:checked={i2pOverVpn}
                         disabled={!vpnEnabled || vpnProvider === 'none'}
                         class="mt-1 w-4 h-4 accent-cursed-500" />
                  <div class="space-y-0.5">
                    <span class="text-zinc-300 text-sm">
                      Nest I2P through the active VPN
                    </span>
                    <p class="text-[11px] text-zinc-500 leading-relaxed">
                      i2pd's outbound TCP rides the VPN tunnel instead of
                      going direct. Hides "uses I2P" from your ISP at the
                      cost of one extra hop. Requires a clearnet VPN to be on
                      ({vpnEnabled && vpnProvider !== 'none'
                        ? `currently: ${vpnProvider}`
                        : 'currently none — toggle locked'}).
                    </p>
                  </div>
                </label>
              </div>
            </div>

            <!-- v90: the Tailscale config block moved OUT of this panel
                 into its own top-level section (placed after the VPN
                 section). This panel is Tor + I2P only again. -->

            <!-- v73: the overlay section owns Tor + I2P end-to-end, so it
                 gets its own Save & Apply. saveVpn() persists both overlay
                 toggles + Tor's bridge preset/bridges/mode/exit-country
                 (and the VPN + Tailscale config too — it's one atomic
                 network save). -->
            <div class="flex items-center gap-3 pt-3 border-t border-ink-700">
              <button class="btn-primary" on:click={saveVpn} disabled={vpnSaving}>
                {vpnSaving ? 'saving…' : 'Save & Apply'}
              </button>
              {#if vpnMsg}<span class="text-xs text-live-300">{vpnMsg}</span>{/if}
            </div>
          </section>
        </div>
      </details>

      <!-- ──────────────────────────────────────────────────────────── -->
      <!-- Advanced network — firewall, NAT, port-forward, etc.          -->
      <!-- Big rules editor lives in a sub-component imported below.     -->
      <!-- v62: matches the other sections' expand-button affordance.   -->
      <!-- ──────────────────────────────────────────────────────────── -->
      <details class="bg-ink-900 border border-ink-700 rounded-xl"
               bind:open={advancedOpen}>
        <summary class="cursor-pointer select-none px-6 py-4
                        flex items-center justify-between gap-3">
          <div class="flex items-center gap-3 min-w-0">
            <span class="h-2 w-2 rounded-full flex-shrink-0 bg-zinc-600"></span>
            <span class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              Advanced Network — Firewall + NAT + Port-Forwarding
            </span>
            <span class="text-xs text-zinc-500 truncate">
              user rules + system defaults
            </span>
          </div>
          <span class="text-[10px] font-mono uppercase tracking-wider text-zinc-500 flex-shrink-0">
            {advancedOpen ? '▾ close' : '▸ expand'}
          </span>
        </summary>
        <div class="border-t border-ink-700">
          {#if advancedOpen}
            <RulesEditor />
          {/if}
        </div>
      </details>

      <section class="text-xs text-zinc-500">
        <p class="mb-2">
          Once enabled and a host connects:
        </p>
        <ul class="ml-4 list-disc space-y-1">
          <li>The host sees a USB-C ethernet adapter (CDC ECM)</li>
          <li>
            Web UI reachable at <code>https://{usbState.pi_addr}/</code>
            over USB <em>unless</em> in restricted mode (then only over
            WiFi/LAN)
          </li>
          <li>Persona selector continues to work — re-enumerating the gadget temporarily drops the USB ethernet (~1s)</li>
          <li>Toggling <strong>enabled</strong> requires an <code>aeon-hid</code> restart (gadget composite needs rebuilding)</li>
        </ul>
      </section>

      <TipJar />
    {/if}
    </div>
  </main>
</div>
