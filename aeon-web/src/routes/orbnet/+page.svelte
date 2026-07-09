<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';

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
  // live enabled state. Each is also a shortcut to where it's configured, so the
  // whole surface is discoverable from OrbNet even though it lives elsewhere too.
  const GROUPS = ['Privacy & network', 'Hosting & decentralized'] as const;
  $: indicators = [
    { group: GROUPS[0], name: 'Encrypted DNS', on: !!dns?.enabled, status: dns?.enabled ? (dns.provider || 'on') : 'off', href: '/network' },
    { group: GROUPS[0], name: 'VPN', on: !!(vpn?.enabled && vpn.provider && vpn.provider !== 'none'), status: vpn?.enabled && vpn.provider && vpn.provider !== 'none' ? vpn.provider : 'off', href: '/network' },
    { group: GROUPS[0], name: 'Tor', on: !!vpn?.tor?.enabled, status: vpn?.tor?.enabled ? (vpn.tor.mode === 'transparent' ? 'transparent' : 'on') : 'off', href: '/network' },
    { group: GROUPS[0], name: 'I2P', on: !!vpn?.i2p?.enabled, status: vpn?.i2p?.enabled ? 'on' : 'off', href: '/network/i2p' },
    { group: GROUPS[0], name: 'Tailscale', on: !!vpn?.tailscale?.enabled, status: vpn?.tailscale?.enabled ? 'mesh on' : 'off', href: '/network' },
    { group: GROUPS[1], name: 'Hidden Services', on: !!onions?.enabled, status: onions?.enabled ? `${onions.count ?? 0} hosted` : 'off', href: '/orbnet/onions' },
    { group: GROUPS[1], name: 'IPFS', on: !!ipfs?.enabled, status: ipfs?.enabled ? (ipfs.daemon === 'active' ? `${ipfs.peers ?? 0} peers` : ipfs.daemon || 'starting') : 'off', href: '/orbnet/ipfs' },
    { group: GROUPS[1], name: 'Mysterium', on: !!myst?.enabled, status: myst?.enabled ? (myst.daemon === 'active' ? 'earning' : 'starting') : 'off', href: '/orbnet/mysterium' },
    { group: GROUPS[1], name: 'Matrix Server', on: !!orbnet?.enabled, status: orbnet?.enabled ? (orbnet.homeserver_up ? 'on' : 'starting') : 'off', href: '/orbnet/chat' },
  ];
  $: enabledCount = indicators.filter((i) => i.on).length;

  // Discovery cards — the big entry points. Model Share + Network are surfaced here
  // on purpose (people look in OrbNet for them), next to the hosted services.
  const cards = [
    { icon: '🌌', name: 'Intergalactic Model Share', href: '/model-share',
      tagline: 'The decentralized, censorship-resistant AI-model network over IPFS — browse, share + pull models from every Orb, with signed provenance.' },
    { icon: '🛡️', name: 'Network & Privacy', href: '/network',
      tagline: 'Your outbound privacy stack in one place — VPN · Tor · I2P · encrypted DNS · Tailscale mesh + firewall.' },
    { icon: '🧅', name: 'Hidden Services', href: '/orbnet/onions',
      tagline: 'Host Tor .onion sites & apps — one address per service. Agents can mint them too.' },
    { icon: '📦', name: 'IPFS', href: '/orbnet/ipfs',
      tagline: 'Decentralized file & site hosting + a gateway for all your devices.' },
    { icon: '🌐', name: 'Mysterium', href: '/orbnet/mysterium',
      tagline: 'Earn by sharing your bandwidth and helping decentralize internet access.' },
    { icon: '💬', name: 'Matrix Chat', href: '/orbnet/chat',
      tagline: 'Private chat with friends + agents over Tor (experimental).' },
  ];
</script>

<div class="min-h-screen bg-ink-950 text-ink-100">
  <PageHeader title="OrbNet" subtitle="decentralized services + privacy hub" />

  <main class="max-w-3xl mx-auto px-5 py-6 space-y-6">
    <p class="text-ink-300 text-sm">
      Everything your Orb runs for you — private networking and decentralized hosting.
      Enable what you want; each runs independently. Here's what's on right now.
    </p>

    <!-- Indicator lights: every feature + its live enabled state -->
    <section class="rounded-xl border border-ink-700 bg-ink-900/60 p-4 sm:p-5 space-y-4">
      <div class="flex items-center justify-between">
        <h2 class="font-mono text-xs uppercase tracking-wider text-ink-400">Status</h2>
        <span class="font-mono text-[11px] {enabledCount ? 'text-emerald-300' : 'text-ink-500'}">
          {enabledCount} of {indicators.length} enabled
        </span>
      </div>
      {#each GROUPS as g}
        <div class="space-y-1.5">
          <div class="text-[10px] uppercase tracking-wider text-ink-600 font-mono">{g}</div>
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-1.5">
            {#each indicators.filter((i) => i.group === g) as ind (ind.name)}
              <a
                href={ind.href}
                class="flex items-center gap-2.5 rounded-lg border px-3 py-2 transition
                       {ind.on
                         ? 'border-emerald-600/40 bg-emerald-900/10 hover:border-emerald-500'
                         : 'border-ink-800 bg-ink-950/40 hover:border-ink-600'}"
                title={ind.on ? `${ind.name} is on — ${ind.status}` : `${ind.name} is off — click to enable`}
              >
                <span
                  class="inline-flex h-2.5 w-2.5 shrink-0 rounded-full
                         {ind.on ? 'bg-emerald-400 shadow-[0_0_6px_rgba(52,211,153,0.85)]' : 'bg-ink-600'}"
                ></span>
                <span class="text-sm flex-1 truncate {ind.on ? 'text-ink-100' : 'text-ink-400'}">{ind.name}</span>
                <span class="font-mono text-[10px] shrink-0 {ind.on ? 'text-emerald-300' : 'text-ink-600'}">{ind.status}</span>
              </a>
            {/each}
          </div>
        </div>
      {/each}
    </section>

    <!-- Explore: the big entry points, incl. Model Share + Network -->
    <section class="space-y-3">
      <h2 class="font-mono text-xs uppercase tracking-wider text-ink-400">Explore</h2>
      <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
        {#each cards as c (c.href)}
          <a
            href={c.href}
            class="group block rounded-lg border border-ink-700 bg-ink-900 p-4 transition hover:border-cursed-600 cursor-pointer"
          >
            <div class="flex items-center gap-2">
              <span class="text-2xl">{c.icon}</span>
              <span class="font-mono text-cursed-300">{c.name}</span>
              <span class="ml-auto text-ink-600 transition group-hover:text-cursed-300" aria-hidden="true">→</span>
            </div>
            <div class="mt-1.5 text-xs text-ink-400 leading-relaxed">{c.tagline}</div>
          </a>
        {/each}
      </div>
    </section>
  </main>
</div>
