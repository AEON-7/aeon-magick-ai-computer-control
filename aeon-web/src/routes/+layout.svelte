<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import * as api from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';
  import Toasts from '$lib/components/Toasts.svelte';
  import ConfirmHost from '$lib/components/ConfirmHost.svelte';
  import CommandPalette from '$lib/components/CommandPalette.svelte';

  // Pages that should never trigger an auth redirect — these are the
  // landing pages for unauthenticated / setup states themselves.
  const PUBLIC_ROUTES = ['/setup', '/setup/wifi', '/login'];

  let bootstrapping = true;
  let lockedDown = false;

  async function checkLockdown() {
    try {
      const r = await fetch('/api/lockdown', { credentials: 'same-origin' });
      if (r.ok) { const d = await r.json(); lockedDown = !!d.enabled; }
    } catch { /* admin-only; non-admin/unauth just won't see the banner */ }
  }

  onMount(async () => {
    try {
      const me = await api.getMe();
      const path = $page.url.pathname;
      if (me.state === 'open' && me.needs_setup) {
        if (!path.startsWith('/setup')) {
          goto('/setup');
          return;
        }
      } else if (me.state === 'locked' && !me.authenticated) {
        if (!PUBLIC_ROUTES.includes(path)) {
          goto('/login');
          return;
        }
      }
    } catch (e) {
      console.warn('auth probe failed', e);
    } finally {
      bootstrapping = false;
    }
    checkLockdown();
    setInterval(checkLockdown, 8000);
  });
</script>

<div class="h-full flex flex-col bg-ink-950 text-zinc-200">
  {#if lockedDown}
    <div class="bg-red-700 text-white text-center text-[11px] font-mono py-1 tracking-widest motion-safe:animate-ember shrink-0
                flex items-center justify-center gap-1.5">
      <Icon name="lock" class="w-3 h-3" />
      LOCKDOWN MODE — all external API + MCP disabled · admin session only
    </div>
  {/if}
  <div class="flex-1 min-h-0">
    {#if bootstrapping}
      <!-- Boot ritual — decorates the REAL auth-probe round-trip, never
           pads it: the moment bootstrapping flips, the app renders,
           even mid-animation. -->
      <div class="h-full flex flex-col items-center justify-center gap-5">
        <svg viewBox="0 0 24 24" class="w-10 h-10 text-cursed-400" aria-hidden="true">
          <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="1"
                  pathLength="100" class="aeon-summon" />
          <circle cx="12" cy="12" r="4" fill="currentColor" class="opacity-80" />
        </svg>
        <div class="font-mono text-[11px] tracking-widest text-zinc-500 space-y-1.5"
             aria-label="Loading">
          <p class="aeon-rite" style="animation-delay: 0ms"><span class="text-cursed-400/70">◇</span> waking the orb</p>
          <p class="aeon-rite" style="animation-delay: 350ms"><span class="text-cursed-400/70">◇</span> binding HID</p>
          <p class="aeon-rite" style="animation-delay: 700ms"><span class="text-cursed-400/70">◇</span> attuning stream</p>
        </div>
      </div>
    {:else}
      <slot />
    {/if}
  </div>
</div>

<!-- Global feedback surfaces — toasts, confirm/ask modals, Cmd+K palette. -->
<Toasts />
<ConfirmHost />
<CommandPalette />
