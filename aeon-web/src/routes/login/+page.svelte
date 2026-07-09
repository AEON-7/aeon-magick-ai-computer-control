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

<div class="page-void h-full flex items-center justify-center p-6">
  <div class="void-frame" aria-hidden="true">
    <span class="void-frame-tr"></span>
    <span class="void-frame-bl"></span>
  </div>

  <form
    on:submit|preventDefault={submit}
    class="panel-bracket w-full max-w-sm p-7 space-y-5 relative z-10 shadow-depth-void"
  >
    <div class="status-strip-cursed -mx-7 -mt-7 mb-1" aria-hidden="true"></div>

    <div class="flex items-center gap-3.5 pt-1">
      <div class="relative">
        <OrbMark class="w-10 h-10" />
        <span
          class="absolute -inset-1 rounded-full border border-cursed-500/20 motion-safe:animate-orb-breathe pointer-events-none"
          aria-hidden="true"
        ></span>
      </div>
      <div class="min-w-0">
        <p class="rack-label text-cursed-500/80">01 · access rite</p>
        <h1 class="text-cursed-200 font-mono text-sm tracking-rite uppercase leading-tight">
          AEON MAGICK
        </h1>
        <p class="text-2xs text-zinc-500 font-mono tracking-wider mt-0.5">THE ORB AWAITS A KEY</p>
      </div>
    </div>

    <p class="text-zinc-500 text-xs font-mono leading-relaxed border-l border-cursed-500/30 pl-3">
      Gaze into the orb — authenticate to bind this session. Hands and eyes stay dark until you do.
    </p>

    <label class="block">
      <span class="field-label">user</span>
      <input type="text" bind:value={username} autocomplete="username" class="field" />
    </label>

    <label class="block">
      <span class="field-label">password</span>
      <input
        type="password"
        bind:value={password}
        autocomplete="current-password"
        class="field"
      />
    </label>

    {#if error}
      <p class="callout-fault">{error}</p>
    {/if}

    <button type="submit" disabled={busy || !password} class="btn-primary w-full tracking-wider uppercase text-xs">
      {busy ? 'binding…' : 'bind session'}
    </button>

    <p class="text-center font-mono text-2xs text-zinc-600 tracking-instrument">
      self-signed tls · per-device credential
    </p>
  </form>
</div>
