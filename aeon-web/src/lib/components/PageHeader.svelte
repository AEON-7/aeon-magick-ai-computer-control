<script lang="ts">
  // Shared sub-page header — rack chrome for every page off the console.
  import OrbMark from './OrbMark.svelte';

  export let title: string;
  export let subtitle = '';
  export let backHref = '/';
  export let backLabel = 'AEON MAGICK';
  /** Optional instrument index shown before the title, e.g. "02" */
  export let index = '';
</script>

<header class="chrome-header">
  <div class="chrome-sigil" aria-hidden="true"></div>
  <div class="flex items-center justify-between gap-3 px-3 sm:px-5 py-2.5">
    <div class="flex items-center gap-3 min-w-0">
      <a
        href={backHref}
        class="group flex items-center gap-2 text-cursed-400 font-mono text-xs tracking-instrument
               hover:text-cursed-300 transition-colors shrink-0"
        title="Back"
      >
        <span
          class="text-cursed-500/80 motion-safe:transition-transform motion-safe:group-hover:-translate-x-0.5"
          aria-hidden="true">←</span
        >
        <OrbMark class="w-4 h-4" />
        <span class="hidden sm:inline uppercase">{backLabel}</span>
      </a>
      <span class="h-4 w-px bg-steel-600 shrink-0" aria-hidden="true"></span>
      <div class="min-w-0 flex items-baseline gap-2">
        {#if index}
          <span class="font-mono text-2xs text-cursed-500/70 shrink-0 tabular-nums">{index}</span>
        {/if}
        <span class="text-zinc-100 font-mono text-xs uppercase tracking-wider truncate">{title}</span>
        {#if subtitle}
          <span class="hidden md:inline text-2xs text-zinc-500 font-mono truncate">// {subtitle}</span>
        {/if}
      </div>
    </div>
    <div class="flex items-center gap-2 shrink-0">
      <slot />
    </div>
  </div>
</header>
