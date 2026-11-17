<script lang="ts">
  // First-boot password setup wizard. Shown when the device is in OPEN
  // state (no password set yet). Once the user submits a password, the
  // device transitions to LOCKED and the wizard auto-logs them in via a
  // session cookie set by the server.

  import { goto } from '$app/navigation';
  import * as api from '$lib/api';

  let username = 'admin';
  let password = '';
  let confirm = '';
  let busy = false;
  let error = '';
  // The per-device setup code (aeon-credentials.txt on the SD card). Only
  // asked for once the first-boot window has passed — the server says when.
  let code = '';
  let codeRequired = false;
  api.getMe().then((m: any) => { codeRequired = !!m?.setup_code_required; }).catch(() => {});

  $: tooShort = password.length > 0 && password.length < 8;
  $: mismatch = confirm.length > 0 && password !== confirm;
  $: canSubmit = !busy && password.length >= 8 && password === confirm && (!codeRequired || code.trim().length >= 6);

  async function submit() {
    busy = true;
    error = '';
    try {
      await api.setupPassword(password, username || 'admin', code.trim());
      // Server set the session cookie; head straight to the main UI.
      goto('/');
    } catch (e: any) {
      error = e?.message ?? 'setup failed';
      if (/setup code/i.test(error)) codeRequired = true;
    } finally {
      busy = false;
    }
  }
</script>

<div class="page-void h-full flex items-center justify-center p-6">
  <form
    on:submit|preventDefault={submit}
    class="w-full max-w-md panel p-8 space-y-5"
  >
    <header class="space-y-1">
      <h1 class="text-cursed-400 font-mono text-lg tracking-widest">
        AEON MAGICK — FIRST-BOOT SETUP
      </h1>
      <p class="text-zinc-400 text-sm">
        Set the password you'll use to sign into this device. The same
        password works for the web UI, the REST API (HTTP Basic), and
        MCP clients. You can also issue scoped API tokens after sign-in.
      </p>
    </header>

    <label class="block">
      <span class="text-xs uppercase tracking-wider text-zinc-500">username</span>
      <input
        type="text"
        bind:value={username}
        autocomplete="username"
        class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-steel-700
               focus:outline-none focus:ring-1 focus:ring-cursed-500/50"
      />
    </label>

    <label class="block">
      <span class="text-xs uppercase tracking-wider text-zinc-500">password</span>
      <input
        type="password"
        bind:value={password}
        autocomplete="new-password"
        class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-steel-700
               focus:outline-none focus:ring-1 focus:ring-cursed-500/50"
      />
      {#if tooShort}
        <span class="text-xs text-amber-400 mt-1 block">at least 8 characters</span>
      {/if}
    </label>

    <label class="block">
      <span class="text-xs uppercase tracking-wider text-zinc-500">confirm</span>
      <input
        type="password"
        bind:value={confirm}
        autocomplete="new-password"
        class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-steel-700
               focus:outline-none focus:ring-1 focus:ring-cursed-500/50"
      />
      {#if mismatch}
        <span class="text-xs text-red-400 mt-1 block">passwords don't match</span>
      {/if}
    </label>

    {#if codeRequired}
      <label class="block">
        <span class="text-xs uppercase tracking-wider text-zinc-500">setup code</span>
        <input
          type="text"
          bind:value={code}
          autocomplete="off"
          autocapitalize="characters"
          spellcheck="false"
          placeholder="XXXX-XXXX"
          class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-steel-700 font-mono tracking-widest
                 focus:outline-none focus:ring-1 focus:ring-cursed-500/50"
        />
        <span class="text-xs text-zinc-500 mt-1 block">
          Proves you have this device in hand. It is in <code class="font-mono">aeon-credentials.txt</code>
          on the SD card's boot partition — asked for because the device has been on for more than
          30 minutes, or was restarted, before setup was finished.
        </span>
      </label>
    {/if}

    {#if error}
      <p class="text-red-400 text-sm">{error}</p>
    {/if}

    <button
      type="submit"
      disabled={!canSubmit}
      class="btn-primary w-full disabled:opacity-50 disabled:cursor-not-allowed"
    >
      {busy ? 'saving…' : 'set password & sign in'}
    </button>

    <p class="text-xs text-zinc-500 pt-2">
      WiFi setup? <a href="/setup/wifi" class="text-cursed-400 underline">Configure WiFi</a>
    </p>
  </form>
</div>
