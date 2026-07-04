<script lang="ts">
  import { goto } from '$app/navigation';
  import * as api from '$lib/api';
  import OrbMark from '$lib/components/OrbMark.svelte';

  let username = 'admin';
  let password = '';
  let busy = false;
  let error = '';

  async function submit() {
    busy = true;
    error = '';
    try {
      await api.login(username, password);
      goto('/');
    } catch (e: any) {
      error = e?.message ?? 'login failed';
    } finally {
      busy = false;
    }
  }
</script>

<div class="h-full flex items-center justify-center p-6">
  <form
    on:submit|preventDefault={submit}
    class="w-full max-w-sm bg-ink-900 border border-ink-700 rounded-2xl p-8 space-y-5"
  >
    <div class="flex items-center gap-3">
      <OrbMark class="w-8 h-8" />
      <h1 class="text-cursed-400 font-mono text-lg tracking-widest">AEON MAGICK AI COMPUTER CONTROL</h1>
    </div>
    <p class="text-zinc-400 text-sm">Gaze into the orb — sign in to access the session.</p>

    <label class="block">
      <span class="text-xs uppercase tracking-wider text-zinc-500">user</span>
      <input
        type="text"
        bind:value={username}
        autocomplete="username"
        class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-ink-700
               focus:outline-none focus:ring-2 focus:ring-cursed-500 focus:border-transparent"
      />
    </label>

    <label class="block">
      <span class="text-xs uppercase tracking-wider text-zinc-500">password</span>
      <input
        type="password"
        bind:value={password}
        autocomplete="current-password"
        class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-ink-700
               focus:outline-none focus:ring-2 focus:ring-cursed-500 focus:border-transparent"
      />
    </label>

    {#if error}
      <p class="text-red-400 text-sm">{error}</p>
    {/if}

    <button
      type="submit"
      disabled={busy || !password}
      class="btn-primary w-full disabled:opacity-50"
    >
      {busy ? 'signing in…' : 'sign in'}
    </button>
  </form>
</div>
