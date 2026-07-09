<script lang="ts">
  // Toast stack — mounted once in +layout.svelte. Sits top-right below
  // the header, above everything (z-[70]) but pointer-transparent
  // except the toasts themselves, so it can never eat canvas input.
  import { fly } from 'svelte/transition';
  import { toasts, dismiss, type ToastKind } from '$lib/toast';
  import OrbMark from './OrbMark.svelte';

  const reduceMotion =
    typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;

  const STYLE: Record<ToastKind, string> = {
    info: 'border-cursed-500/40 text-cursed-100',
    success: 'border-live-500/40 text-live-400',
    error: 'border-red-500/50 text-red-200',
  };
  const DOT: Record<ToastKind, string> = {
    info: 'dot-cursed',
    success: 'dot-live',
    error: 'dot-off',
  };
</script>

{#if $toasts.length}
  <div class="fixed top-14 right-3 z-[70] flex flex-col items-end gap-2 pointer-events-none max-w-[min(24rem,calc(100vw-1.5rem))]"
       role="status" aria-live="polite">
    {#each $toasts as t (t.id)}
      <button
        class="pointer-events-auto flex items-start gap-2.5 px-3 py-2 rounded-sm
               bg-ink-900/95 border shadow-plate text-left
               font-mono text-xs leading-relaxed {STYLE[t.kind]}"
        in:fly={{ y: -6, duration: reduceMotion ? 0 : 150 }}
        on:click={() => dismiss(t.id)}
        title="Dismiss"
      >
        {#if t.kind === 'error'}
          <OrbMark mode="offline" class="w-3.5 h-3.5 mt-0.5" />
        {:else}
          <span class="mt-1.5 shrink-0 {DOT[t.kind]}"></span>
        {/if}
        <span class="whitespace-pre-line break-words min-w-0">{t.message}</span>
      </button>
    {/each}
  </div>
{/if}
