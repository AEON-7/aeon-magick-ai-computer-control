<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import * as api from '$lib/api';

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
    <div class="bg-red-700 text-white text-center text-[11px] font-mono py-1 tracking-widest animate-pulse shrink-0">
      🔒 LOCKDOWN MODE — all external API + MCP disabled · admin session only
    </div>
  {/if}
  <div class="flex-1 min-h-0">
    {#if bootstrapping}
      <div class="h-full flex items-center justify-center">
        <span class="font-mono text-xs text-zinc-500 tracking-widest">
          AEON MAGICK · loading…
        </span>
      </div>
    {:else}
      <slot />
    {/if}
  </div>
</div>
