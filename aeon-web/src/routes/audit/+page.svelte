<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let state: api.AuditState | null = null;
  let loading = true;
  let error = '';
  let poll_iv: ReturnType<typeof setInterval>;

  // Filters
  let filterActor = '';
  let filterAction = '';
  let limit = 200;

  async function refresh() {
    try {
      state = await api.getAudit({
        actor: filterActor || undefined,
        action: filterAction || undefined,
        limit,
      });
      loading = false;
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
      loading = false;
    }
  }

  onMount(() => {
    refresh();
    poll_iv = setInterval(refresh, 5000);
  });
  onDestroy(() => { if (poll_iv) clearInterval(poll_iv); });

  async function clearLog() {
    if (!confirm('Clear the entire audit log?\n\nThis cannot be undone — entries are not stored anywhere else.')) {
      return;
    }
    try {
      await api.clearAudit();
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'clear failed';
    }
  }

  function fmtTime(ms: number): string {
    if (!ms) return '—';
    const d = new Date(ms);
    return d.toLocaleString(undefined, {
      hour12: false,
      year: 'numeric', month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit', second: '2-digit',
    });
  }
  function fmtAgo(ms: number): string {
    const dt = Date.now() - ms;
    if (dt < 60_000) return 'just now';
    if (dt < 3_600_000) return `${Math.floor(dt / 60_000)} min ago`;
    if (dt < 86_400_000) return `${Math.floor(dt / 3_600_000)}h ago`;
    return `${Math.floor(dt / 86_400_000)}d ago`;
  }
  function fmtBytes(b: number): string {
    if (b < 1_000) return `${b} B`;
    if (b < 1_000_000) return `${(b / 1_000).toFixed(1)} kB`;
    return `${(b / 1_000_000).toFixed(2)} MB`;
  }

  // Colour each action category for fast visual scan.
  function actionClass(a: string): string {
    if (a.startsWith('login') && a.endsWith('_fail')) return 'bg-red-500/20 text-red-300 border-red-500/40';
    if (a.startsWith('login')) return 'bg-live-500/20 text-live-300 border-live-500/40';
    if (a === 'logout') return 'bg-ink-700 text-zinc-300 border-ink-600';
    if (a === 'password_set' || a === 'password_change') return 'bg-amber-500/15 text-amber-300 border-amber-500/40';
    if (a.startsWith('token_')) return 'bg-cursed-500/15 text-cursed-300 border-cursed-500/40';
    if (a === 'scope_denied') return 'bg-red-500/15 text-red-300 border-red-500/40';
    if (a.includes('delete')) return 'bg-red-500/10 text-red-300 border-red-500/30';
    if (a.startsWith('set_network')) return 'bg-cursed-500/10 text-cursed-200 border-cursed-500/30';
    if (a.startsWith('set_firewall')) return 'bg-fuchsia-500/15 text-fuchsia-300 border-fuchsia-500/40';
    if (a.startsWith('set_dns')) return 'bg-cursed-500/10 text-cursed-200 border-cursed-500/30';
    if (a.startsWith('set_ssh')) return 'bg-amber-500/10 text-amber-300 border-amber-500/30';
    if (a === 'hid_persona_set') return 'bg-cursed-500/10 text-cursed-200 border-cursed-500/30';
    return 'bg-ink-700 text-zinc-300 border-ink-600';
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← AEON MAGICK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">audit log</span>
    </div>
    <button class="btn text-xs hover:bg-red-500/20 hover:text-red-300 hover:border-red-500/40"
            on:click={clearLog}>
      clear log
    </button>
  </header>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-5xl mx-auto w-full space-y-6">
    {#if loading}<p class="text-zinc-500 text-sm">loading…</p>{/if}
    {#if error}<p class="text-red-400 text-sm">{error}</p>{/if}

    {#if state}
      <!-- Stats -->
      <section class="grid grid-cols-3 gap-3">
        <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Entries logged</div>
          <div class="text-2xl font-mono text-zinc-200 mt-1">{state.total_lines.toLocaleString()}</div>
        </div>
        <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Showing</div>
          <div class="text-2xl font-mono text-cursed-300 mt-1">{state.entries.length}</div>
          <div class="text-[10px] text-zinc-500 mt-1">filtered • newest first</div>
        </div>
        <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Log size</div>
          <div class="text-2xl font-mono text-zinc-300 mt-1">{fmtBytes(state.current_bytes)}</div>
          <div class="text-[10px] text-zinc-500 mt-1">
            rotates at {fmtBytes(state.max_bytes)}
          </div>
        </div>
      </section>

      <!-- Filters -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-4 space-y-3">
        <h2 class="font-mono text-xs uppercase tracking-wider text-zinc-400">Filters</h2>
        <div class="grid grid-cols-1 sm:grid-cols-3 gap-3 text-xs">
          <label class="space-y-1">
            <span class="text-zinc-500">Actor (exact)</span>
            <input type="text" bind:value={filterActor}
                   on:input={() => refresh()}
                   placeholder="admin (session)"
                   class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200 font-mono" />
          </label>
          <label class="space-y-1">
            <span class="text-zinc-500">Action (prefix)</span>
            <input type="text" bind:value={filterAction}
                   on:input={() => refresh()}
                   placeholder="login / token / set_network"
                   class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200 font-mono" />
          </label>
          <label class="space-y-1">
            <span class="text-zinc-500">Limit</span>
            <select bind:value={limit} on:change={() => refresh()}
                    class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200">
              <option value={50}>50</option>
              <option value={200}>200</option>
              <option value={500}>500</option>
              <option value={2000}>2000</option>
            </select>
          </label>
        </div>
      </section>

      <!-- Entry list -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl">
        <div class="max-h-[60vh] overflow-auto divide-y divide-ink-800">
          {#each state.entries as e (e.ts_ms + e.actor + e.action + e.detail)}
            <div class="p-3 hover:bg-ink-950/40 flex items-start gap-3 text-xs font-mono">
              <span class="text-zinc-500 w-44 shrink-0" title={fmtTime(e.ts_ms)}>
                {fmtAgo(e.ts_ms)}
              </span>
              <span class="text-[10px] px-1.5 py-0.5 rounded border shrink-0
                           {actionClass(e.action)}">
                {e.action}
              </span>
              <span class="text-zinc-300 shrink-0 max-w-[180px] truncate" title={e.actor}>
                {e.actor}
              </span>
              <span class="text-zinc-400 flex-1 truncate" title={e.detail}>
                {e.detail || '—'}
              </span>
              {#if e.result === 'fail'}
                <span class="text-red-400 shrink-0">✕ {e.err}</span>
              {:else}
                <span class="text-live-400 shrink-0">✓</span>
              {/if}
            </div>
          {/each}
          {#if state.entries.length === 0}
            <p class="p-6 text-center text-zinc-500 italic">
              No entries match the current filter — try clearing it or changing the limit.
            </p>
          {/if}
        </div>
      </section>

      <section class="text-xs text-zinc-500 leading-relaxed">
        <strong class="text-zinc-300">What's logged:</strong>
        login successes + failures, logout, password setup/change, token
        create/revoke, scope denials, and any non-GET mutation to
        <code>/network</code>, <code>/storage</code>, <code>/firewall</code>,
        <code>/ssh</code>, <code>/dns</code>, <code>/wifi</code>, plus persona swaps.
        Each entry carries the authenticated <em>actor</em> (session user, token name,
        or "anonymous"), the HTTP method+path it touched, success/fail, and any error.
        Chatty per-keystroke HID events and frame fetches are intentionally
        excluded to keep the log to high-signal events.
      </section>
    {/if}
    </div>
  </main>
</div>
