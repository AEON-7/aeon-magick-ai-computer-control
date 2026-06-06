<script lang="ts">
  // API token management. List existing tokens, create new ones, revoke.
  // The plaintext token is shown ONCE — only after creation. After that,
  // only the argon2 hash remains on disk.

  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import TipJar from '$lib/components/TipJar.svelte';
  import LockdownPanel from '$lib/components/LockdownPanel.svelte';

  let tokens: api.Token[] = [];
  let loading = true;
  let error = '';

  // Create-token form
  let newName = '';
  let newScope: 'admin' | 'full' | 'macros' | 'read' = 'full';
  let creating = false;
  let lastCreated: api.NewTokenResponse | null = null;
  let copied = false;

  async function refresh() {
    loading = true;
    error = '';
    try {
      const r = await api.listTokens();
      tokens = r.tokens.sort((a, b) => b.created_at_ms - a.created_at_ms);
    } catch (e: any) {
      error = e?.message ?? 'failed to load tokens';
    } finally {
      loading = false;
    }
  }

  onMount(refresh);

  async function create() {
    if (!newName.trim()) return;
    creating = true;
    error = '';
    try {
      lastCreated = await api.createToken(newName.trim(), newScope);
      newName = '';
      newScope = 'full';
      copied = false;
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to create token';
    } finally {
      creating = false;
    }
  }

  async function copyToken() {
    if (!lastCreated) return;
    try {
      await navigator.clipboard.writeText(lastCreated.token);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch (e) {
      console.warn('clipboard failed', e);
    }
  }

  async function revoke(id: string, name: string) {
    if (!confirm(`Revoke token "${name}"? Any agent using it will lose access immediately.`)) {
      return;
    }
    try {
      await api.revokeToken(id);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'revoke failed';
    }
  }

  function fmtTime(ms: number | null): string {
    if (!ms) return '—';
    const d = new Date(ms);
    return d.toLocaleString();
  }

  function scopeDescription(scope: string): string {
    switch (scope) {
      case 'admin':
        return 'Full + token management + password change';
      case 'full':
        return 'All HID, macros, snapshots — no token mgmt';
      case 'macros':
        return 'Run stored macros + read state/snapshots';
      case 'read':
        return 'Read state, snapshots, list macros/prompts';
      default:
        return scope;
    }
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← AEON MAGICK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">API tokens</span>
    </div>
  </header>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-4xl mx-auto w-full space-y-6">
      <LockdownPanel />
    <!-- New token creation -->
    <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
      <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
        Issue a new token
      </h2>

      <form on:submit|preventDefault={create} class="flex flex-col gap-3">
        <div class="flex gap-3 flex-wrap">
          <label class="flex-1 min-w-[200px]">
            <span class="text-xs uppercase tracking-wider text-zinc-500">name</span>
            <input
              type="text"
              bind:value={newName}
              placeholder="e.g. claude-desktop, cron-bot"
              class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-ink-700
                     focus:outline-none focus:ring-2 focus:ring-cursed-500"
            />
          </label>

          <label class="min-w-[160px]">
            <span class="text-xs uppercase tracking-wider text-zinc-500">scope</span>
            <select
              bind:value={newScope}
              class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-ink-700
                     focus:outline-none focus:ring-2 focus:ring-cursed-500"
            >
              <option value="full">full</option>
              <option value="macros">macros</option>
              <option value="read">read</option>
              <option value="admin">admin</option>
            </select>
          </label>

          <button
            type="submit"
            disabled={creating || !newName.trim()}
            class="btn-primary self-end disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {creating ? 'issuing…' : 'issue token'}
          </button>
        </div>

        <p class="text-xs text-zinc-500">
          <strong class="text-zinc-300">{newScope}</strong> —
          {scopeDescription(newScope)}
        </p>
      </form>

      {#if lastCreated}
        <div class="bg-cursed-900/30 border border-cursed-500/40 rounded-md p-4 space-y-2">
          <p class="text-xs uppercase tracking-wider text-cursed-300">
            new token — shown once, copy it now
          </p>
          <div class="flex items-center gap-2">
            <code
              class="flex-1 font-mono text-sm break-all text-zinc-100 bg-ink-950 px-3 py-2 rounded"
              >{lastCreated.token}</code
            >
            <button
              type="button"
              on:click={copyToken}
              class="btn whitespace-nowrap"
            >
              {copied ? 'copied!' : 'copy'}
            </button>
          </div>
          <p class="text-xs text-zinc-400">
            id: {lastCreated.id} · scope: {lastCreated.scope} · name: {lastCreated.name}
          </p>
          <p class="text-xs text-zinc-500">
            Use as <code>Authorization: Bearer {lastCreated.token.slice(0, 18)}…</code>
            or <code>X-Aeon-Token</code> header.
          </p>
        </div>
      {/if}
    </section>

    <!-- Existing tokens -->
    <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
      <div class="flex items-center justify-between">
        <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
          Active tokens
        </h2>
        <button class="btn text-xs" on:click={refresh}>refresh</button>
      </div>

      {#if loading}
        <p class="text-zinc-500 text-sm">loading…</p>
      {:else if error}
        <p class="text-red-400 text-sm">{error}</p>
      {:else if tokens.length === 0}
        <p class="text-zinc-500 text-sm italic">no tokens issued yet.</p>
      {:else}
        <div class="overflow-x-auto">
          <table class="w-full text-sm">
            <thead class="text-xs uppercase text-zinc-500 tracking-wider">
              <tr class="border-b border-ink-700">
                <th class="text-left py-2 pr-3 font-normal">name</th>
                <th class="text-left py-2 pr-3 font-normal">id</th>
                <th class="text-left py-2 pr-3 font-normal">scope</th>
                <th class="text-left py-2 pr-3 font-normal">created</th>
                <th class="text-left py-2 pr-3 font-normal">last used</th>
                <th class="py-2"></th>
              </tr>
            </thead>
            <tbody>
              {#each tokens as t (t.id)}
                <tr class="border-b border-ink-800 hover:bg-ink-800/40">
                  <td class="py-2 pr-3 font-mono">{t.name}</td>
                  <td class="py-2 pr-3 font-mono text-xs text-zinc-500">{t.id}</td>
                  <td class="py-2 pr-3 font-mono text-xs">
                    <span class="px-2 py-0.5 rounded bg-ink-800 border border-ink-700">
                      {t.scope}
                    </span>
                  </td>
                  <td class="py-2 pr-3 text-xs text-zinc-400">{fmtTime(t.created_at_ms)}</td>
                  <td class="py-2 pr-3 text-xs text-zinc-400">{fmtTime(t.last_used_at_ms)}</td>
                  <td class="py-2 text-right">
                    <button
                      class="text-red-400 hover:text-red-300 text-xs"
                      on:click={() => revoke(t.id, t.name)}>revoke</button
                    >
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </section>

    <section class="text-xs text-zinc-500 space-y-1">
      <p>Scopes:</p>
      <ul class="ml-4 list-disc space-y-1">
        <li><strong>admin</strong> — everything including issuing new tokens and changing passwords</li>
        <li><strong>full</strong> — all HID input, macros, snapshots; cannot manage tokens</li>
        <li><strong>macros</strong> — run pre-stored macros + read snapshots; no raw HID</li>
        <li><strong>read</strong> — read-only: state, snapshots, list macros/prompts</li>
      </ul>
    </section>

    <TipJar />
    </div>
  </main>
</div>
