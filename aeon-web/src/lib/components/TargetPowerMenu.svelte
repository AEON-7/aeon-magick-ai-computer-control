<script lang="ts">
  // v74: consolidates the four loose target-power buttons (wake / tap /
  // reboot / force-off) into one "Target Power" dropdown. These act on the
  // USB-CONNECTED MACHINE via the HID consumer power button + WoL — NOT the
  // Pi (Pi maintenance lives on /system). Handlers are passed in so this stays
  // a dumb presentational dropdown; the page owns the confirm dialogs + API.
  import Icon from './Icon.svelte';

  export let onWake: () => void;
  export let onTap: () => void;
  export let onReboot: () => void;
  export let onForceOff: () => void;
  /** Full-width trigger for the mobile menu. */
  export let block = false;

  let open = false;
  let root: HTMLDivElement;
  const close = () => (open = false);
  const run = (fn: () => void) => { close(); fn(); };
  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) close();
  }

  const ACTIONS = [
    { fn: () => run(onWake),     icon: 'zap',    label: 'Wake',        desc: 'WoL magic packet', tone: 'hover:bg-live-500/15 hover:text-live-300' },
    { fn: () => run(onTap),      icon: 'power',  label: 'Tap Power',   desc: 'graceful — OS handles it', tone: 'hover:bg-zinc-500/20 hover:text-zinc-100' },
    { fn: () => run(onReboot),   icon: 'reboot', label: 'Reboot',      desc: 'force-off → wait → WoL', tone: 'hover:bg-amber-500/15 hover:text-amber-300' },
    { fn: () => run(onForceOff), icon: 'power',  label: 'Force Off',   desc: 'hold 8s — hard shutdown', tone: 'hover:bg-red-500/15 hover:text-red-300' },
  ];
</script>

<svelte:window on:click={onWindowClick} />

<div class="relative {block ? 'w-full' : ''}" bind:this={root}>
  <button
    class="btn text-xs inline-flex items-center gap-1.5 {block ? 'w-full justify-center' : ''}"
    on:click|stopPropagation={() => (open = !open)}
    aria-haspopup="menu" aria-expanded={open}
    title="Power controls for the USB-connected target machine">
    <Icon name="power" class="w-3.5 h-3.5" />
    Target&nbsp;Power
    <Icon name="chevron" class="w-3 h-3 transition-transform {open ? 'rotate-180' : ''}" />
  </button>

  {#if open}
    <div class="absolute {block ? 'left-0 right-0' : 'right-0'} mt-1 z-40 min-w-[14rem]
                panel bg-ink-900/98 backdrop-blur-sm
                shadow-xl shadow-black/40 p-1 space-y-0.5"
         role="menu">
      {#each ACTIONS as a}
        <button role="menuitem"
                class="w-full flex items-center gap-2.5 px-2.5 py-2 rounded-md
                       text-left text-zinc-300 transition-colors {a.tone}"
                on:click={a.fn}>
          <Icon name={a.icon} class="w-4 h-4 flex-shrink-0" />
          <span class="flex-1 min-w-0">
            <span class="block text-sm leading-tight">{a.label}</span>
            <span class="block text-[10px] text-zinc-500 leading-tight">{a.desc}</span>
          </span>
        </button>
      {/each}
    </div>
  {/if}
</div>
