<script lang="ts">
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { onMount, onDestroy } from 'svelte';
  import { confirmRite } from '$lib/confirm';

  type Service = { id: string; nickname: string; local_port: number; virt_port: number; onion: string };
  type Status = { ok: boolean; enabled: boolean; tor: string; count: number; services: Service[] };

  let status: Status | null = null;
  let err = '';
  let busy = '';
  let poll: ReturnType<typeof setInterval>;

  let nickname = '';
  let localPort: number | null = null;
  let virtPort = 80;
  let copied = '';

  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/onions${path}`, { credentials: 'same-origin', ...opts }).then((r) => r.json());

  async function load() {
    try {
      status = await api('/status');
      err = '';
    } catch (e: any) {
      err = e?.message ?? 'failed to load';
    }
  }

  async function enable() {
    busy = 'Starting Tor…';
    try { await api('/enable', { method: 'POST' }); } catch {}
    busy = ''; await load();
  }
  async function disable() {
    busy = 'Stopping…';
    try { await api('/disable', { method: 'POST' }); } catch {}
    busy = ''; await load();
  }

  async function create() {
    if (!localPort) { err = 'Local port is required'; return; }
    err = ''; busy = 'Minting onion…';
    try {
      const r = await api('/create', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ nickname, local_port: localPort, virt_port: virtPort }),
      });
      if (r && r.ok === false) err = typeof r.err === 'string' ? r.err : JSON.stringify(r.err);
      else { nickname = ''; localPort = null; virtPort = 80; }
    } catch (e: any) {
      err = e?.message ?? 'create failed';
    }
    busy = ''; await load();
  }

  async function remove(id: string) {
    if (!(await confirmRite({
      title: 'Retire hidden service',
      body: 'Retire this hidden service? Its .onion address is gone for good.',
      danger: true,
      confirmLabel: 'retire',
    }))) return;
    busy = 'Removing…';
    try {
      await api('/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id }),
      });
    } catch {}
    busy = ''; await load();
  }

  async function copy(text: string, id: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = id;
      setTimeout(() => (copied = ''), 1200);
    } catch {}
  }

  onMount(() => {
    load();
    poll = setInterval(load, 4000);
  });
  onDestroy(() => clearInterval(poll));
</script>

<div class="min-h-screen bg-ink-950 text-ink-100">
  <PageHeader title="Tor hidden services" backHref="/orbnet" backLabel="ORBNET" />

  <main class="max-w-3xl mx-auto px-5 py-6 space-y-6">
    <p class="text-ink-300 text-sm leading-relaxed">
      Host Tor <span class="text-cursed-300">v3 .onion</span> services — one address per site or app. Each maps an
      onion port to a local <code class="text-cursed-300">127.0.0.1:&lt;port&gt;</code>; share the address and anyone with
      Tor Browser can reach it. No port-forwarding, no exposed IP. Agents can mint these too.
    </p>

    {#if err}
      <div class="rounded border border-red-700 bg-red-950/40 text-red-300 px-3 py-2 text-sm">{err}</div>
    {/if}

    <div class="flex items-center gap-3">
      <span
        class="font-mono text-[10px] uppercase tracking-wider px-2 py-1 rounded {status?.enabled
          ? 'bg-emerald-900/50 text-emerald-300'
          : 'bg-ink-800 text-ink-400'}"
      >{status?.enabled ? 'enabled' : 'off'}</span>
      <span class="text-ink-400 text-xs">Tor: {status?.tor ?? '…'} · {status?.count ?? 0} hosted</span>
      <div class="flex-1"></div>
      {#if status?.enabled}
        <button class="text-xs text-ink-400 hover:text-ink-200" on:click={disable} disabled={!!busy}>Disable</button>
      {:else}
        <button class="text-xs text-cursed-300 hover:text-cursed-200" on:click={enable} disabled={!!busy}>Enable</button>
      {/if}
    </div>

    <div class="rounded-lg border border-ink-700 bg-ink-900 p-4 space-y-3">
      <h2 class="font-mono text-cursed-300 text-sm">Host a new service</h2>
      <div class="grid grid-cols-1 sm:grid-cols-3 gap-3">
        <label class="text-xs text-ink-400 block">Nickname
          <input class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1 text-ink-100" bind:value={nickname} placeholder="my site" />
        </label>
        <label class="text-xs text-ink-400 block">Local port
          <input type="number" min="1" max="65535" class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1 text-ink-100" bind:value={localPort} placeholder="8080" />
        </label>
        <label class="text-xs text-ink-400 block">Onion port
          <input type="number" min="1" max="65535" class="mt-1 w-full bg-ink-800 border border-ink-600 rounded px-2 py-1 text-ink-100" bind:value={virtPort} />
        </label>
      </div>
      <button
        class="font-mono text-sm px-3 py-1.5 rounded bg-cursed-700 hover:bg-cursed-600 text-white disabled:opacity-50"
        on:click={create}
        disabled={!!busy}
      >{busy === 'Minting onion…' ? busy : '🧅 Mint onion'}</button>
    </div>

    <div class="space-y-2">
      {#if status && status.services.length}
        {#each status.services as s (s.id)}
          <div class="rounded-lg border border-ink-700 bg-ink-900 p-3">
            <div class="flex items-center justify-between gap-2">
              <div class="font-mono text-sm text-ink-100">{s.nickname || s.id}</div>
              <button class="text-xs text-ink-500 hover:text-red-400" on:click={() => remove(s.id)}>retire</button>
            </div>
            <div class="mt-1 flex items-center gap-2">
              <code class="text-cursed-300 text-xs break-all">{s.onion || 'pending…'}</code>
              {#if s.onion}
                <button class="text-xs text-ink-400 hover:text-cursed-300 shrink-0" on:click={() => copy(s.onion, s.id)}>{copied === s.id ? '✓ copied' : 'copy'}</button>
              {/if}
            </div>
            <div class="mt-1 text-xs text-ink-500">onion :{s.virt_port} → 127.0.0.1:{s.local_port}</div>
          </div>
        {/each}
      {:else}
        <div class="text-ink-500 text-sm text-center py-6">No hidden services yet — host your first above.</div>
      {/if}
    </div>
  </main>
</div>
