<script lang="ts">
  // Shared sub-page header — the entablature every page sits under.
  //
  // Two jobs. Visually it's the inscribed band that caps the page: orb sigil,
  // struck index, monumental title, a gilt line beneath. Functionally it carries
  // the ONLY lateral navigation on a sub-page — before this, every one of the 27
  // sub-pages was a leaf and you had to go home to go anywhere.
  import { page } from '$app/stores';
  import OrbMark from './OrbMark.svelte';
  import Icon from './Icon.svelte';
  import { SUPER_APPS, SETTINGS_ITEMS, MONITOR_ITEMS } from '$lib/nav';

  export let title: string;
  export let subtitle = '';
  export let backHref = '/';
  export let backLabel = 'AEON MAGICK';
  /** Optional instrument index shown before the title, e.g. "02" */
  export let index = '';

  let navOpen = false;
  $: here = $page?.url?.pathname ?? '';
  const close = () => (navOpen = false);

  const GROUPS = [
    { label: 'Modules', items: SUPER_APPS },
    { label: 'Settings', items: SETTINGS_ITEMS },
    { label: 'Monitor', items: MONITOR_ITEMS },
  ];
</script>

<svelte:window on:keydown={(e) => e.key === 'Escape' && close()} />

<header class="chrome-header relative">
  <div class="chrome-sigil" aria-hidden="true"></div>
  <div class="flex items-center justify-between gap-3 px-3ru sm:px-6ru py-3ru">
    <div class="flex items-center gap-3ru min-w-0">
      <a
        href={backHref}
        class="group flex items-center gap-2ru text-cursed-400 font-mono text-xs tracking-instrument
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
      <div class="min-w-0 flex items-baseline gap-2ru">
        {#if index}
          <span class="font-mono text-2xs text-cursed-500/70 shrink-0 tabular-nums">{index}</span>
        {/if}
        <!-- The page's one <h1>. Was a <span>, so 20 of 28 routes shipped with no
             heading at all for assistive tech. -->
        <h1 class="inscription-sm text-zinc-100 truncate">{title}</h1>
        {#if subtitle}
          <span class="hidden md:inline text-2xs text-zinc-500 font-mono truncate">// {subtitle}</span>
        {/if}
      </div>
    </div>
    <div class="flex items-center gap-2ru shrink-0">
      <slot />
      <!-- Lateral nav — the fix for "every sub-page is a dead end". -->
      <button
        class="btn btn-xs"
        aria-haspopup="menu"
        aria-expanded={navOpen}
        aria-label="Open navigation"
        on:click|stopPropagation={() => (navOpen = !navOpen)}
      >
        <Icon name="menu" class="w-3.5 h-3.5" />
        <span class="hidden sm:inline">go to</span>
      </button>
    </div>
  </div>
  <div class="entablature" aria-hidden="true"></div>

  {#if navOpen}
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="fixed inset-0 z-40" on:click={close} role="presentation"></div>
    <div
      class="absolute right-2 sm:right-4 top-full mt-1 z-50 w-[19rem] max-w-[94vw]
             stele-lit has-aether p-4ru space-y-4ru max-h-[75dvh] overflow-y-auto"
      role="menu"
      tabindex="-1"
    >
      {#each GROUPS as g}
        <div class="space-y-2ru">
          <div class="rack-label">{g.label}</div>
          <div class="grid grid-cols-2 gap-1.5">
            {#each g.items as it}
              {@const active = here === it.href}
              <a
                href={it.href}
                on:click={close}
                role="menuitem"
                title={it.title ?? it.label}
                aria-current={active ? 'page' : undefined}
                class="flex items-center gap-2ru rounded-sm border px-2ru py-1.5 min-h-8 text-xs font-mono
                       transition-colors
                       {active
                         ? 'border-cursed-500/50 bg-cursed-500/10 text-cursed-200'
                         : 'border-steel-700 text-zinc-400 hover:border-cursed-500/40 hover:text-zinc-100'}"
              >
                <Icon name={it.icon} class="w-3.5 h-3.5 shrink-0" />
                <span class="truncate">{it.label}</span>
              </a>
            {/each}
          </div>
        </div>
      {/each}
      <p class="text-2xs text-zinc-500 font-mono pt-2ru border-t border-steel-700/60">
        <kbd class="text-cursed-400">⌘K</kbd> anywhere for the command palette
      </p>
    </div>
  {/if}
</header>
