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
  import { confirmRite } from '$lib/confirm';

  let rules: api.FirewallRule[] = [];
  let systemRules: api.SystemFirewallRule[] = [];
  let systemDiag: api.SystemFirewallDiagnostics | null = null;
  let showSystem = false;           // System rules panel hidden by default
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
    out_iface: '',
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
      const [r, s] = await Promise.all([
        api.listFirewallRules(),
        api.listSystemFirewallRules(),
      ]);
      rules = r.rules;
      systemRules = s.rules;
      systemDiag = s.diagnostics ?? null;
      loading = false;
    } catch (e: any) {
      error = e?.message ?? 'failed to load rules';
      loading = false;
    }
  }

  /// Pre-populate the add-rule form from a system rule the user wants
  /// to override. Flips the effect (DROP/AEON_DROP → ACCEPT) since
  /// override is almost always "let this through". User rules now run
  /// BEFORE system rules (v37+) so an ACCEPT here will take precedence
  /// over the system DROP it's overriding.
  function overrideSystem(r: api.SystemFirewallRule) {
    newRule.chain = r.chain;
    // Tables other than filter are rarer; keep filter as the default.
    newRule.table = r.table;
    // Override is almost always "allow what was being blocked".
    newRule.action = 'ACCEPT';
    newRule.proto = r.proto === 'any' ? '' : r.proto;
    newRule.interface = r.iface;
    newRule.out_iface = r.out_iface;
    newRule.src = r.src === 'any' ? '' : r.src;
    newRule.dst = r.dst === 'any' ? '' : r.dst;
    newRule.sport = r.sport;
    newRule.dport = r.dport;
    newRule.comment = `Override ${r.source} ${r.target} on ${r.chain}`;
    // Trigger the same highlight + scroll-into-view as the deep-link path.
    prefilledFromUrl = true;
    setTimeout(() => {
      document.getElementById('fw-add-rule')?.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }, 80);
    setTimeout(() => (prefilledFromUrl = false), 4000);
  }

  function fmtHitsBig(n: number): string {
    if (n < 1000) return n.toString();
    if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
    return `${(n / 1_000_000).toFixed(1)}M`;
  }

  // Highlight flash for the new-rule form when we arrive via a deep
  // link from /security's "allow this traffic" button.
  let prefilledFromUrl = false;

  onMount(() => {
    refresh();
    refresh_iv = setInterval(refresh, 5000);

    // Deep-link from /security → pre-populate newRule from URL params.
    // /security/+page.svelte builds the URL via allowLink(); we mirror
    // its schema here. Wrapped in a try so a malformed URL doesn't
    // break the editor.
    try {
      const sp = new URLSearchParams(window.location.search);
      if (sp.get('createRule') === '1') {
        if (sp.has('chain'))   newRule.chain = sp.get('chain')!;
        if (sp.has('action'))  newRule.action = sp.get('action')!;
        if (sp.has('proto'))   newRule.proto = sp.get('proto')!;
        if (sp.has('iface'))   newRule.interface = sp.get('iface')!;
        if (sp.has('out_iface')) newRule.out_iface = sp.get('out_iface')!;
        if (sp.has('src'))     newRule.src = sp.get('src')!;
        if (sp.has('dst'))     newRule.dst = sp.get('dst')!;
        if (sp.has('sport'))   newRule.sport = sp.get('sport')!;
        if (sp.has('dport'))   newRule.dport = sp.get('dport')!;
        if (sp.has('comment')) newRule.comment = sp.get('comment')!;
        prefilledFromUrl = true;
        // Scroll the form into view + fade the highlight after a moment.
        setTimeout(() => {
          document.getElementById('fw-add-rule')?.scrollIntoView({ behavior: 'smooth', block: 'center' });
        }, 80);
        setTimeout(() => (prefilledFromUrl = false), 4000);
        // Clean the URL so a refresh doesn't re-trigger the pre-fill.
        const cleaned = window.location.pathname + window.location.hash;
        history.replaceState({}, '', cleaned);
      }
    } catch (e) {
      console.warn('deep-link parse failed', e);
    }
  });
  onDestroy(() => { if (refresh_iv) clearInterval(refresh_iv); });

  async function addRule() {
    savingNew = true;
    error = '';
    try {
      await api.addFirewallRule(newRule);
      await refresh();
      newRule = { ...newRule, src: '', dst: '', sport: '', dport: '', comment: '', out_iface: '' };
    } catch (e: any) {
      error = e?.message ?? 'failed to add rule';
    } finally {
      savingNew = false;
    }
  }

  async function removeRule(id: string) {
    if (!(await confirmRite({
      title: 'Delete firewall rule',
      body: 'Delete this rule?',
      danger: true,
      confirmLabel: 'delete',
    }))) return;
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

  <!-- Add-rule form. The id is the deep-link anchor target for
       "allow this traffic" buttons on the /security page. -->
  <section id="fw-add-rule"
           class="border rounded-lg p-4 space-y-3 transition-colors duration-500
                  {prefilledFromUrl
                    ? 'bg-live-500/10 border-live-500/40 shadow-[0_0_18px_rgba(110,231,183,0.18)]'
                    : 'bg-ink-950/40 border-ink-800'}">
    <h4 class="font-mono text-xs uppercase tracking-wider text-zinc-400">
      Add rule
      {#if prefilledFromUrl}
        <span class="ml-2 text-[10px] text-live-300 normal-case">
          ← pre-filled from blocked-traffic log; review + save
        </span>
      {/if}
    </h4>
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
                title={
                  newRule.action === 'DROP'
                    ? 'DROP: silently black-hole the packet. Best for INBOUND blocks from untrusted sources (the WAN, an attacker) — no response = no recon info.'
                  : newRule.action === 'REJECT'
                    ? 'REJECT: send an explicit "no" back (ICMP port-unreachable for UDP, TCP RST for TCP). Best for OUTBOUND blocks against your trusted clients — they fail-fast and try the next thing.'
                  : 'Pick the action this rule should take.'
                }
                class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200">
          <option value="ACCEPT">ACCEPT</option>
          <option value="DROP">DROP — silent (inbound stealth)</option>
          <option value="REJECT">REJECT — explicit fail (client-facing)</option>
          <option value="REDIRECT">REDIRECT</option>
          <option value="DNAT">DNAT</option>
          <option value="SNAT">SNAT</option>
          <option value="MASQUERADE">MASQUERADE</option>
        </select>
      </label>
      {#if newRule.action === 'DROP' || newRule.action === 'REJECT'}
        <!-- Quick guidance on which to pick. The general rule: REJECT
             on outbound to give your client fast fall-back; DROP on
             inbound from outside to deny recon info. -->
        <p class="col-span-2 sm:col-span-4 text-[10px] text-zinc-500 leading-relaxed">
          {#if newRule.action === 'DROP'}
            <strong class="text-zinc-400">DROP:</strong> silent black-hole.
            Use for <em>inbound</em> blocks from untrusted sources — no
            response means no information leaked to a port-scanner.
            Downside: trusted clients hitting this rule will hang until
            their TCP/UDP layer times out.
          {:else}
            <strong class="text-zinc-400">REJECT:</strong> explicit "no"
            sent back (ICMP port-unreachable for UDP, TCP RST for TCP).
            Use for <em>outbound</em> blocks of your own client — they
            fail-fast and retry over the next allowed transport (e.g.
            QUIC → TCP fall-back in milliseconds).
          {/if}
        </p>
      {/if}
      <label class="space-y-1">
        <span class="text-zinc-500">In iface ({newRule.chain === 'OUTPUT' || newRule.chain === 'POSTROUTING' ? '-o' : '-i'} e.g. usb0)</span>
        <input type="text" bind:value={newRule.interface} placeholder="any"
               class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200" />
      </label>
      {#if newRule.chain === 'FORWARD'}
        <!-- FORWARD chains have both an input and output interface match;
             pairing -i and -o makes the rule strictly narrower than -i
             alone (e.g. "allow this exact usb0→eth0 hop, not any other
             egress"). Hidden for non-FORWARD chains where -o is already
             handled by the single `iface` field above. -->
        <label class="space-y-1">
          <span class="text-zinc-500">Out iface (-o e.g. eth0)</span>
          <input type="text" bind:value={newRule.out_iface} placeholder="any"
                 class="w-full bg-ink-800 border border-ink-700 rounded px-2 py-1 text-zinc-200" />
        </label>
      {/if}
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
        <span class="text-zinc-500">Source port(s)</span>
        <input type="text" bind:value={newRule.sport} placeholder="any"
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
      Expand the panel below to see what the system installed, or add a
      user rule above to override any of them.
    </p>
  {/if}

  <!-- ──────────────────────────────────────────────────────────────── -->
  <!-- System default rules (read-only) — what aeon-net-services and    -->
  <!-- aeon-usb-net installed at boot/apply. Click "override" to drop   -->
  <!-- a matching ACCEPT into your user rules. User rules run BEFORE    -->
  <!-- system rules (v37 changed user-rule application from -A to -I 1) -->
  <!-- so an override actually takes effect.                            -->
  <!-- ──────────────────────────────────────────────────────────────── -->
  <section class="space-y-2 pt-4 border-t border-ink-800">
    <button class="flex items-center gap-2 text-xs uppercase tracking-wider
                   text-zinc-500 hover:text-zinc-300 transition-colors"
            on:click={() => (showSystem = !showSystem)}>
      <span>{showSystem ? '▼' : '▶'}</span>
      System default rules ({systemRules.length})
      <span class="normal-case text-[10px] text-zinc-600">
        installed by aeon-net-services + aeon-usb-net
      </span>
    </button>

    {#if showSystem}
      <!-- v62: more prominent warning banner — the user explicitly
           asked for full visibility into system rules. The override
           flow (click "override" to drop a matching user-ACCEPT
           before this rule fires) is the safe primary action.
           Direct edit/disable of system rules is on the roadmap
           but lives in a separate apply pipeline so it doesn't ship
           in this cut. -->
      <div class="mt-2 p-3 rounded border border-amber-500/40
                  bg-amber-500/10 space-y-1.5">
        <div class="flex items-start gap-2">
          <span class="text-amber-400 text-base leading-none mt-0.5">⚠</span>
          <div class="space-y-1 flex-1">
            <p class="text-xs text-amber-200 font-medium">
              These are the system-managed rules backing Tor, I2P, the
              VPN kill-switch, USB-net isolation/restriction, DNS
              hijacking, and anti-leak guards.
            </p>
            <p class="text-[11px] text-amber-100/70 leading-relaxed">
              Your user rules in the editor above run <strong>before</strong>
              these — that's how you punch holes safely. Click
              <span class="font-mono text-cursed-300">override</span> on
              any DROP/REJECT row to drop a matching ACCEPT into your
              user rules. <strong>Disabling a system rule directly is
              not yet exposed in the UI</strong> (planned for v63);
              modifying these without understanding the wider iptables
              chain can break Tor routing, expose your real IP under
              VPN, or break USB-host isolation. If you really need to
              tear one down, SSH in and edit the relevant section of
              <code>/usr/local/bin/aeon-net-services.sh</code> or
              <code>/usr/local/bin/aeon-usb-net.sh</code>.
            </p>
          </div>
        </div>
      </div>

      {#each Array.from(new Set(systemRules.map(r => `${r.table}:${r.chain}`))) as tableChain}
        {@const [table, chain] = tableChain.split(':')}
        <div class="space-y-1">
          <p class="text-[10px] uppercase tracking-wider text-cursed-400 mt-3">
            <span class="text-zinc-500">{table}:</span>{chain}
          </p>
          {#each systemRules.filter(r => `${r.table}:${r.chain}` === tableChain) as r, i (tableChain + i)}
            <div class="flex items-center gap-2 p-2 rounded
                        bg-ink-950/30 border border-ink-800/60 text-[11px] font-mono">
              <span class="text-[10px] px-1.5 py-0.5 rounded border shrink-0
                           {r.target === 'ACCEPT' ? 'bg-live-500/15 text-live-300 border-live-500/40'
                             : r.target === 'AEON_DROP' || r.target === 'DROP' || r.target === 'REJECT'
                               ? 'bg-red-500/15 text-red-300 border-red-500/40'
                               : 'bg-cursed-500/10 text-cursed-300 border-cursed-500/30'}">
                {r.effect}
              </span>
              <span class="text-[10px] text-zinc-500 shrink-0">{r.source}</span>
              <span class="text-zinc-300 flex-1 truncate"
                    title={`${r.proto} ${r.iface ? 'in=' + r.iface : ''} ${r.out_iface ? 'out=' + r.out_iface : ''} src=${r.src} dst=${r.dst} ${r.sport ? 'sport=' + r.sport : ''} ${r.dport ? 'dport=' + r.dport : ''}`}>
                {r.proto !== 'any' ? r.proto : ''}
                {#if r.iface}<span class="text-zinc-500">in=</span>{r.iface}{/if}
                {#if r.out_iface}<span class="text-zinc-500">out=</span>{r.out_iface}{/if}
                <span class="text-zinc-500">src=</span>{r.src}
                <span class="text-zinc-500">dst=</span>{r.dst}
                {#if r.dport}<span class="text-zinc-500">dport=</span>{r.dport}{/if}
                {#if r.sport}<span class="text-zinc-500">sport=</span>{r.sport}{/if}
              </span>
              <span class="text-[10px] text-zinc-500" title="{r.packets} packets, {r.bytes} bytes">
                {fmtHitsBig(r.packets)}
              </span>
              {#if r.target === 'AEON_DROP' || r.target === 'DROP' || r.target === 'REJECT'}
                <button class="text-[10px] px-2 py-0.5 rounded shrink-0
                               border border-live-500/40 text-live-300
                               hover:bg-live-500/10 hover:text-live-200"
                        on:click={() => overrideSystem(r)}
                        title="Pre-fill an ACCEPT user rule with the same predicates">
                  override
                </button>
              {/if}
            </div>
          {/each}
        </div>
      {/each}

      {#if systemRules.length === 0}
        <!-- v63: dump the diagnostics so the operator can tell why
             this list is empty. The two common reasons are: (a) no
             aeon-* tagged rules installed yet because VPN/Tor/I2P
             are off AND USB net is off (in which case
             total_rules_per_table will still show non-zero counts
             from NM/kernel defaults), or (b) the supervisor literally
             can't shell out to iptables (all zeros). -->
        {#if systemDiag}
          {@const totalAcrossTables = Object.values(systemDiag.total_rules_per_table).reduce((a, b) => a + b, 0)}
          {@const aeonTotal = Object.values(systemDiag.aeon_tag_counts).reduce((a, b) => a + b, 0)}
          {#if totalAcrossTables === 0}
            <div class="p-3 rounded bg-red-500/10 border border-red-500/40
                        text-xs text-red-200 space-y-1">
              <p><strong>Supervisor can't read iptables.</strong></p>
              <p class="text-red-200/70">
                <code>iptables -nvL</code> returned 0 total rules across
                filter/nat/mangle. Either the binary isn't on $PATH for
                the systemd unit, or the supervisor doesn't have
                CAP_NET_ADMIN. SSH in and check
                <code>journalctl -u aeon-supervisor</code>.
              </p>
            </div>
          {:else if aeonTotal === 0}
            <div class="p-3 rounded bg-zinc-700/30 border border-ink-700
                        text-xs text-zinc-400 space-y-1">
              <p><strong>No aeon-tagged rules installed.</strong></p>
              <p class="text-zinc-500">
                The kernel has
                {totalAcrossTables} total iptables rules across
                {Object.entries(systemDiag.total_rules_per_table)
                  .map(([t, n]) => `${t}=${n}`).join(', ')}
                — those are from NetworkManager, kernel defaults, and
                other non-aeon services. Aeon installs its own rules
                only when the relevant feature is on:
                <em>USB ethernet</em> adds DNAT/MASQUERADE for usb0:53,
                <em>VPN/Tor/I2P</em> add the kill-switch + redirect
                chains, etc. Turn one of those on (network page) and
                this list will populate.
              </p>
            </div>
          {/if}
        {:else}
          <p class="text-xs text-zinc-500 italic">
            No system rules tagged. Either nothing is installed yet, or the
            supervisor can't shell out to iptables.
          </p>
        {/if}
      {:else if systemDiag}
        <!-- Non-empty list — still surface the breakdown for context. -->
        <p class="text-[10px] font-mono text-zinc-600 pt-2">
          Showing {systemRules.length} aeon-tagged rule{systemRules.length === 1 ? '' : 's'}
          of
          {Object.entries(systemDiag.total_rules_per_table)
            .map(([t, n]) => `${t}=${n}`).join(', ')}
          total kernel rules. Per-tag:
          {Object.entries(systemDiag.aeon_tag_counts)
            .map(([t, n]) => `${t}=${n}`).join(', ') || 'none'}
        </p>
      {/if}
    {/if}
  </section>
</div>
