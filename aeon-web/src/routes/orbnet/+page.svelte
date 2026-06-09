<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  let onions: { enabled: boolean; count: number } | null = null;
  let ipfs: { enabled: boolean; daemon: string; peers: number } | null = null;
  let myst: { enabled: boolean; daemon: string; registration: string; mmn_linked: boolean } | null = null;
  let poll: ReturnType<typeof setInterval>;

  const get = (p: string) =>
    fetch(p, { credentials: 'same-origin' }).then((r) => r.json()).catch(() => null);

  const load = async () => {
    onions = await get('/api/onions/status');
    ipfs = await get('/api/ipfs/status');
    myst = await get('/api/mysterium/status');
  };

  onMount(() => {
    load();
    poll = setInterval(load, 5000);
  });
  onDestroy(() => clearInterval(poll));

  $: services = [
    {
      key: 'onions', icon: '🧅', name: 'Hidden Services', href: '/orbnet/onions', ready: true,
      tagline: 'Host Tor .onion sites & apps — one address per service. Agents can mint them too.',
      status: onions ? (onions.enabled ? `on · ${onions.count} hosted` : 'off') : '…',
    },
    {
      key: 'ipfs', icon: '📦', name: 'IPFS', href: '/orbnet/ipfs', ready: true,
      tagline: 'Decentralized file & site hosting + a gateway for all your devices.',
      status: ipfs ? (ipfs.enabled ? (ipfs.daemon === 'active' ? `on · ${ipfs.peers} peers` : 'starting…') : 'off') : '…',
    },
    {
      key: 'mysterium', icon: '🌐', name: 'Mysterium', href: '/orbnet/mysterium', ready: true,
      tagline: 'Earn by sharing your bandwidth and helping decentralize internet access.',
      status: myst
        ? myst.enabled
          ? myst.daemon === 'active'
            ? myst.registration === 'Registered' ? 'on · earning' : myst.mmn_linked ? 'on · linked' : 'on · setup'
            : 'starting…'
          : 'off'
        : '…',
    },
    {
      key: 'chat', icon: '💬', name: 'Matrix Chat', href: '/orbnet/chat', ready: true,
      tagline: 'Private chat with friends + agents over Tor (experimental).',
      status: 'parked',
    },
  ];
</script>

<div class="min-h-screen bg-ink-950 text-ink-100">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
    <h1 class="font-mono text-lg text-cursed-300">🔮 OrbNet</h1>
    <div class="w-32"></div>
  </header>

  <main class="max-w-3xl mx-auto px-5 py-6 space-y-5">
    <p class="text-ink-300 text-sm">
      Opt-in decentralized services hosted on your Orb. Enable what you want — each runs on its own.
    </p>

    <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
      {#each services as s (s.key)}
        <a
          href={s.ready ? s.href : undefined}
          class="block rounded-lg border border-ink-700 bg-ink-900 p-4 transition {s.ready
            ? 'hover:border-cursed-600 cursor-pointer'
            : 'opacity-60 cursor-default'}"
        >
          <div class="flex items-center justify-between">
            <div class="text-2xl">{s.icon}</div>
            <span
              class="font-mono text-[10px] uppercase tracking-wider px-2 py-0.5 rounded {s.status.startsWith('on')
                ? 'bg-emerald-900/50 text-emerald-300'
                : 'bg-ink-800 text-ink-400'}"
            >{s.status}</span>
          </div>
          <div class="mt-2 font-mono text-cursed-300">{s.name}</div>
          <div class="mt-1 text-xs text-ink-400 leading-relaxed">{s.tagline}</div>
        </a>
      {/each}
    </div>
  </main>
</div>
