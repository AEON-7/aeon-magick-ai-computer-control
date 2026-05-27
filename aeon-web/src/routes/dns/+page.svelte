<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let log: api.DnsLogState | null = null;
  let blacklist: api.DnsBlacklist = { domains: [], regexes: [] };
  let loading = true;
  let error = '';
  let poll_iv: ReturnType<typeof setInterval>;

  let newDomain = '';
  let newRegex = '';
  let saving = false;
  let saveMsg = '';
  let csvImport = '';
  let filter: 'all' | 'block' | 'allow' = 'all';

  async function refresh() {
    try {
      const [l, b] = await Promise.all([api.getDnsLog(), api.getDnsBlacklist()]);
      log = l;
      blacklist = b.blacklist;
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

  async function toggleLog(enabled: boolean) {
    try {
      await api.setDnsLogEnabled(enabled);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to toggle';
    }
  }

  async function saveBlacklist() {
    saving = true;
    error = '';
    try {
      // De-dup + sort on client too for nicer UX.
      const domains = Array.from(new Set(blacklist.domains.map(s => s.trim().toLowerCase()).filter(Boolean))).sort();
      const regexes = blacklist.regexes.map(s => s.trim()).filter(Boolean);
      await api.setDnsBlacklist({ domains, regexes });
      saveMsg = '✓ saved & applied';
      setTimeout(() => (saveMsg = ''), 4000);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to save';
    } finally {
      saving = false;
    }
  }

  async function addDomain() {
    const d = newDomain.trim().toLowerCase();
    if (!d) return;
    if (!blacklist.domains.includes(d)) {
      blacklist.domains = [...blacklist.domains, d].sort();
    }
    newDomain = '';
  }

  async function addRegex() {
    const r = newRegex.trim();
    if (!r) return;
    if (!blacklist.regexes.includes(r)) {
      blacklist.regexes = [...blacklist.regexes, r];
    }
    newRegex = '';
  }

  function removeDomain(d: string) {
    blacklist.domains = blacklist.domains.filter(x => x !== d);
  }
  function removeRegex(r: string) {
    blacklist.regexes = blacklist.regexes.filter(x => x !== r);
  }

  async function doImport() {
    if (!csvImport.trim()) return;
    saving = true;
    try {
      const r = await api.uploadDnsBlacklistCsv(csvImport);
      saveMsg = `✓ added ${r.added} domain(s)`;
      setTimeout(() => (saveMsg = ''), 5000);
      csvImport = '';
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'import failed';
    } finally {
      saving = false;
    }
  }

  function fmtTime(ms: number): string {
    if (!ms) return '—';
    const d = new Date(ms);
    return d.toLocaleTimeString();
  }

  $: filteredEntries = log?.entries.filter(e => filter === 'all' || e.action === filter) ?? [];
  $: blockedCount = log?.entries.filter(e => e.action === 'block').length ?? 0;
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← AEON MAGICK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">DNS log + blacklist</span>
    </div>
  </header>

  <main class="flex-1 overflow-auto p-6 max-w-4xl mx-auto w-full space-y-6">

    {#if loading}
      <p class="text-zinc-500 text-sm">loading…</p>
    {/if}

    <!-- Stats overview -->
    {#if log}
      <section class="grid grid-cols-3 gap-3">
        <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Allowed</div>
          <div class="text-2xl font-mono text-live-400 mt-1">{log.allowed_total.toLocaleString()}</div>
        </div>
        <div class="bg-ink-900 border border-red-500/30 rounded-xl p-4">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Blocked</div>
          <div class="text-2xl font-mono text-red-400 mt-1">{log.blocked_total.toLocaleString()}</div>
        </div>
        <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Block list size</div>
          <div class="text-2xl font-mono text-cursed-400 mt-1">{blacklist.domains.length.toLocaleString()}</div>
          <div class="text-[10px] text-zinc-500 mt-1">+ {blacklist.regexes.length} regex(es)</div>
        </div>
      </section>
    {/if}

    <!-- Logging toggle -->
    <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
      <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">Query log</h2>
      <label class="flex items-center gap-3 cursor-pointer">
        <input type="checkbox" checked={log?.enabled ?? false}
               on:change={(e) => toggleLog(e.currentTarget.checked)}
               class="w-4 h-4 accent-cursed-500" />
        <span class="text-zinc-200 text-sm">
          Enable per-query DNS logging
        </span>
      </label>
      <p class="text-xs text-zinc-500 leading-relaxed">
        When on, dnsmasq writes every client DNS query to the system
        journal and we tail the last 500 entries here. Logs include
        client IP, requested domain, query type. They're not persisted
        beyond the journal's normal rotation window — there's no shadow
        copy anywhere else.
      </p>
    </section>

    <!-- Log viewer -->
    {#if log?.enabled}
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
        <div class="flex items-center justify-between">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Recent queries ({filteredEntries.length})
          </h2>
          <div class="flex gap-1 text-xs">
            <button class="btn text-xs {filter === 'all' ? 'bg-cursed-500/20 border-cursed-500/40' : ''}"
                    on:click={() => filter = 'all'}>all</button>
            <button class="btn text-xs {filter === 'block' ? 'bg-red-500/20 border-red-500/40' : ''}"
                    on:click={() => filter = 'block'}>blocked</button>
            <button class="btn text-xs {filter === 'allow' ? 'bg-live-500/20 border-live-500/40' : ''}"
                    on:click={() => filter = 'allow'}>allowed</button>
          </div>
        </div>
        <div class="max-h-96 overflow-auto bg-ink-950 rounded border border-ink-800 p-2 font-mono text-[11px] space-y-0.5">
          {#each filteredEntries.slice(0, 200) as e (e.ts_ms + e.domain + e.client)}
            <div class="flex items-baseline gap-3 px-2 py-0.5 hover:bg-ink-900/50 rounded
                        {e.action === 'block' ? 'text-red-300' : 'text-zinc-300'}">
              <span class="text-zinc-600 w-16 shrink-0">{fmtTime(e.ts_ms)}</span>
              <span class="text-zinc-500 w-28 shrink-0 truncate">{e.client}</span>
              <span class="text-cursed-400 w-12 shrink-0">{e.qtype}</span>
              <span class="truncate flex-1" title={e.domain}>{e.domain}</span>
              {#if e.action === 'block'}
                <span class="text-red-400">✕</span>
              {/if}
            </div>
          {/each}
          {#if filteredEntries.length === 0}
            <p class="text-zinc-500 italic p-2">no entries yet</p>
          {/if}
        </div>
      </section>
    {/if}

    <!-- Blacklist editor -->
    <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
      <header>
        <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">Blacklist</h2>
        <p class="text-xs text-zinc-500 mt-1">
          Blacklisted domains resolve to <code class="text-cursed-300">0.0.0.0</code>,
          effectively dropping the connection. Wildcard subdomain blocking
          via dnsmasq's <code>address=/</code> syntax — adding
          <code class="text-cursed-300">evil.com</code> also blocks
          <code class="text-cursed-300">ads.evil.com</code>.
        </p>
      </header>

      <!-- Add domain -->
      <div class="space-y-2">
        <label class="text-xs text-zinc-500 block" for="dns-newdomain">Add domain</label>
        <div class="flex gap-2">
          <input id="dns-newdomain" type="text" bind:value={newDomain}
                 placeholder="evil-tracker.com"
                 on:keydown={(e) => e.key === 'Enter' && addDomain()}
                 class="flex-1 bg-ink-800 border border-ink-700 rounded px-3 py-2 text-sm text-zinc-200 font-mono" />
          <button class="btn-primary text-xs" on:click={addDomain}>+ add</button>
        </div>
      </div>

      <!-- Add regex -->
      <div class="space-y-2">
        <label class="text-xs text-zinc-500 block" for="dns-newregex">Add regex pattern</label>
        <div class="flex gap-2">
          <input id="dns-newregex" type="text" bind:value={newRegex}
                 placeholder=".*\.adsrv\.[a-z]+"
                 on:keydown={(e) => e.key === 'Enter' && addRegex()}
                 class="flex-1 bg-ink-800 border border-ink-700 rounded px-3 py-2 text-sm text-zinc-200 font-mono" />
          <button class="btn-primary text-xs" on:click={addRegex}>+ add</button>
        </div>
        <p class="text-[11px] text-zinc-500">
          Regex support is best-effort — we extract domain-like literals
          from the pattern and feed them to dnsmasq. Complex regex needs
          a dedicated DNS filter (out of scope for the Pi).
        </p>
      </div>

      <!-- Import CSV -->
      <div class="space-y-2">
        <label class="text-xs text-zinc-500 block" for="dns-csvimport">Bulk import (paste CSV or hosts-file)</label>
        <textarea id="dns-csvimport" bind:value={csvImport} rows="3"
                  placeholder="ads.example.com&#10;0.0.0.0 trackers.evil.io&#10;another-bad.tld"
                  class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-2 text-xs text-zinc-200 font-mono"></textarea>
        <button class="btn text-xs" on:click={doImport} disabled={saving || !csvImport.trim()}>
          import
        </button>
      </div>

      <!-- Current entries -->
      {#if blacklist.domains.length}
        <details class="border-t border-ink-700 pt-3">
          <summary class="cursor-pointer text-xs text-zinc-500 hover:text-zinc-300">
            Current entries ({blacklist.domains.length} domains, {blacklist.regexes.length} regexes) — expand to edit
          </summary>
          <div class="mt-3 max-h-64 overflow-auto space-y-1">
            {#each blacklist.domains as d}
              <div class="flex items-center justify-between px-2 py-1 rounded
                          hover:bg-ink-950 group text-xs font-mono">
                <span class="text-zinc-300">{d}</span>
                <button class="text-zinc-600 group-hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
                        on:click={() => removeDomain(d)}>✕</button>
              </div>
            {/each}
            {#each blacklist.regexes as r}
              <div class="flex items-center justify-between px-2 py-1 rounded
                          hover:bg-ink-950 group text-xs font-mono text-fuchsia-400">
                <span>regex: {r}</span>
                <button class="text-zinc-600 group-hover:text-red-400 opacity-0 group-hover:opacity-100 transition-opacity"
                        on:click={() => removeRegex(r)}>✕</button>
              </div>
            {/each}
          </div>
        </details>
      {/if}

      <div class="flex items-center gap-3 pt-2 border-t border-ink-700">
        <button class="btn-primary" on:click={saveBlacklist} disabled={saving}>
          {saving ? 'saving…' : 'save & apply'}
        </button>
        {#if saveMsg}
          <span class="text-xs text-live-400 font-mono">{saveMsg}</span>
        {/if}
        {#if error}
          <span class="text-xs text-red-400 font-mono">{error}</span>
        {/if}
      </div>
    </section>

  </main>
</div>
