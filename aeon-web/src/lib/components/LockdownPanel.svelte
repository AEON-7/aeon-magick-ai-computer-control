<script lang="ts">
  // LOCKDOWN + granular API/MCP exposure controls. The killswitch refuses all
  // external API + MCP (admin session + KVM unaffected); individual categories
  // can be disabled instead. Admin-only surface (/api/lockdown).
  import { onMount } from 'svelte';
  import { confirmRite } from '$lib/confirm';

  let enabled = false;
  let disabled: string[] = [];
  let categories: string[] = [];
  let loaded = false;
  let busy = false;

  const CAT_LABELS: Record<string, string> = {
    hid: 'HID (keyboard/mouse)', vision: 'Vision (screen)', macros: 'Macros',
    network: 'Network config', files: 'File transfer', hardware: 'GPIO / Hardware',
    target: 'Target power', orbnet: 'OrbNet', mcp: 'MCP (agent tools)',
  };

  async function load() {
    try {
      const r = await fetch('/api/lockdown', { credentials: 'same-origin' }).then((r) => r.json());
      if (r.ok) { enabled = r.enabled; disabled = r.disabled_categories ?? []; categories = r.categories ?? []; loaded = true; }
    } catch { /* admin-only */ }
  }
  async function save(newEnabled: boolean, newDisabled: string[]) {
    busy = true;
    try {
      const r = await fetch('/api/lockdown', {
        method: 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled: newEnabled, disabled_categories: newDisabled }),
      }).then((r) => r.json());
      if (r.ok) { enabled = r.enabled; disabled = r.disabled_categories ?? []; }
    } finally { busy = false; }
  }
  async function toggleLockdown() {
    if (!enabled && !(await confirmRite({
      title: 'Engage lockdown mode',
      body: 'This refuses ALL external API + MCP calls — the Orb becomes a single-user jump box + KVM. Only the admin session works. You can release it here anytime.',
      danger: true,
      confirmLabel: 'engage lockdown',
    }))) return;
    save(!enabled, disabled);
  }
  function toggleCat(cat: string) {
    const next = disabled.includes(cat) ? disabled.filter((c) => c !== cat) : [...disabled, cat];
    save(enabled, next);
  }
  onMount(load);
</script>

{#if loaded}
  <section class="bg-ink-900 border {enabled ? 'border-red-500/60' : 'border-steel-700'} rounded-sm p-5 space-y-4">
    <div class="flex items-start justify-between gap-4 flex-wrap">
      <div>
        <h2 class="font-mono text-sm uppercase tracking-wider {enabled ? 'text-red-300' : 'text-zinc-400'}">Lockdown &amp; API exposure</h2>
        <p class="text-[11px] text-zinc-500 mt-1 max-w-md">The killswitch refuses every external API token + MCP call — the admin web session and the KVM keep working. Or disable individual categories below.</p>
      </div>
      <button class="px-4 py-2 rounded-sm font-mono text-sm transition disabled:opacity-50 {enabled ? 'bg-red-700 text-white motion-safe:animate-ember' : 'bg-red-900/30 text-red-300 border border-red-500/40 hover:bg-red-800/40'}"
              disabled={busy} on:click={toggleLockdown}>
        {enabled ? '● LOCKDOWN ENGAGED — release' : 'ENGAGE LOCKDOWN'}
      </button>
    </div>
    {#if !enabled}
      <div class="grid grid-cols-2 sm:grid-cols-3 gap-2 pt-3 border-t border-ink-800">
        {#each categories as cat}
          <button class="flex items-center justify-between gap-2 px-3 py-2 rounded border text-xs transition {disabled.includes(cat) ? 'border-red-500/40 bg-red-900/20 text-red-300' : 'border-steel-700 bg-ink-800 text-zinc-300 hover:border-steel-600'}"
                  disabled={busy} on:click={() => toggleCat(cat)}>
            <span class="truncate">{CAT_LABELS[cat] ?? cat}</span>
            <span class="text-[10px] font-mono">{disabled.includes(cat) ? 'OFF' : 'on'}</span>
          </button>
        {/each}
      </div>
      <p class="text-[10px] text-zinc-600">Disabling a category refuses it for agent tokens + MCP; the admin web UI is never affected.</p>
    {/if}
  </section>
{/if}
