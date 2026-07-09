<script lang="ts">
  // Cmd+K / Ctrl+K command palette — jump to any page from anywhere.
  // Mounted once in +layout.svelte. Stands down while the control page
  // has input captured (every keystroke belongs to the target then) and
  // while a confirm/ask modal is open.
  import { goto } from '$app/navigation';
  import { fly, fade } from 'svelte/transition';
  import { PALETTE_GROUPS, type NavItem } from '$lib/nav';
  import { inputCaptured } from '$lib/capture';
  import { modal } from '$lib/confirm';
  import * as api from '$lib/api';
  import Icon from './Icon.svelte';
  import OrbMark from './OrbMark.svelte';

  const reduceMotion =
    typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;

  type Entry = NavItem & { group: string; action?: () => void };

  const ENTRIES: Entry[] = [
    ...PALETTE_GROUPS.flatMap(({ group, items }) => items.map((it) => ({ ...it, group }))),
    {
      href: '/login',
      label: 'Sign out',
      icon: 'logout',
      title: 'End this admin session',
      group: 'session',
      action: async () => {
        try { await api.logout(); } catch { /* cookie may already be gone */ }
        window.location.href = '/login';
      },
    },
  ];

  let open = false;
  let query = '';
  let sel = 0;
  let inputEl: HTMLInputElement | undefined;

  function score(e: Entry, q: string): number {
    const label = e.label.toLowerCase();
    const hay = `${label} ${e.title ?? ''} ${e.href}`.toLowerCase();
    if (!q) return 1;
    if (label.startsWith(q)) return 100;
    if (label.includes(q)) return 50;
    if (hay.includes(q)) return 10;
    // All query words present somewhere → weak match.
    const words = q.split(/\s+/).filter(Boolean);
    return words.length && words.every((w) => hay.includes(w)) ? 5 : 0;
  }

  $: q = query.trim().toLowerCase();
  $: results = ENTRIES.map((e) => ({ e, s: score(e, q) }))
    .filter((r) => r.s > 0)
    .sort((a, b) => b.s - a.s)
    .map((r) => r.e);
  $: if (sel >= results.length) sel = Math.max(0, results.length - 1);

  function show() {
    open = true;
    query = '';
    sel = 0;
    queueMicrotask(() => inputEl?.focus());
  }
  function hide() {
    open = false;
  }
  function run(e: Entry) {
    hide();
    if (e.action) e.action();
    else goto(e.href);
  }

  function onWindowKeydown(ev: KeyboardEvent) {
    if ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === 'k') {
      // Captured keystrokes belong to the target; modal keystrokes to the
      // modal. In both cases the palette pretends it doesn't exist.
      if ($inputCaptured || $modal) return;
      ev.preventDefault();
      ev.stopPropagation();
      open ? hide() : show();
      return;
    }
    if (!open) return;
    if (ev.key === 'Escape') {
      ev.preventDefault();
      ev.stopPropagation();
      hide();
    } else if (ev.key === 'ArrowDown') {
      ev.preventDefault();
      sel = results.length ? (sel + 1) % results.length : 0;
    } else if (ev.key === 'ArrowUp') {
      ev.preventDefault();
      sel = results.length ? (sel - 1 + results.length) % results.length : 0;
    } else if (ev.key === 'Enter') {
      ev.preventDefault();
      if (results[sel]) run(results[sel]);
    }
  }
</script>

<svelte:window on:keydown|capture={onWindowKeydown} />

{#if open}
  <div
    class="fixed inset-0 z-[75] flex items-start justify-center pt-[14vh] px-4 bg-ink-950/85 backdrop-blur-[2px]"
    transition:fade={{ duration: reduceMotion ? 0 : 100 }}
    on:mousedown|self={hide}
    role="presentation"
  >
    <div
      class="w-full max-w-lg panel-cursed overflow-hidden shadow-depth-void"
      in:fly={{ y: reduceMotion ? 0 : -8, duration: reduceMotion ? 0 : 130 }}
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
    >
      <div class="chrome-sigil" aria-hidden="true"></div>
      <div class="flex items-center gap-2.5 px-4 py-3 border-b border-steel-700 bg-ink-950/30">
        <OrbMark class="w-4 h-4" />
        <input
          bind:this={inputEl}
          bind:value={query}
          type="text"
          placeholder="Jump to… / search pages"
          autocomplete="off"
          spellcheck="false"
          class="flex-1 bg-transparent border-0 ring-0 focus:ring-0 text-sm font-mono text-zinc-200 placeholder-zinc-600 focus:outline-none"
          aria-label="Search pages"
        />
        <kbd class="text-2xs font-mono text-zinc-600 border border-steel-600 rounded-sm px-1.5 py-0.5 tracking-instrument">esc</kbd>
      </div>
      <ul class="max-h-[46vh] overflow-y-auto p-1" role="listbox">
        {#each results as e, i (e.group + e.href)}
          <li role="option" aria-selected={i === sel}>
            <button
              class="w-full flex items-center gap-2.5 px-2.5 py-2 rounded-sm text-left text-sm
                     {i === sel ? 'bg-cursed-600/20 text-cursed-100 border border-cursed-500/25' : 'text-zinc-300 hover:bg-ink-800 border border-transparent'}"
              on:click={() => run(e)}
              on:mousemove={() => (sel = i)}
            >
              <Icon name={e.icon} class="w-4 h-4 {i === sel ? 'text-cursed-300' : 'text-zinc-500'}" />
              <span class="shrink-0 font-mono text-xs tracking-wide">{e.label}</span>
              {#if e.title}
                <span class="text-2xs text-zinc-600 truncate flex-1">{e.title}</span>
              {/if}
              <span class="text-2xs font-mono uppercase tracking-instrument text-zinc-600 shrink-0">{e.group}</span>
            </button>
          </li>
        {:else}
          <li class="void-empty !border-0 !bg-transparent !py-8">
            <p class="void-empty-title">no match</p>
            <p class="void-empty-body">Try a page name, group, or path fragment.</p>
          </li>
        {/each}
      </ul>
      <div class="px-3 py-1.5 border-t border-steel-700/80 bg-ink-950/40 flex justify-between font-mono text-2xs text-zinc-600 tracking-instrument uppercase">
        <span>⌘K / Ctrl+K</span>
        <span>enter · jump</span>
      </div>
    </div>
  </div>
{/if}
