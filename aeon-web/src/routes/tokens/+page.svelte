<script lang="ts">
  // API token management. List existing tokens, create new ones, revoke.
  // The plaintext token is shown ONCE — only after creation. After that,
  // only the argon2 hash remains on disk.

  import PageHeader from '$lib/components/PageHeader.svelte';
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import TipJar from '$lib/components/TipJar.svelte';
  import LockdownPanel from '$lib/components/LockdownPanel.svelte';
  import { confirmRite } from '$lib/confirm';

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
    if (
      !(await confirmRite({
        title: 'Revoke token',
        body: `Revoke token "${name}"? Any agent using it will lose access immediately.`,
        danger: true,
        confirmLabel: 'revoke',
      }))
    ) {
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

  function scopePill(scope: string): string {
    switch (scope) {
      case 'admin':
        return 'pill-warn';
      case 'full':
        return 'pill-net';
      case 'macros':
        return 'pill-live';
      default:
        return 'pill-idle';
    }
  }
</script>

<div class="page-void h-full flex flex-col">
  <PageHeader title="API tokens" subtitle="keys for agents · REST · MCP" index="07" />

  <main class="page-main flex-1 overflow-auto">
    <p class="rite-lead">
      Mint scoped keys for every agent. Name them after the agent that will hold them —
      the audit log will thank you. Plaintext is shown <strong class="text-zinc-300">once</strong>.
    </p>

    <LockdownPanel />

    <section class="rack-section">
      <div class="status-strip-cursed" aria-hidden="true"></div>
      <div class="rack-section-head">
        <h2 class="rack-title">Issue a key</h2>
        <span class="rack-label">once · then gone</span>
      </div>
      <div class="rack-section-body">
        <form on:submit|preventDefault={create} class="flex flex-col gap-3">
          <div class="flex gap-3 flex-wrap">
            <label class="flex-1 min-w-[200px]">
              <span class="field-label">name</span>
              <input
                type="text"
                bind:value={newName}
                placeholder="e.g. claude-desktop, cron-bot"
                class="field"
              />
            </label>

            <label class="min-w-[160px]">
              <span class="field-label">scope</span>
              <select bind:value={newScope} class="field">
                <option value="full">full</option>
                <option value="macros">macros</option>
                <option value="read">read</option>
                <option value="admin">admin</option>
              </select>
            </label>

            <button
              type="submit"
              disabled={creating || !newName.trim()}
              class="btn-primary self-end"
            >
              {creating ? 'issuing…' : 'issue token'}
            </button>
          </div>

          <p class="text-xs text-zinc-500 font-mono">
            <span class="text-cursed-300">{newScope}</span>
            — {scopeDescription(newScope)}
          </p>
        </form>

        {#if lastCreated}
          <div class="callout-info space-y-2 !p-4">
            <p class="text-2xs uppercase tracking-instrument text-cursed-300">
              new token — shown once · copy it now
            </p>
            <div class="flex items-center gap-2">
              <code class="code-well flex-1">{lastCreated.token}</code>
              <button type="button" on:click={copyToken} class="btn whitespace-nowrap">
                {copied ? 'copied!' : 'copy'}
              </button>
            </div>
            <p class="text-2xs text-zinc-500">
              id: {lastCreated.id} · scope: {lastCreated.scope} · name: {lastCreated.name}
            </p>
            <p class="text-2xs text-zinc-600">
              Use as <code class="text-zinc-400">Authorization: Bearer …</code>
              or <code class="text-zinc-400">X-Aeon-Token</code>.
            </p>
          </div>
        {/if}
      </div>
    </section>

    <section class="rack-section">
      <div class="rack-section-head">
        <h2 class="rack-title">Active tokens</h2>
        <button class="btn btn-xs" on:click={refresh}>refresh</button>
      </div>
      <div class="rack-section-body !pt-0 !px-0 !pb-0">
        {#if loading}
          <p class="text-zinc-500 text-sm font-mono px-5 py-6">loading…</p>
        {:else if error}
          <p class="callout-fault m-4">{error}</p>
        {:else if tokens.length === 0}
          <div class="void-empty m-4">
            <p class="void-empty-title">no keys issued</p>
            <p class="void-empty-body">
              Mint a token above, name it after the agent, and hand it the Bearer secret once.
            </p>
          </div>
        {:else}
          <div class="overflow-x-auto">
            <table class="w-full text-sm">
              <thead class="text-2xs uppercase text-zinc-500 tracking-instrument font-mono">
                <tr class="border-b border-steel-700">
                  <th class="text-left py-2.5 px-4 font-normal">name</th>
                  <th class="text-left py-2.5 pr-3 font-normal">id</th>
                  <th class="text-left py-2.5 pr-3 font-normal">scope</th>
                  <th class="text-left py-2.5 pr-3 font-normal">created</th>
                  <th class="text-left py-2.5 pr-3 font-normal">last used</th>
                  <th class="py-2.5 px-4"></th>
                </tr>
              </thead>
              <tbody>
                {#each tokens as t (t.id)}
                  <tr class="rack-row">
                    <td class="py-2.5 px-4 font-mono text-zinc-200">{t.name}</td>
                    <td class="py-2.5 pr-3 font-mono text-2xs text-zinc-500">{t.id}</td>
                    <td class="py-2.5 pr-3">
                      <span class={scopePill(t.scope)}>{t.scope}</span>
                    </td>
                    <td class="py-2.5 pr-3 font-mono text-2xs text-zinc-500">{fmtTime(t.created_at_ms)}</td>
                    <td class="py-2.5 pr-3 font-mono text-2xs text-zinc-500">{fmtTime(t.last_used_ms)}</td>
                    <td class="py-2.5 px-4 text-right">
                      <button class="btn-danger btn-xs" on:click={() => revoke(t.id, t.name)}>
                        revoke
                      </button>
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </div>
    </section>

    <TipJar />
  </main>
</div>
