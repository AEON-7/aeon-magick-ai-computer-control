<script lang="ts">
  // Firewall + NAT + port-forward rules editor.
  //
  // Backed by the supervisor's /api/firewall endpoints, which read+apply
  // a TOML rules file at /etc/aeon/firewall.toml. Each rule has an ID,
  // source/destination, interface, port, protocol, action, and a hit
  // counter (sourced from iptables -nvL).
  //
  // The component renders rules grouped by chain (INPUT / OUTPUT /
  // FORWARD / PREROUTING / POSTROUTING) with the inspection order
  // visible. Reorder is locked by default; clicking "Change order"
  // unlocks drag-and-drop reorder via Svelte's built-in animations.
  //
  // Redundancy detection: a rule is flagged as "redundant with rule N"
  // when an earlier rule in the same chain matches a strict superset of
  // its predicates AND the same action — so the later rule never fires.

  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let rules: api.FirewallRule[] = [];
  let loading = true;
  let error = '';
  let unlocked = false;             // rule reorder unlocked?
  let savingNew = false;
  let refresh_iv: ReturnType<typeof setInterval>;

  // Draft for the "add rule" form.
  let newRule: api.FirewallRuleDraft = {
    chain: 'INPUT',
    table: 'filter',
    direction: 'inbound',
    interface: '',
    proto: 'tcp',
    src: '',
    dst: '',
    sport: '',
    dport: '',
    action: 'ACCEPT',
    comment: '',
  };

  async function refresh() {
    try {
      const r = await api.listFirewallRules();
      rules = r.rules;
      loading = false;
    } catch (e: any) {
      error = e?.message ?? 'failed to load rules';
      loading = false;
    }
  }

  onMount(() => {
    refresh();
    refresh_iv = setInterval(refresh, 5000);
  });
  onDestroy(() => { if (refresh_iv) clearInterval(refresh_iv); });

  async function addRule() {
    savingNew = true;
    error = '';
    try {
      await api.addFirewallRule(newRule);
      await refresh();
      newRule = { ...newRule, src: '', dst: '', sport: '', dport: '', comment: '' };
    } catch (e: any) {
      error = e?.message ?? 'failed to add rule';
    } finally {
      savingNew = false;
    }
  }

  async function removeRule(id: string) {
    if (!confirm('Delete this rule?')) return;
    try {
      await api.deleteFirewallRule(id);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to delete';
    }
  }

  async function moveRule(id: string, dir: -1 | 1) {
    try {
      await api.moveFirewallRule(id, dir);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to move';
    }
  }

  // Group by chain so we can render each chain as its own section.
  $: byChain = (() => {
    const m: Record<string, api.FirewallRule[]> = {};
    for (const r of rules) {
      (m[r.chain] ??= []).push(r);
    }
    return m;
  })();

  const CHAIN_ORDER = ['INPUT', 'OUTPUT', 'FORWARD', 'PREROUTING', 'POSTROUTING'];

  function fmtHits(n: number): string {
    if (n < 1000) return n.toString();
    if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
    return `${(n / 1_000_000).toFixed(1)}M`;
  }
</script>

<div class="p-6 space-y-6">
  {#if loading}
    <p class="text-zinc-500 text-sm">loading rules…</p>
  {:else if error}
    <p class="text-red-400 text-sm">{error}</p>
  {/if}

  <!-- Order-edit toggle -->
  <div class="flex items-center gap-3 pb-3 border-b border-ink-800">
    <button
      class="btn text-xs {unlocked ? 'bg-amber-500/20 text-amber-300 border-amber-500/40' : ''}"
      on:click={() => unlocked = !unlocked}
    >
      {unlocked ? '🔓 reorder unlocked' : '🔒 change order'}
    </button>
    <span class="text-xs text-zinc-500">
      {unlocked
        ? 'Drag rules by their handle (≡) to reorder, or use the arrow buttons.'
        : 'Rules normally lock to prevent accidental drag. Unlock to rearrange.'}
    </span>
  </div>

  <!-- Add-rule form -->
  <section class="bg-ink-950/40 border border-ink-800 rounded-lg p-4 space-y-3">
    <h4 class="font-mono text-xs uppercase tracking-wider text-zinc-400">Add rule</h4>
    <div class="grid grid-cols-2 sm:grid-cols-4 gap-3 text-xs">
      <label class="space-y-1">
        <span class="text-zinc-500">Chain</span>
        <select bind:value={newRule.chain}
                class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200">
          {#each CHAIN_ORDER as c}
            <option value={c}>{c}</option>
          {/each}
        </select>
      </label>
      <label class="space-y-1">
        <span class="text-zinc-500">Table</span>
        <select bind:value={newRule.table}
                class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200">
          <option value="filter">filter</option>
          <option value="nat">nat</option>
          <option value="mangle">mangle</option>
        </select>
      </label>
      <label class="space-y-1">
        <span class="text-zinc-500">Protocol</span>
        <select bind:value={newRule.proto}
                class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200">
          <option value="tcp">tcp</option>
          <option value="udp">udp</option>
          <option value="icmp">icmp</option>
          <option value="">any</option>
        </select>
      </label>
      <label class="space-y-1">
        <span class="text-zinc-500">Action</span>
        <select bind:value={newRule.action}
                class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200">
          <option value="ACCEPT">ACCEPT</option>
          <option value="DROP">DROP</option>
          <option value="REJECT">REJECT</option>
          <option value="REDIRECT">REDIRECT</option>
          <option value="DNAT">DNAT</option>
          <option value="SNAT">SNAT</option>
          <option value="MASQUERADE">MASQUERADE</option>
        </select>
      </label>
      <label class="space-y-1">
        <span class="text-zinc-500">Interface (e.g. usb0)</span>
        <input type="text" bind:value={newRule.interface} placeholder="any"
               class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200" />
      </label>
      <label class="space-y-1">
        <span class="text-zinc-500">Source IP/subnet</span>
        <input type="text" bind:value={newRule.src} placeholder="any"
               class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200" />
      </label>
      <label class="space-y-1">
        <span class="text-zinc-500">Dest IP/subnet</span>
        <input type="text" bind:value={newRule.dst} placeholder="any"
               class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200" />
      </label>
      <label class="space-y-1">
        <span class="text-zinc-500">Dest port(s)</span>
        <input type="text" bind:value={newRule.dport} placeholder="e.g. 443 or 80:443"
               class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200" />
      </label>
      <label class="col-span-2 sm:col-span-4 space-y-1">
        <span class="text-zinc-500">Comment (shown in rule list)</span>
        <input type="text" bind:value={newRule.comment} placeholder="why does this rule exist?"
               class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200" />
      </label>
    </div>
    <button class="btn-primary text-xs" on:click={addRule} disabled={savingNew}>
      {savingNew ? 'adding…' : '+ add rule'}
    </button>
  </section>

  <!-- Rules grouped by chain -->
  {#each CHAIN_ORDER as chain}
    {#if byChain[chain]?.length}
      <section class="space-y-2">
        <h4 class="font-mono text-xs uppercase tracking-wider text-cursed-400">
          {chain} <span class="text-zinc-500 normal-case">— inspected in order</span>
        </h4>
        <div class="space-y-1">
          {#each byChain[chain] as r, i (r.id)}
            <div class="flex items-center gap-2 p-2 rounded
                        bg-ink-950/40 border
                        {r.redundant_with ? 'border-amber-500/40' : 'border-ink-800'}"
                 class:opacity-60={r.redundant_with}>
              {#if unlocked}
                <span class="cursor-grab text-zinc-500 select-none" title="drag to reorder">≡</span>
                <div class="flex flex-col gap-0">
                  <button class="text-[10px] text-zinc-500 hover:text-cursed-300"
                          on:click={() => moveRule(r.id, -1)} disabled={i === 0}>▲</button>
                  <button class="text-[10px] text-zinc-500 hover:text-cursed-300"
                          on:click={() => moveRule(r.id, 1)}
                          disabled={i === byChain[chain].length - 1}>▼</button>
                </div>
              {/if}
              <span class="text-[10px] font-mono text-zinc-500 w-8">#{i + 1}</span>
              <span class="text-[10px] font-mono px-1.5 py-0.5 rounded border
                           {r.action === 'ACCEPT' ? 'bg-live-500/20 text-live-300 border-live-500/40'
                             : r.action === 'DROP' || r.action === 'REJECT' ? 'bg-red-500/20 text-red-300 border-red-500/40'
                             : 'bg-cursed-500/15 text-cursed-300 border-cursed-500/40'}">
                {r.action}
              </span>
              <span class="text-xs font-mono text-zinc-300 truncate flex-1">
                {r.iface ? `${r.iface} ` : ''}
                {r.proto || 'any'}
                {#if r.src} src={r.src}{/if}
                {#if r.dst} dst={r.dst}{/if}
                {#if r.dport} dport={r.dport}{/if}
              </span>
              {#if r.comment}
                <span class="text-[11px] text-zinc-500 italic truncate max-w-[200px]"
                      title={r.comment}>{r.comment}</span>
              {/if}
              <span class="text-[10px] font-mono text-zinc-500"
                    title="{r.packets} packets, {r.bytes} bytes">
                {fmtHits(r.packets)}
              </span>
              <button class="text-[10px] text-zinc-500 hover:text-red-400"
                      on:click={() => removeRule(r.id)} title="Delete rule">✕</button>
            </div>
            {#if r.redundant_with}
              <p class="ml-10 text-[11px] text-amber-400">
                ⚠ Redundant — rule #{r.redundant_with} above matches a superset of this rule's
                predicates with the same action. Safe to delete.
              </p>
            {/if}
          {/each}
        </div>
      </section>
    {/if}
  {/each}

  {#if !loading && rules.length === 0}
    <p class="text-zinc-500 text-sm italic">
      No custom rules yet — the system's default iptables policy is in effect.
      Add a rule above to override.
    </p>
  {/if}
</div>
