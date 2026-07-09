<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import Icon from '$lib/components/Icon.svelte';

  // One combined /network/vpn read carries the VPN + Tor + I2P + Tailscale toggles;
  // the rest are the per-service status endpoints. Everything's best-effort so the
  // hub still renders if one service is down.
  let vpn: any = null; // /network/vpn — vpn.enabled + tor/i2p/tailscale.enabled
  let dns: any = null; // /network/dnscrypt — encrypted DNS
  let onions: any = null;
  let ipfs: any = null;
  let myst: any = null;
  let orbnet: any = null; // /orbnet/status — Matrix homeserver
  let poll: ReturnType<typeof setInterval>;

  const get = (p: string) =>
    fetch(p, { credentials: 'same-origin' }).then((r) => r.json()).catch(() => null);

  const load = async () => {
    [vpn, dns, onions, ipfs, myst, orbnet] = await Promise.all([
      get('/api/network/vpn'),
      get('/api/network/dnscrypt'),
      get('/api/onions/status'),
      get('/api/ipfs/status'),
      get('/api/mysterium/status'),
      get('/api/orbnet/status'),
    ]);
  };

  onMount(() => {
    load();
    poll = setInterval(load, 5000);
  });
  onDestroy(() => clearInterval(poll));

  // Indicator lights — every privacy / network / decentralized feature with its
  // live enabled state. Each is also a shortcut to where it's configured.
  const GROUPS = ['Privacy & network', 'Hosting & decentralized'] as const;
  $: indicators = [
    {
      group: GROUPS[0],
      name: 'Encrypted DNS',
      icon: 'dns',
      tech: 'DNSCrypt',
      on: !!dns?.enabled,
      status: dns?.enabled ? dns.provider || 'on' : 'off',
      href: '/network',
    },
    {
      group: GROUPS[0],
      name: 'VPN',
      icon: 'vpn',
      tech: 'WireGuard / OpenVPN',
      on: !!(vpn?.enabled && vpn.provider && vpn.provider !== 'none'),
      status:
        vpn?.enabled && vpn.provider && vpn.provider !== 'none' ? vpn.provider : 'off',
      href: '/network',
    },
    {
      group: GROUPS[0],
      name: 'Tor',
      icon: 'onion',
      tech: 'onion routing',
      on: !!vpn?.tor?.enabled,
      status: vpn?.tor?.enabled
        ? vpn.tor.mode === 'transparent'
          ? 'transparent'
          : 'on'
        : 'off',
      href: '/network',
    },
    {
      group: GROUPS[0],
      name: 'I2P',
      icon: 'tunnel',
      tech: 'garlic routing',
      on: !!vpn?.i2p?.enabled,
      status: vpn?.i2p?.enabled ? 'on' : 'off',
      href: '/network/i2p',
    },
    {
      group: GROUPS[0],
      name: 'Tailscale',
      icon: 'mesh',
      tech: 'WireGuard mesh',
      on: !!vpn?.tailscale?.enabled,
      status: vpn?.tailscale?.enabled ? 'mesh on' : 'off',
      href: '/network',
    },
    {
      group: GROUPS[1],
      name: 'Hidden Services',
      icon: 'onion',
      tech: 'Tor v3 .onion',
      on: !!onions?.enabled,
      status: onions?.enabled ? `${onions.count ?? 0} hosted` : 'off',
      href: '/orbnet/onions',
    },
    {
      group: GROUPS[1],
      name: 'IPFS',
      icon: 'ipfs',
      tech: 'content-addressed',
      on: !!ipfs?.enabled,
      status: ipfs?.enabled
        ? ipfs.daemon === 'active'
          ? `${ipfs.peers ?? 0} peers`
          : ipfs.daemon || 'starting'
        : 'off',
      href: '/orbnet/ipfs',
    },
    {
      group: GROUPS[1],
      name: 'Mysterium',
      icon: 'mysterium',
      tech: 'dVPN node',
      on: !!myst?.enabled,
      status: myst?.enabled
        ? myst.daemon === 'active'
          ? 'earning'
          : 'starting'
        : 'off',
      href: '/orbnet/mysterium',
    },
    {
      group: GROUPS[1],
      name: 'Matrix Server',
      icon: 'matrix',
      tech: 'homeserver · Tor',
      on: !!orbnet?.enabled,
      status: orbnet?.enabled ? (orbnet.homeserver_up ? 'on' : 'starting') : 'off',
      href: '/orbnet/chat',
    },
  ];
  $: enabledCount = indicators.filter((i) => i.on).length;

  // Discovery cards — professional icon + protocol tag for instant recognition.
  const cards = [
    {
      icon: 'aether',
      name: 'Intergalactic Model Share',
      tech: 'IPFS · model library',
      href: '/model-share',
      tagline:
        'Decentralized AI-model network — browse, share, and pull models from every Orb with signed provenance.',
    },
    {
      icon: 'shield',
      name: 'Network & Privacy',
      tech: 'VPN · Tor · DNS · mesh',
      href: '/network',
      tagline:
        'Outbound privacy stack in one place — VPN, Tor, I2P, encrypted DNS, Tailscale mesh, and firewall.',
    },
    {
      icon: 'onion',
      name: 'Hidden Services',
      tech: 'Tor v3 · .onion',
      href: '/orbnet/onions',
      tagline:
        'Host Tor .onion sites and apps — one address per service. Agents can mint them too.',
    },
    {
      icon: 'ipfs',
      name: 'IPFS',
      tech: 'libp2p · CID gateway',
      href: '/orbnet/ipfs',
      tagline:
        'Local IPFS node and HTTP gateway — pin content, serve a fleet model library, open CIDs on any device.',
    },
    {
      icon: 'mysterium',
      name: 'Mysterium',
      tech: 'dVPN · bandwidth share',
      href: '/orbnet/mysterium',
      tagline:
        'Run a Mysterium node — earn by sharing bandwidth and helping decentralize access.',
    },
    {
      icon: 'matrix',
      name: 'Matrix Chat',
      tech: 'Conduit · Tor onion',
      href: '/orbnet/chat',
      tagline:
        'Private Matrix homeserver on this Orb (Conduit) — community rooms and E2EE DMs over Tor.',
    },
  ];
</script>

<div class="page-void min-h-screen">
  <PageHeader title="OrbNet" subtitle="decentralized services + privacy hub" index="03" />

  <main class="page-main-narrow">
    <div>
      <p class="rite-kicker">the mesh · the cloak · the archive</p>
      <p class="rite-lead">
        Everything your Orb runs for privacy and decentralized hosting.
        Enable what you want; each layer runs independently. Live state below.
      </p>
    </div>

    <!-- Indicator lights -->
    <section class="rack-section">
      <div class={enabledCount ? 'status-strip-live' : 'status-strip'} aria-hidden="true"></div>
      <div class="rack-section-head">
        <h2 class="rack-title">Status</h2>
        <span
          class="font-mono text-2xs uppercase tracking-instrument tabular-nums
                 {enabledCount ? 'text-live-400' : 'text-zinc-600'}"
        >
          {enabledCount} / {indicators.length} armed
        </span>
      </div>
      <div class="rack-section-body space-y-4">
        {#each GROUPS as g}
          <div class="space-y-1.5">
            <div class="rack-label">{g}</div>
            <div class="grid grid-cols-1 sm:grid-cols-2 gap-1.5">
              {#each indicators.filter((i) => i.group === g) as ind (ind.name)}
                <a
                  href={ind.href}
                  class={ind.on ? 'status-cell-on' : 'status-cell'}
                  title={ind.on
                    ? `${ind.name} is on — ${ind.status}`
                    : `${ind.name} is off — open to configure`}
                >
                  <span
                    class="inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-sm border
                           {ind.on
                             ? 'border-live-500/30 bg-live-500/10 text-live-400'
                             : 'border-steel-700 bg-ink-900 text-zinc-500'}"
                    aria-hidden="true"
                  >
                    <Icon name={ind.icon} class="w-3.5 h-3.5" />
                  </span>
                  <span class="min-w-0 flex-1">
                    <span class="flex items-center gap-1.5">
                      <span class="text-sm truncate {ind.on ? 'text-zinc-100' : 'text-zinc-400'}"
                        >{ind.name}</span
                      >
                      <span class={ind.on ? 'dot-live' : 'dot bg-steel-600'}></span>
                    </span>
                    <span class="block font-mono text-2xs text-zinc-600 truncate">{ind.tech}</span>
                  </span>
                  <span
                    class="font-mono text-2xs shrink-0 uppercase tracking-wide {ind.on
                      ? 'text-live-400'
                      : 'text-zinc-600'}"
                  >
                    {ind.status}
                  </span>
                </a>
              {/each}
            </div>
          </div>
        {/each}
      </div>
    </section>

    <!-- Explore cards -->
    <section class="space-y-3">
      <h2 class="rack-label">Explore</h2>
      <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
        {#each cards as c (c.href)}
          <a href={c.href} class="module-tile group">
            <span class="module-icon" aria-hidden="true">
              <Icon name={c.icon} class="w-5 h-5" />
            </span>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <span class="font-mono text-sm text-cursed-200 tracking-wide truncate">{c.name}</span>
                <span
                  class="ml-auto text-zinc-600 transition group-hover:text-cursed-300 shrink-0"
                  aria-hidden="true">→</span
                >
              </div>
              <div class="font-mono text-2xs uppercase tracking-instrument text-zinc-500 mt-0.5">
                {c.tech}
              </div>
              <div class="mt-2 text-xs text-zinc-400 leading-relaxed">{c.tagline}</div>
            </div>
          </a>
        {/each}
      </div>
    </section>
  </main>
</div>
