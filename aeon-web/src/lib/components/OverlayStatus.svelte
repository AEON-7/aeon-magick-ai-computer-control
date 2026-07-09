<script lang="ts">
  // Presentational live-status card for a single network overlay
  // (clearnet VPN, Tor, I2P, or the Tailscale mesh). Extracted from
  // network/+page.svelte so each service's status can render under its
  // own section instead of all overlays sharing one block in the VPN panel.
  import type { VpnStatusOverlay } from '$lib/api';

  export let overlay: VpnStatusOverlay;
  export let label: string;
  export let rotating = false;
  export let rotateMsg = '';
  // When null, no rotate button is shown (e.g. Tailscale status).
  export let onRotate: (() => void) | null = null;
</script>

<div class="space-y-3 p-3 rounded-sm border border-ink-800 bg-ink-950/40">
  <header class="flex items-center justify-between gap-2">
    <div class="flex items-center gap-2 flex-wrap">
      <span class="font-mono text-xs uppercase tracking-wider text-zinc-300">
        {label}
      </span>
      {#if overlay.state === 'connected'}
        <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                     bg-live-900/40 border border-live-500/40
                     text-live-300 text-[10px] font-mono uppercase">
          <span class="h-1.5 w-1.5 rounded-full bg-live-400 animate-pulse"></span>
          connected
        </span>
      {:else if overlay.state === 'establishing'}
        <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                     bg-amber-900/40 border border-amber-500/40
                     text-amber-300 text-[10px] font-mono uppercase">
          <span class="h-1.5 w-1.5 rounded-full bg-amber-400 animate-pulse"></span>
          establishing
        </span>
      {:else if overlay.state === 'reconnecting'}
        <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                     bg-amber-900/40 border border-amber-500/40
                     text-amber-300 text-[10px] font-mono uppercase">
          reconnecting
        </span>
      {:else}
        <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                     bg-red-900/40 border border-red-500/40
                     text-red-300 text-[10px] font-mono uppercase">
          <span class="h-1.5 w-1.5 rounded-full bg-red-400"></span>
          {overlay.state}
        </span>
      {/if}
    </div>

    {#if onRotate}
      <button class="btn text-xs whitespace-nowrap"
              on:click={onRotate}
              disabled={rotating || overlay.state !== 'connected'}
              title="Refresh identity / circuits / keys">
        {rotating ? 'rotating…' : '↻ change identity'}
      </button>
    {/if}
  </header>

  {#if rotateMsg}
    <p class="text-xs font-mono text-zinc-400">{rotateMsg}</p>
  {/if}

  <p class="text-xs text-zinc-400">{overlay.summary}</p>

  <!-- Bootstrap progress bar (Tor / I2P) -->
  {#if overlay.bootstrap_percent !== null && overlay.bootstrap_percent !== undefined && overlay.bootstrap_percent < 100}
    <div class="space-y-1">
      <div class="flex justify-between text-[10px] font-mono text-zinc-500">
        <span>BOOTSTRAP</span>
        <span>{overlay.bootstrap_percent}%</span>
      </div>
      <div class="h-1.5 rounded-full bg-ink-800 overflow-hidden">
        <div class="h-full bg-cursed-500 transition-all duration-300"
             style="width: {overlay.bootstrap_percent}%"></div>
      </div>
    </div>
  {/if}

  <!-- Public IP + country -->
  {#if overlay.public_ip}
    <div class="flex items-center gap-3 text-xs font-mono">
      <span class="text-zinc-500">Public IP:</span>
      <span class="text-zinc-200">{overlay.public_ip}</span>
      {#if overlay.public_country}
        <span class="text-cursed-300 uppercase tracking-wider">
          {overlay.public_country}
        </span>
      {/if}
    </div>
  {/if}

  <!-- Tor circuit list -->
  {#if overlay.detail.circuits && overlay.detail.circuits.length > 0}
    <div class="space-y-1">
      <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">
        Active circuits ({overlay.detail.circuits.length})
      </p>
      <div class="space-y-0.5 max-h-32 overflow-y-auto font-mono text-[10px] text-zinc-400">
        {#each overlay.detail.circuits.slice(0, 6) as c}
          <div class="truncate">
            <span class="text-zinc-600">#{c.id}</span>
            {c.hops.join(' → ')}
          </div>
        {/each}
      </div>
    </div>
  {/if}

  <!-- Tailscale peer list — Tailscale-only. WireGuard
       providers (wireguard/mullvad/ivpn) also expose a WG
       "peer" but with no host/ips, which rendered here as
       "undefined undefined". Gate strictly to tailscale. -->
  {#if overlay.provider === 'tailscale' && overlay.detail.peers && overlay.detail.peers.length > 0}
    <div class="space-y-1">
      <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">
        Tailnet peers ({overlay.detail.peers.length})
      </p>
      <div class="space-y-0.5 max-h-32 overflow-y-auto font-mono text-[10px]">
        {#each overlay.detail.peers as p}
          <div class="flex items-center gap-2 truncate">
            <span class={p.online ? 'text-live-400' : 'text-zinc-600'}>●</span>
            <span class="text-zinc-300">{p.host}</span>
            <span class="text-zinc-500">{p.ips?.[0]}</span>
            {#if p.exit_node}
              <span class="text-cursed-400 text-[9px]">[exit]</span>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  {/if}

  <!-- I2P peer count -->
  {#if overlay.detail.active_peers !== undefined}
    <p class="text-xs font-mono text-zinc-400">
      <span class="text-zinc-500">Active peers:</span>
      {overlay.detail.active_peers}
    </p>
  {/if}

  <!-- WireGuard handshake age -->
  {#if overlay.detail.handshake_age_s !== undefined && overlay.detail.handshake_age_s !== null}
    <p class="text-xs font-mono text-zinc-400">
      <span class="text-zinc-500">Last handshake:</span>
      {overlay.detail.handshake_age_s}s ago
    </p>
  {/if}
</div>
