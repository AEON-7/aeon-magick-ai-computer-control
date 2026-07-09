<script lang="ts">
  // v74: on-screen special-keys pad. Sends F-keys / nav keys / BIOS combos
  // straight to the target as HID reports (POST /api/hid/key), so they CANNOT
  // be intercepted by the local browser or OS the way physical F-keys are
  // (macOS maps F1–F12 to brightness/Mission-Control/etc.; F11/F12 are browser
  // shortcuts). This is the reliable path to a boot menu / BIOS / GRUB, where
  // a specific key has to land on the target during POST.
  import * as api from '$lib/api';
  import Icon from './Icon.svelte';

  let open = false;
  let root: HTMLDivElement;
  let flash = ''; // last key sent, shown briefly as feedback
  const close = () => (open = false);
  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) close();
  }

  async function send(keys: string[], label: string) {
    flash = label;
    try {
      await api.sendKey(keys, 40);
    } catch (e) {
      console.warn('sendKey failed', e);
    }
    setTimeout(() => { if (flash === label) flash = ''; }, 700);
  }

  const FN = Array.from({ length: 12 }, (_, i) => `F${i + 1}`);
  const NAV: { keys: string[]; label: string }[] = [
    { keys: ['ESC'], label: 'Esc' },
    { keys: ['TAB'], label: 'Tab' },
    { keys: ['ENTER'], label: 'Enter' },
    { keys: ['BACKSPACE'], label: 'Bksp' },
    { keys: ['DELETE'], label: 'Del' },
    { keys: ['HOME'], label: 'Home' },
    { keys: ['END'], label: 'End' },
    { keys: ['PGUP'], label: 'PgUp' },
    { keys: ['PGDN'], label: 'PgDn' },
    { keys: ['SPACE'], label: 'Space' },
  ];
  const ARROWS: { keys: string[]; label: string }[] = [
    { keys: ['UP'], label: '↑' },
    { keys: ['LEFT'], label: '←' },
    { keys: ['DOWN'], label: '↓' },
    { keys: ['RIGHT'], label: '→' },
  ];
  const COMBOS: { keys: string[]; label: string }[] = [
    { keys: ['CTRL', 'ALT', 'DELETE'], label: 'Ctrl+Alt+Del' },
  ];
</script>

<svelte:window on:click={onWindowClick} />

<div class="relative" bind:this={root}>
  <button
    class="btn text-xs inline-flex items-center gap-1.5"
    on:click|stopPropagation={() => (open = !open)}
    aria-haspopup="menu" aria-expanded={open}
    title="Send F-keys / BIOS keys straight to the target. Use for boot menus, BIOS/UEFI, GRUB — these bypass the browser/OS, which otherwise eats F-keys.">
    <Icon name="keyboard" class="w-3.5 h-3.5" />
    Keys
    <Icon name="chevron" class="w-3 h-3 transition-transform {open ? 'rotate-180' : ''}" />
  </button>

  {#if open}
    <div class="absolute left-0 mt-1 z-40 w-[20rem] max-w-[92vw]
                panel/98 backdrop-blur-sm
                shadow-xl shadow-black/40 p-3 space-y-2.5"
         role="menu">
      <p class="text-[10px] text-zinc-500 leading-snug">
        Sent straight to the target as real key presses — immune to the
        browser/OS swallowing F-keys. Use for boot menus &amp; BIOS.
        {#if flash}<span class="text-live-300 font-mono ml-1">sent: {flash}</span>{/if}
      </p>

      <div class="grid grid-cols-6 gap-1">
        {#each FN as f}
          <button class="px-1 py-1.5 rounded bg-ink-800 hover:bg-cursed-500/20 hover:text-cursed-200
                         border border-steel-700 text-[11px] font-mono text-zinc-300 transition-colors"
                  on:click={() => send([f], f)}>{f}</button>
        {/each}
      </div>

      <div class="flex flex-wrap gap-1">
        {#each NAV as n}
          <button class="px-2 py-1.5 rounded bg-ink-800 hover:bg-cursed-500/20 hover:text-cursed-200
                         border border-steel-700 text-[11px] text-zinc-300 transition-colors"
                  on:click={() => send(n.keys, n.label)}>{n.label}</button>
        {/each}
      </div>

      <div class="flex items-center gap-2">
        <div class="flex gap-1">
          {#each ARROWS as a}
            <button class="w-8 py-1.5 rounded bg-ink-800 hover:bg-cursed-500/20 hover:text-cursed-200
                           border border-steel-700 text-sm text-zinc-300 transition-colors"
                    on:click={() => send(a.keys, a.label)}>{a.label}</button>
          {/each}
        </div>
        <div class="flex-1"></div>
        {#each COMBOS as c}
          <button class="px-2 py-1.5 rounded bg-red-500/10 hover:bg-red-500/20 text-red-300
                         border border-red-500/40 text-[11px] font-mono transition-colors"
                  on:click={() => send(c.keys, c.label)}>{c.label}</button>
        {/each}
      </div>
    </div>
  {/if}
</div>
