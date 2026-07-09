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
    class="fixed inset-0 z-[80] flex items-center justify-center p-4 bg-ink-950/80"
    transition:fade={{ duration: reduceMotion ? 0 : 120 }}
    on:mousedown|self={() => finish(false)}
    role="presentation"
  >
    <div
      class="w-full max-w-md panel p-6 space-y-4
             {danger ? 'panel-danger' : 'panel-cursed'}"
      in:fly={{ y: reduceMotion ? 0 : 8, duration: reduceMotion ? 0 : 150 }}
      role="dialog"
      aria-modal="true"
      aria-label={opts.title}
    >
      <div class="{danger ? 'status-strip-fault' : 'status-strip-cursed'} -mx-6 -mt-6 mb-1" aria-hidden="true"></div>
      <div class="flex items-center gap-2.5 pt-1">
        <OrbMark mode={danger ? 'offline' : 'idle'} class="w-5 h-5" />
        <div>
          <p class="rack-label {danger ? 'text-red-500/80' : 'text-cursed-500/80'}">
            {danger ? 'confirm · danger' : 'confirm'}
          </p>
          <h2 class="font-mono text-sm uppercase tracking-wider {danger ? 'text-red-300' : 'text-cursed-300'}">
            {opts.title}
          </h2>
        </div>
      </div>

      {#if opts.body}
        <p class="text-sm text-zinc-400 leading-relaxed whitespace-pre-line">{opts.body}</p>
      {/if}

      {#if current.kind === 'ask'}
        {@const ask = current.opts}
        <label class="block">
          {#if ask.label}
            <span class="field-label">{ask.label}</span>
          {/if}
          <input
            bind:this={textInput}
            bind:value={askInput}
            type="text"
            placeholder={ask.placeholder ?? ''}
            autocomplete="off"
            spellcheck="false"
            class="field"
          />
        </label>
      {:else if current.kind === 'confirm' && current.opts.phrase}
        {@const phrase = current.opts.phrase}
        <label class="block">
          <span class="field-label">
            type <span class="text-red-300 select-none">{phrase}</span> to confirm
          </span>
          <input
            bind:this={textInput}
            bind:value={phraseInput}
            type="text"
            placeholder={phrase}
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            class="field font-mono tracking-widest
                   {phraseOk ? 'border-red-500/60 focus:ring-red-500 text-red-200' : ''}"
          />
        </label>
      {/if}

      <div class="flex justify-end gap-2 pt-1">
        <button class="btn btn-sm" on:click={() => finish(false)}>
          {opts.cancelLabel ?? 'cancel'}
        </button>
        <button
          bind:this={confirmBtn}
          class="{danger ? 'btn-danger' : 'btn-primary'} btn-sm disabled:opacity-40"
          disabled={!phraseOk}
          on:click={accept}
        >
          {opts.confirmLabel ?? 'confirm'}
        </button>
      </div>
    </div>
  </div>
{/if}
