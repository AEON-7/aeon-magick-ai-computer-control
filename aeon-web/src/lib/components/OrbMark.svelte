<script lang="ts" context="module">
  // Per-instance gradient ids — two orbs on one page with the same id
  // would make one render black. A module counter is deterministic
  // across the SSR fallback shell + hydration (same mount order).
  let instances = 0;
</script>

<script lang="ts">
  // The living orb — the brand mark that IS the status indicator.
  // currentColor does all theming: the tint class sets the color, the
  // halo + core gradient inherit it. Animation is transform+opacity
  // only (compositor-composited) and gated behind motion-safe.
  export let mode: 'idle' | 'live' | 'offline' | 'captured' = 'idle';
  let cls = 'w-4 h-4';
  export { cls as class };

  const uid = `aeon-orb-${++instances}`;

  const TINT: Record<string, string> = {
    idle: 'text-cursed-400',
    live: 'text-live-400',
    offline: 'text-red-500/70',
    captured: 'text-red-400',
  };
</script>

<svg viewBox="0 0 24 24" class="{cls} {TINT[mode]} shrink-0" aria-hidden="true">
  <defs>
    <radialGradient id={uid}>
      <stop offset="0%" stop-color="#e9e4ff" />
      <stop offset="100%" stop-color="currentColor" />
    </radialGradient>
  </defs>
  <!-- transform-box is required or Safari scales the halo from the
       SVG's 0,0 corner instead of its own center. -->
  <circle
    cx="12" cy="12" r="9"
    fill="currentColor"
    class="opacity-30 motion-safe:animate-orb-breathe"
    style="transform-box: fill-box; transform-origin: center;"
  />
  <circle cx="12" cy="12" r="5.5" fill="url(#{uid})" />
  {#if mode === 'captured'}
    <circle cx="12" cy="12" r="10.5" fill="none" stroke="currentColor" stroke-width="1" class="opacity-60" />
  {/if}
</svg>
