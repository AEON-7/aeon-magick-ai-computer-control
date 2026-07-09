<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';

  let roster: api.FleetRoster | null = null;
  let err = '';
  let loading = true;
  let poll: ReturnType<typeof setInterval>;

  // this Orb's editable settings
  let cfg: api.FleetConfig | null = null;
  let labelInput = '';
  let seedsInput = '';
  let tailscaleInput = true;
  let saving = false;
  let saveMsg = '';

  async function loadRoster() {
    try {
      roster = await api.getFleetRoster();
      err = '';
    } catch (e: any) {
      err = e?.message ?? 'failed to load roster';
    } finally {
      loading = false;
    }
  }
  async function loadConfig() {
    try {
      cfg = await api.getFleetConfig();
      labelInput = cfg.label ?? '';
      seedsInput = (cfg.seeds ?? []).join('\n');
      tailscaleInput = cfg.tailscale ?? true;
    } catch {
      /* config editor stays empty if it fails — roster is the important bit */
    }
  }
  async function saveConfig() {
    saving = true;
    saveMsg = 'saving…';
    const seeds = seedsInput.split(/[\n,]+/).map((s) => s.trim()).filter(Boolean);
    try {
      await api.setFleetConfig({ label: labelInput.trim(), seeds, tailscale: tailscaleInput });
      saveMsg = 'saved';
      await Promise.all([loadConfig(), loadRoster()]);
      setTimeout(() => (saveMsg = ''), 3000);
    } catch (e: any) {
      saveMsg = 'error: ' + (e?.message ?? 'failed');
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    loadRoster();
    loadConfig();
    poll = setInterval(loadRoster, 5000);
  });
  onDestroy(() => clearInterval(poll));

  const MODEL_PILL: Record<string, string> = {
    pi5: 'bg-emerald-900/50 text-emerald-300',
    pi4: 'bg-sky-900/50 text-sky-300',
  };
  const modelPill = (m?: string) => MODEL_PILL[m ?? ''] ?? 'bg-ink-800 text-ink-400';
  const modelLabel = (m?: string) => (m === 'pi5' ? 'Pi 5' : m === 'pi4' ? 'Pi 4' : m ?? '—');
  const upsText = (o: api.FleetOrb) =>
    o.ups?.present ? `${o.ups.charge ?? '?'}%${o.ups.on_battery ? ' · on battery' : ''}` : 'no UPS';
  const tempText = (o: api.FleetOrb) =>
    o.health?.cpu_temp_c ? `${o.health.cpu_temp_c.toFixed(0)}°C` : '—';
  const title = (o: api.FleetOrb) => o.label || o.hostname || o.addr || o.id || 'orb';

  $: orbs = roster ? [roster.self, ...roster.peers] : [];
</script>

<div class="page-void min-h-screen">
  <header class="flex items-center justify-between px-5 py-3 chrome-header">
    <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
    <h1 class="font-mono text-lg text-emerald-300 flex items-center gap-2">
      <Icon name="fleet" class="w-5 h-5" /> Fleet
    </h1>
    <div class="w-32"></div>
  </header>

  <main class="max-w-3xl mx-auto px-5 py-6 space-y-5">
    {#if loading}
      <p class="text-ink-400 text-sm font-mono">loading roster…</p>
    {:else if err}
      <div class="rounded-sm border border-red-700 bg-red-900/20 p-4 text-red-300 text-sm font-mono">{err}</div>
    {:else if roster}
      {#if !roster.configured}
        <div class="rounded-sm border border-amber-700/60 bg-amber-900/15 p-4 text-amber-200 text-sm leading-relaxed">
          This Orb isn't enrolled in a fleet yet. Add a shared <code class="font-mono">token</code> (the same value on
          every Orb) and peer <code class="font-mono">seeds</code> to <code class="font-mono">/etc/aeon/fleet.toml</code>,
          then it discovers the others. Only this Orb is shown until then.
        </div>
      {/if}

      <p class="text-ink-300 text-sm">
        {orbs.filter((o) => o.online).length} of {orbs.length} Orb{orbs.length === 1 ? '' : 's'} online · polled directly
        over the tailnet/LAN.
      </p>

      <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
        {#each orbs as o (o.id ?? o.addr)}
          <div class="rounded-sm border bg-ink-900 p-4 {o.online ? 'border-steel-700' : 'border-ink-800 opacity-70'}">
            <div class="flex items-center justify-between">
              <div class="flex items-center gap-2 min-w-0">
                <span class="h-2 w-2 rounded-full shrink-0 {o.online ? 'bg-emerald-400' : 'bg-red-500'}"></span>
                <span class="font-mono text-emerald-300 truncate">{title(o)}</span>
                {#if o.is_self}<span class="text-[10px] font-mono text-ink-500 uppercase tracking-wider">you</span>{/if}
              </div>
              <span class="font-mono text-[10px] uppercase tracking-wider px-2 py-0.5 rounded shrink-0 {modelPill(o.model)}">{modelLabel(o.model)}</span>
            </div>

            {#if o.online}
              <div class="mt-3 grid grid-cols-2 gap-x-3 gap-y-1.5 text-xs font-mono">
                <div class="text-ink-400">view</div>
                <div class="text-ink-200 text-right">{o.view_source ?? '—'}</div>
                <div class="text-ink-400">webcam</div>
                <div class="text-ink-200 text-right">{o.webcam?.source ?? 'off'}</div>
                <div class="text-ink-400">UPS</div>
                <div class="text-ink-200 text-right">{upsText(o)}</div>
                <div class="text-ink-400">temp</div>
                <div class="text-ink-200 text-right">{tempText(o)}</div>
                <div class="text-ink-400">addr</div>
                <div class="text-ink-200 text-right truncate">{o.lan_ip ?? o.addr ?? '—'}</div>
                <div class="text-ink-400">version</div>
                <div class="text-ink-200 text-right">{o.version ?? '—'}</div>
              </div>
              {#if o.sources?.length || o.ipfs_models?.models?.length}
                <div class="mt-2 flex flex-wrap gap-1">
                  {#each o.sources ?? [] as s}
                    <span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-ink-800 text-ink-300">{s}</span>
                  {/each}
                  {#if o.ipfs_models?.models?.length}
                    <a href="/orbnet/ipfs"
                       class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-cursed-900/50 text-cursed-300 hover:bg-cursed-900"
                       title="AI models this Orb shares over IPFS — see the fleet model index">
                      {o.ipfs_models.models.length} model{o.ipfs_models.models.length === 1 ? '' : 's'} 📦
                    </a>
                  {/if}
                </div>
              {/if}
            {:else}
              <div class="mt-3 text-xs font-mono text-ink-500">offline · last addr {o.addr ?? '—'}</div>
            {/if}
          </div>
        {/each}
      </div>

      <div class="panel p-4 space-y-3">
        <div class="font-mono text-sm text-ink-300">This Orb's fleet settings</div>
        <label class="block text-xs font-mono text-ink-400">
          label (what this Orb controls)
          <input
            bind:value={labelInput}
            placeholder="e.g. Pi5 CSI rig"
            class="mt-1 w-full bg-ink-800 border border-steel-700 rounded px-2 py-1 text-ink-100 font-mono text-sm focus:outline-none focus:ring-1 focus:ring-emerald-500"
          />
        </label>
        <label class="block text-xs font-mono text-ink-400">
          peer seeds (one per line — LAN IPs or tailnet names)
          <textarea
            bind:value={seedsInput}
            rows="3"
            class="mt-1 w-full bg-ink-800 border border-steel-700 rounded px-2 py-1 text-ink-100 font-mono text-xs focus:outline-none focus:ring-1 focus:ring-emerald-500"
          ></textarea>
        </label>
        <label class="flex items-center gap-2 text-xs font-mono text-ink-400">
          <input type="checkbox" bind:checked={tailscaleInput} class="accent-emerald-500" />
          also auto-discover peers from Tailscale
        </label>
        <div class="flex items-center gap-3">
          <button
            on:click={saveConfig}
            disabled={saving}
            class="px-3 py-1.5 rounded text-sm font-mono bg-emerald-600/80 hover:bg-emerald-600 text-white disabled:opacity-50 disabled:cursor-wait"
            >save</button
          >
          {#if saveMsg}<span class="text-xs font-mono text-ink-400">{saveMsg}</span>{/if}
        </div>
        <p class="text-[11px] text-ink-500 leading-relaxed">
          The shared fleet <code class="font-mono">token</code> lives on disk (and is synced securely later) — it's never shown
          or edited here.
        </p>
      </div>
    {/if}
  </main>
</div>
