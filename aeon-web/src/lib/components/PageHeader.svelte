<script lang="ts">
  // Shared sub-page header — every page off the console uses this
  // instead of hand-rolling the same back-link + title bar. The slot
  // carries page-specific actions on the right.
  import OrbMark from './OrbMark.svelte';

  export let title: string;
  export let subtitle = '';
  export let backHref = '/';
  export let backLabel = 'AEON MAGICK';
</script>

<header class="flex items-center justify-between gap-3 px-3 sm:px-5 py-3 border-b border-ink-700 bg-ink-900 shrink-0">
  <div class="flex items-center gap-3 min-w-0">
    <a
      href={backHref}
      class="group flex items-center gap-2 text-cursed-400 font-mono text-sm tracking-widest
             hover:text-cursed-300 transition-colors shrink-0"
      title="Back to the console"
    >
      <span class="text-cursed-500/70 motion-safe:transition-transform motion-safe:group-hover:-translate-x-0.5"
            aria-hidden="true">←</span>
      <OrbMark class="w-4 h-4" />
      <span class="hidden sm:inline">{backLabel}</span>
    </a>
    <span class="h-4 w-px bg-ink-600 shrink-0" aria-hidden="true"></span>
    <span class="text-zinc-300 font-mono text-xs uppercase tracking-wider truncate">{title}</span>
    {#if subtitle}
      <span class="hidden md:inline text-[11px] text-zinc-500 truncate">{subtitle}</span>
    {/if}
  </div>
  <div class="flex items-center gap-2 shrink-0">
    <slot />
  </div>
</header>
