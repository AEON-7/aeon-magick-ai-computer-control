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
  });
</script>

<div class="h-full bg-ink-950 text-zinc-200">
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
