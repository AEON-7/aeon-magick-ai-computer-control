<script lang="ts">
  // Modal host for confirmRite()/askRite() — mounted once in +layout.svelte.
  // Enter confirms (when unlocked), Esc cancels. The window keydown
  // listener only exists while a modal is open, and runs in the capture
  // phase with stopPropagation so the control page's key-forwarding
  // never sees keystrokes meant for the dialog.
  import { fly, fade } from 'svelte/transition';
  import { modal, settle, type ModalRequest } from '$lib/confirm';
  import OrbMark from './OrbMark.svelte';

  const reduceMotion =
    typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;

  let phraseInput = '';
  let askInput = '';
  let confirmBtn: HTMLButtonElement | undefined;
  let textInput: HTMLInputElement | undefined;

  let current: ModalRequest | null = null;
  $: current = $modal;

  // Reset per-dialog state + focus whenever a new request arrives.
  $: if (current) {
    phraseInput = '';
    askInput = current.kind === 'ask' ? (current.opts.initial ?? '') : '';
    queueMicrotask(() => (textInput ?? confirmBtn)?.focus());
  }

  $: phraseOk =
    !current || current.kind !== 'confirm' || !current.opts.phrase
      ? true
      : phraseInput === current.opts.phrase;

  function finish(ok: boolean) {
    if (!current) return;
    const req = current;
    if (req.kind === 'confirm') req.resolve(ok);
    else req.resolve(ok ? askInput : null);
    settle();
  }

  function accept() {
    if (phraseOk) finish(true);
  }

  // Bound unconditionally (Svelte binds handlers once at mount); the
  // guard makes it a no-op — and lets keys propagate — while closed.
  function onWindowKeydown(ev: KeyboardEvent) {
    if (!current) return;
    ev.stopPropagation();
    if (ev.key === 'Escape') {
      ev.preventDefault();
      finish(false);
    } else if (ev.key === 'Enter') {
      ev.preventDefault();
      accept();
    }
  }
</script>

<svelte:window on:keydown|capture={onWindowKeydown} />

{#if current}
  {@const opts = current.opts}
  {@const danger = 'danger' in opts && !!opts.danger}
  <div
    class="fixed inset-0 z-[80] flex items-center justify-center p-4 bg-ink-950/70 backdrop-blur-sm"
    transition:fade={{ duration: reduceMotion ? 0 : 120 }}
    on:mousedown|self={() => finish(false)}
    role="presentation"
  >
    <div
      class="w-full max-w-md bg-ink-900 border rounded-2xl p-6 space-y-4 shadow-2xl
             {danger ? 'border-red-500/50 shadow-red-950/40' : 'border-cursed-500/40 shadow-cursed-800/20'}"
      in:fly={{ y: reduceMotion ? 0 : 8, duration: reduceMotion ? 0 : 150 }}
      role="dialog"
      aria-modal="true"
      aria-label={opts.title}
    >
      <div class="flex items-center gap-2.5">
        <OrbMark mode={danger ? 'offline' : 'idle'} class="w-5 h-5" />
        <h2 class="font-mono text-sm uppercase tracking-wider {danger ? 'text-red-300' : 'text-cursed-300'}">
          {opts.title}
        </h2>
      </div>

      {#if opts.body}
        <p class="text-sm text-zinc-400 leading-relaxed whitespace-pre-line">{opts.body}</p>
      {/if}

      {#if current.kind === 'ask'}
        {@const ask = current.opts}
        <label class="block">
          {#if ask.label}
            <span class="text-xs uppercase tracking-wider text-zinc-500">{ask.label}</span>
          {/if}
          <input
            bind:this={textInput}
            bind:value={askInput}
            type="text"
            placeholder={ask.placeholder ?? ''}
            autocomplete="off"
            spellcheck="false"
            class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-ink-700 text-sm
                   focus:outline-none focus:ring-2 focus:ring-cursed-500 focus:border-transparent"
          />
        </label>
      {:else if current.kind === 'confirm' && current.opts.phrase}
        {@const phrase = current.opts.phrase}
        <label class="block">
          <span class="text-xs uppercase tracking-wider text-zinc-500">
            type <span class="font-mono text-red-300 select-none">{phrase}</span> to confirm
          </span>
          <input
            bind:this={textInput}
            bind:value={phraseInput}
            type="text"
            placeholder={phrase}
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border font-mono text-sm tracking-widest
                   focus:outline-none focus:ring-2 focus:border-transparent
                   {phraseOk ? 'border-red-500/60 focus:ring-red-500 text-red-200' : 'border-ink-700 focus:ring-red-500/60'}"
          />
        </label>
      {/if}

      <div class="flex justify-end gap-2 pt-1">
        <button class="btn text-xs" on:click={() => finish(false)}>
          {opts.cancelLabel ?? 'cancel'}
        </button>
        <button
          bind:this={confirmBtn}
          class="text-xs px-4 py-2 rounded-md font-medium transition-colors
                 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-offset-ink-900
                 disabled:opacity-40 disabled:cursor-not-allowed
                 {danger
                   ? 'bg-red-700 hover:bg-red-600 text-white focus:ring-red-500'
                   : 'bg-cursed-600 hover:bg-cursed-500 text-white focus:ring-cursed-500'}"
          disabled={!phraseOk}
          on:click={accept}
        >
          {opts.confirmLabel ?? 'confirm'}
        </button>
      </div>
    </div>
  </div>
{/if}
