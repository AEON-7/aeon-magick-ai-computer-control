<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '$lib/api';

  let tab: 'overview' | 'systems' = 'overview';
  let pubkey = '';
  let systems: api.ConnectedSystem[] = [];
  let metrics: Record<string, api.SystemMetrics> = {};
  let loading = true;
  let metricsLoading = false;
  let err = '';

  // add-system form
  let label = '';
  let address = '';
  let sshUser = 'root';
  let port = 22;
  let roleOpenclaw = false;
  let roleHermes = false;
  let roleDgx = false;
  let adding = false;
  let busyId = '';
  let fallback: Record<string, string> = {};

  const inputCls =
    'bg-ink-900 border border-ink-700 rounded px-2 py-1 text-xs text-zinc-200 placeholder-zinc-600 focus:outline-none focus:border-cursed-500';

  async function refresh() {
    try {
      const [pk, sys] = await Promise.all([api.getAgentPubkey(), api.listSystems()]);
      pubkey = pk.pubkey ?? '';
      systems = sys.systems ?? [];
    } catch (e) {
      err = (e as any)?.message ?? String(e);
    } finally {
      loading = false;
    }
  }
  async function loadMetrics() {
    if (!systems.length) return;
    metricsLoading = true;
    await Promise.all(
      systems.map(async (s) => {
        try {
          const r = await api.getSystemMetrics(s.id);
          metrics[s.id] = r.metrics;
        } catch {
          metrics[s.id] = { reachable: false };
        }
      }),
    );
    metrics = metrics;
    metricsLoading = false;
  }
  onMount(async () => {
    await refresh();
    await loadMetrics();
  });

  function roles(): string[] {
    const r: string[] = [];
    if (roleOpenclaw) r.push('openclaw');
    if (roleHermes) r.push('hermes');
    if (roleDgx) r.push('dgx');
    return r;
  }
  async function onAdd() {
    if (!address.trim()) return;
    adding = true;
    try {
      const res = await api.addSystem({ label, address, ssh_user: sshUser, port, roles: roles() });
      if (!res.ok) {
        alert(res.err ?? 'add failed');
        return;
      }
      label = '';
      address = '';
      sshUser = 'root';
      port = 22;
      roleOpenclaw = roleHermes = roleDgx = false;
      await refresh();
    } finally {
      adding = false;
    }
  }
  async function onRegister(s: api.ConnectedSystem) {
    const pw = prompt(
      `One-time SSH password for ${s.ssh_user}@${s.address} — used ONCE to install the Pi's key, never stored. Leave blank if you'll authorize manually.`,
    );
    if (pw === null) return;
    busyId = s.id;
    delete fallback[s.id];
    fallback = fallback;
    try {
      const res = await api.registerSystem(s.id, pw);
      if (!res.ok && res.authorize_command) {
        fallback[s.id] = res.authorize_command;
        fallback = fallback;
      }
      await refresh();
    } finally {
      busyId = '';
    }
  }
  async function onTest(s: api.ConnectedSystem) {
    busyId = s.id;
    try {
      await api.testSystem(s.id);
      await refresh();
    } finally {
      busyId = '';
    }
  }
  async function onRemove(s: api.ConnectedSystem) {
    if (!confirm(`Remove ${s.label}?`)) return;
    await api.removeSystem(s.id);
    await refresh();
  }
  function copy(text: string) {
    navigator.clipboard?.writeText(text);
  }
  function badgeCls(status: string): string {
    if (status === 'connected' || status === 'online')
      return 'bg-live-900/40 text-live-300 border border-live-500/40';
    if (status === 'unreachable')
      return 'bg-red-900/30 text-red-300 border border-red-500/40';
    return 'bg-ink-800 text-zinc-400 border border-ink-700';
  }
  function tabCls(t: string): string {
    return tab === t
      ? 'bg-ink-900 text-cursed-300 border-x border-t border-ink-700'
      : 'text-zinc-500 hover:text-zinc-300 border border-transparent';
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">Agent Dash</span>
    </div>
  </header>

  <div class="flex gap-1 px-5 pt-2 border-b border-ink-800 bg-ink-900/40">
    <button class="px-3 py-1.5 text-xs font-mono rounded-t {tabCls('overview')}"
            on:click={() => { tab = 'overview'; loadMetrics(); }}>Overview</button>
    <button class="px-3 py-1.5 text-xs font-mono rounded-t {tabCls('systems')}"
            on:click={() => (tab = 'systems')}>Connected Systems</button>
  </div>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-3xl mx-auto w-full space-y-5">
      {#if loading}<p class="text-zinc-500 text-sm">loading…</p>{/if}
      {#if err}<p class="text-red-400 text-sm">{err}</p>{/if}

      {#if tab === 'overview'}
        <div class="flex items-center justify-between">
          <h2 class="font-mono text-xs uppercase tracking-wider text-cursed-300">Systems</h2>
          <button class="btn text-xs" on:click={loadMetrics} disabled={metricsLoading}>
            {metricsLoading ? 'refreshing…' : '↻ refresh'}
          </button>
        </div>
        {#if !systems.length}
          <p class="text-zinc-500 text-xs">No systems yet — add them in the <button class="underline text-cursed-300" on:click={() => (tab = 'systems')}>Connected Systems</button> tab.</p>
        {/if}
        {#each systems as s (s.id)}
          {@const m = metrics[s.id]}
          <div class="p-4 rounded-lg border border-ink-800 bg-ink-950/40 space-y-2">
            <div class="flex items-center justify-between gap-2">
              <div class="min-w-0">
                <p class="text-sm text-zinc-200 truncate">{s.label}</p>
                <p class="text-[10px] font-mono text-zinc-500">{s.roles.join(', ') || 'no role'} · {m?.host || s.address}</p>
              </div>
              <span class="text-[10px] font-mono uppercase px-2 py-0.5 rounded-full {m?.reachable ? badgeCls('online') : badgeCls(m ? 'unreachable' : '')}">
                {m ? (m.reachable ? 'online' : 'unreachable') : '…'}
              </span>
            </div>
            {#if m?.reachable}
              <div class="flex flex-wrap gap-x-4 gap-y-1 text-[10px] font-mono text-zinc-400">
                {#if m.load}<span><span class="text-zinc-600">load</span> {m.load}</span>{/if}
                {#if m.mem}<span><span class="text-zinc-600">mem</span> {m.mem} MB</span>{/if}
              </div>
              {#if m.gpus?.length}
                <div class="space-y-1.5 pt-1">
                  {#each m.gpus as g}
                    <div class="text-[10px] font-mono text-zinc-300 space-y-0.5">
                      <div class="flex justify-between"><span class="truncate">{g.name}</span><span class="text-cursed-300">{g.util}% · {g.temp}°C</span></div>
                      <div class="h-1.5 rounded bg-ink-800 overflow-hidden"><div class="h-full bg-cursed-500 transition-all" style="width:{g.util}%"></div></div>
                      <span class="text-zinc-600">{g.mem_used}/{g.mem_total} MB VRAM</span>
                    </div>
                  {/each}
                </div>
              {/if}
              {#if m.containers?.length}
                <p class="text-[10px] font-mono text-zinc-400"><span class="text-zinc-600">containers ({m.containers.length}):</span> {m.containers.join(', ')}</p>
              {/if}
            {:else if m}
              <p class="text-[10px] text-red-400">{m.err || 'unreachable'}</p>
            {/if}
          </div>
        {/each}
        {#if systems.length}
          <p class="text-[10px] text-zinc-600">Per-agent roster + token usage (your OpenClaw pantheon) is the next layer — needs the way you query OpenClaw's agents (`openclaw` isn't in albert's PATH on .155).</p>
        {/if}
      {/if}

      {#if tab === 'systems'}
        <section class="space-y-2 p-4 rounded-lg border border-ink-800 bg-ink-950/40">
          <h2 class="font-mono text-xs uppercase tracking-wider text-cursed-300">This device's agent-connect key</h2>
          <p class="text-xs text-zinc-400">The Pi installs <em>this</em> public key on each system so it has SSH key-auth for metrics + provisioning.</p>
          <code class="block text-[10px] font-mono text-zinc-300 bg-ink-900 rounded p-2 break-all">{pubkey || '—'}</code>
          <button class="btn text-xs" on:click={() => copy(pubkey)}>copy public key</button>
        </section>

        <section class="space-y-2 p-4 rounded-lg border border-ink-800 bg-ink-950/40">
          <h2 class="font-mono text-xs uppercase tracking-wider text-cursed-300">Add a connected system</h2>
          <div class="grid grid-cols-2 gap-2">
            <input class="{inputCls} col-span-2" placeholder="Label (e.g. OpenClaw)" bind:value={label} />
            <input class={inputCls} placeholder="Address (e.g. 192.168.1.155)" bind:value={address} />
            <div class="flex gap-2">
              <input class="{inputCls} flex-1" placeholder="ssh user" bind:value={sshUser} />
              <input class="{inputCls} w-20" type="number" placeholder="port" bind:value={port} />
            </div>
            <div class="col-span-2 flex items-center gap-4 text-xs text-zinc-300">
              <label class="flex items-center gap-1"><input type="checkbox" bind:checked={roleOpenclaw} /> OpenClaw</label>
              <label class="flex items-center gap-1"><input type="checkbox" bind:checked={roleHermes} /> Hermes</label>
              <label class="flex items-center gap-1"><input type="checkbox" bind:checked={roleDgx} /> DGX Spark</label>
            </div>
          </div>
          <button class="btn-primary text-xs" on:click={onAdd} disabled={adding || !address.trim()}>add system</button>
        </section>

        <section class="space-y-2">
          <h2 class="font-mono text-xs uppercase tracking-wider text-cursed-300">Connected systems</h2>
          {#if !systems.length}<p class="text-zinc-500 text-xs">No systems yet.</p>{/if}
          {#each systems as s (s.id)}
            <div class="p-3 rounded-lg border border-ink-800 bg-ink-950/40 space-y-2">
              <div class="flex items-center justify-between gap-2">
                <div class="min-w-0">
                  <p class="text-sm text-zinc-200 truncate">{s.label}</p>
                  <p class="text-[10px] font-mono text-zinc-500">{s.ssh_user}@{s.address}:{s.port} · {s.roles.join(', ') || 'no role'}</p>
                </div>
                <span class="text-[10px] font-mono uppercase px-2 py-0.5 rounded-full {badgeCls(s.status)}">{s.status || 'pending'}</span>
              </div>
              <div class="flex gap-2">
                <button class="btn text-xs" on:click={() => onRegister(s)} disabled={busyId === s.id}>register (SSH key)</button>
                <button class="btn text-xs" on:click={() => onTest(s)} disabled={busyId === s.id}>test</button>
                <button class="btn text-xs ml-auto text-red-300" on:click={() => onRemove(s)}>remove</button>
              </div>
              {#if fallback[s.id]}
                <div class="text-[10px] font-mono text-amber-300 space-y-1">
                  <p>Password auth unavailable — run this on {s.address}, then Test:</p>
                  <code class="block bg-ink-900 rounded p-2 break-all text-zinc-200">{fallback[s.id]}</code>
                </div>
              {/if}
            </div>
          {/each}
        </section>
      {/if}
    </div>
  </main>
</div>
