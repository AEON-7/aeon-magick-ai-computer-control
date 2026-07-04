<script lang="ts">
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { confirmRite } from '$lib/confirm';
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let log: api.DnsLogState | null = null;
  let blacklist: api.DnsBlacklist = { domains: [], regexes: [] };
  let sources: api.DnsSource[] = [];
  let presets: api.DnsSourcePreset[] = [];
  let loading = true;
  let error = '';
  let poll_iv: ReturnType<typeof setInterval>;

  let newDomain = '';
  let newRegex = '';
  let saving = false;
  let saveMsg = '';
  let csvImport = '';
  let filter: 'all' | 'block' | 'allow' = 'all';

  // Subscription source UI state
  let newSrcName = '';
  let newSrcUrl = '';
  let newSrcFormat: 'hosts' | 'domains' | 'adblock' = 'hosts';
  let newSrcHours = 24;
  let addingSrc = false;
  let refreshingId = '';

  async function refresh() {
    try {
      const [l, b, s] = await Promise.all([
        api.getDnsLog(),
        api.getDnsBlacklist(),
        api.listDnsSources(),
      ]);
      log = l;
      blacklist = b.blacklist;
      sources = s.sources;
      presets = s.presets;
      loading = false;
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
      loading = false;
    }
  }

  async function addPreset(p: api.DnsSourcePreset) {
    addingSrc = true;
    error = '';
    try {
      await api.addDnsSource({
        name: p.name,
        url: p.url,
        format: p.format,
        refresh_hours: 24,
      });
      saveMsg = `✓ subscribed to "${p.name}" — fetching in background`;
      setTimeout(() => (saveMsg = ''), 5000);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to add';
    } finally {
      addingSrc = false;
    }
  }

  async function addCustomSource() {
    if (!newSrcUrl.trim()) return;
    addingSrc = true;
    error = '';
    try {
      await api.addDnsSource({
        name: newSrcName.trim() || newSrcUrl,
        url: newSrcUrl.trim(),
        format: newSrcFormat,
        refresh_hours: newSrcHours,
      });
      saveMsg = '✓ subscription added';
      setTimeout(() => (saveMsg = ''), 5000);
      newSrcName = '';
      newSrcUrl = '';
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to add';
    } finally {
      addingSrc = false;
    }
  }

  async function refreshSrc(id: string) {
    refreshingId = id;
    error = '';
    try {
      const r = await api.refreshDnsSource(id);
      saveMsg = `✓ refreshed — ${r.entry_count.toLocaleString()} entries`;
      setTimeout(() => (saveMsg = ''), 5000);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'refresh failed';
    } finally {
      refreshingId = '';
    }
  }

  async function toggleSrc(id: string, enabled: boolean) {
    try {
      await api.updateDnsSource(id, { enabled });
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'toggle failed';
    }
  }

  async function deleteSrc(id: string, name: string) {
    if (!(await confirmRite({
      title: 'Remove subscription',
      body: `Remove subscription "${name}"?\n\nThis deletes the cached list; the domains will stop being blocked.`,
      danger: true,
      confirmLabel: 'remove',
    }))) return;
    try {
      await api.deleteDnsSource(id);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'delete failed';
    }
  }

  function fmtAge(ms: number): string {
    if (ms === 0) return 'never';
    const dt = Date.now() - ms;
    if (dt < 60_000) return 'just now';
    if (dt < 3_600_000) return `${Math.floor(dt / 60_000)} min ago`;
    if (dt < 86_400_000) return `${Math.floor(dt / 3_600_000)}h ago`;
    return `${Math.floor(dt / 86_400_000)}d ago`;
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
  <PageHeader title="DNS log + blacklist" />

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-4xl mx-auto w-full space-y-6">

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

    <!-- ─── Subscription sources ─── -->
    <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
      <header>
        <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
          Subscription sources
        </h2>
        <p class="text-xs text-zinc-500 mt-1">
          Subscribe to upstream blacklists (StevenBlack, OISD, Ultimate
          Hosts, etc.) and they'll auto-refresh on a schedule. The fetched
          domain set is merged with your manual list + regex matches into
          a single dnsmasq drop-in. Cache lives in
          <code class="text-cursed-300">/var/lib/aeon/dns-sources/</code>.
        </p>
      </header>

      <!-- Active subscriptions -->
      {#if sources.length}
        <div class="space-y-1.5">
          {#each sources as s (s.id)}
            <div class="flex items-center gap-3 p-3 rounded-lg
                        bg-ink-950/60 border
                        {s.last_error
                          ? 'border-red-500/40'
                          : s.stale
                            ? 'border-amber-500/30'
                            : 'border-ink-800'}">
              <input type="checkbox" checked={s.enabled}
                     on:change={(e) => toggleSrc(s.id, e.currentTarget.checked)}
                     class="w-4 h-4 accent-cursed-500 shrink-0" />
              <div class="flex-1 min-w-0 space-y-0.5">
                <div class="flex items-center gap-2 flex-wrap">
                  <span class="text-zinc-200 text-sm font-medium truncate">{s.name}</span>
                  <span class="text-[10px] font-mono uppercase tracking-wider
                               px-1.5 py-0.5 rounded border
                               bg-cursed-500/15 text-cursed-300 border-cursed-500/40">
                    {s.format}
                  </span>
                  {#if s.entry_count > 0}
                    <span class="text-[10px] font-mono text-zinc-500">
                      {s.entry_count.toLocaleString()} domains
                    </span>
                  {/if}
                  {#if s.stale && s.enabled}
                    <span class="text-[10px] font-mono text-amber-400">stale — refresh due</span>
                  {/if}
                </div>
                <div class="text-[10px] font-mono text-zinc-500 truncate" title={s.url}>
                  {s.url}
                </div>
                <div class="text-[10px] text-zinc-600">
                  last fetched {fmtAge(s.last_fetched_ms)} ·
                  every {s.refresh_hours}h
                  {#if s.sha256}· sha256 {s.sha256.slice(0, 12)}{/if}
                </div>
                {#if s.last_error}
                  <div class="text-[11px] text-red-400 mt-1 break-all">⚠ {s.last_error}</div>
                {/if}
              </div>
              <button class="text-xs px-2 py-1 rounded
                             border border-ink-700 hover:border-cursed-500/60
                             text-zinc-400 hover:text-cursed-200 transition-colors
                             disabled:opacity-50"
                      on:click={() => refreshSrc(s.id)}
                      disabled={refreshingId === s.id}>
                {refreshingId === s.id ? '⟳…' : '↻ refresh'}
              </button>
              <button class="text-xs text-zinc-500 hover:text-red-400 px-1"
                      on:click={() => deleteSrc(s.id, s.name)}>✕</button>
            </div>
          {/each}
        </div>
      {:else}
        <p class="text-xs text-zinc-500 italic">
          No subscriptions yet. Pick a curated list below or paste any
          hosts/domains URL.
        </p>
      {/if}

      <!-- Curated presets -->
      {#if presets.length}
        <div class="space-y-2 pt-3 border-t border-ink-800">
          <p class="text-xs uppercase tracking-wider text-zinc-500">
            One-click subscribe
          </p>
          <div class="grid sm:grid-cols-2 gap-2">
            {#each presets as p}
              {@const already = sources.some(s => s.url === p.url)}
              <button class="text-left p-3 rounded-lg border transition-colors
                             {already
                               ? 'bg-ink-950/40 border-ink-800 opacity-50 cursor-not-allowed'
                               : 'bg-ink-950/40 border-ink-800 hover:border-cursed-500/50 hover:bg-cursed-500/5'}"
                      disabled={already || addingSrc}
                      on:click={() => addPreset(p)}>
                <div class="flex items-center gap-2 mb-1 flex-wrap">
                  <span class="text-sm text-zinc-200 font-medium">{p.name}</span>
                  <span class="text-[10px] font-mono uppercase tracking-wider
                               px-1.5 py-0.5 rounded border
                               {p.category === 'security' ? 'bg-red-500/15 text-red-300 border-red-500/30'
                                 : p.category === 'comprehensive' ? 'bg-fuchsia-500/15 text-fuchsia-300 border-fuchsia-500/30'
                                 : p.category === 'lite' ? 'bg-live-500/15 text-live-300 border-live-500/30'
                                 : 'bg-cursed-500/15 text-cursed-300 border-cursed-500/30'}">
                    {p.category}
                  </span>
                  {#if already}
                    <span class="text-[10px] text-zinc-500">subscribed</span>
                  {/if}
                </div>
                <p class="text-[11px] text-zinc-500 leading-snug">{p.blurb}</p>
              </button>
            {/each}
          </div>
        </div>
      {/if}

      <!-- Custom source -->
      <div class="space-y-2 pt-3 border-t border-ink-800">
        <p class="text-xs uppercase tracking-wider text-zinc-500">
          Add custom source
        </p>
        <div class="grid sm:grid-cols-2 gap-2">
          <input type="text" bind:value={newSrcName}
                 placeholder="Name (e.g. 'My corp blocklist')"
                 class="bg-ink-800 border border-ink-700 rounded px-3 py-2 text-sm text-zinc-200" />
          <select bind:value={newSrcFormat}
                  class="bg-ink-800 border border-ink-700 rounded px-3 py-2 text-sm text-zinc-200">
            <option value="hosts">hosts file (0.0.0.0 domain.com lines)</option>
            <option value="domains">domains (one per line)</option>
            <option value="adblock">adblock (||domain.com^ syntax)</option>
          </select>
        </div>
        <input type="url" bind:value={newSrcUrl}
               placeholder="https://raw.githubusercontent.com/…/hosts"
               class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-2 text-sm text-zinc-200 font-mono" />
        <div class="flex items-center gap-3 flex-wrap">
          <label class="flex items-center gap-2 text-xs text-zinc-500">
            Refresh every
            <input type="number" bind:value={newSrcHours} min="1" max="720" step="1"
                   class="w-16 bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200" />
            hours
          </label>
          <button class="btn-primary text-xs ml-auto"
                  on:click={addCustomSource}
                  disabled={addingSrc || !newSrcUrl.trim()}>
            {addingSrc ? 'adding…' : '+ subscribe'}
          </button>
        </div>
      </div>
    </section>

    <!-- ─── Manual blacklist editor ─── -->
    <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
      <header>
        <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">Manual blacklist</h2>
        <p class="text-xs text-zinc-500 mt-1">
          Hand-curated entries on top of subscriptions. Blacklisted domains
          resolve to <code class="text-cursed-300">0.0.0.0</code>, effectively
          dropping the connection. Wildcard subdomain blocking via dnsmasq's
          <code>address=/</code> syntax — adding
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

    </div>
  </main>
</div>
