<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '$lib/api';

  let keys: api.SshKey[] = [];
  let loading = true;
  let error = '';
  let newKey = '';
  let adding = false;
  let addMsg = '';

  async function refresh() {
    try {
      const r = await api.listSshKeys();
      keys = r.keys;
      loading = false;
    } catch (e: any) {
      error = e?.message ?? 'failed to load keys';
      loading = false;
    }
  }

  onMount(refresh);

  async function add() {
    if (!newKey.trim()) return;
    adding = true;
    error = '';
    addMsg = '';
    try {
      const r = await api.addSshKey(newKey.trim());
      addMsg = r.id ? '✓ key added — try SSH now' : '✓ key already present';
      setTimeout(() => (addMsg = ''), 5000);
      newKey = '';
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to add';
    } finally {
      adding = false;
    }
  }

  async function remove(id: string, fp: string) {
    if (!confirm(`Delete this key (${fp})?\n\nDevices using this key will lose passwordless SSH access.`)) {
      return;
    }
    try {
      await api.removeSshKey(id);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'failed to delete';
    }
  }

  function fmtAge(ms: number): string {
    const dt = Date.now() - ms;
    if (dt < 60_000) return 'just now';
    if (dt < 3_600_000) return `${Math.floor(dt / 60_000)} min ago`;
    if (dt < 86_400_000) return `${Math.floor(dt / 3_600_000)}h ago`;
    return `${Math.floor(dt / 86_400_000)}d ago`;
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← AEON MAGICK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">SSH key trust store</span>
    </div>
  </header>

  <main class="flex-1 overflow-auto p-6 max-w-3xl mx-auto w-full space-y-6">
    <section class="bg-ink-900 border border-ink-700 rounded-xl p-6 space-y-4">
      <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">Add a public key</h2>
      <p class="text-sm text-zinc-400">
        Paste a single OpenSSH public key from your laptop's
        <code class="text-cursed-300">~/.ssh/id_ed25519.pub</code> (or
        <code class="text-cursed-300">id_rsa.pub</code>, ECDSA, FIDO2 sk-… etc.).
        Once added, you can SSH as <code class="text-cursed-300">admin@aeon-magick.local</code>
        without typing a password.
      </p>
      <textarea bind:value={newKey} rows="3"
                placeholder="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI… you@laptop"
                class="w-full bg-ink-800 border border-ink-700 rounded
                       px-3 py-2 text-xs text-zinc-200 font-mono break-all"></textarea>
      <div class="flex items-center gap-3">
        <button class="btn-primary text-sm" on:click={add} disabled={adding || !newKey.trim()}>
          {adding ? 'adding…' : 'add key'}
        </button>
        {#if addMsg}
          <span class="text-xs text-live-400 font-mono">{addMsg}</span>
        {/if}
        {#if error}
          <span class="text-xs text-red-400 font-mono">{error}</span>
        {/if}
      </div>
      <p class="text-xs text-zinc-500 leading-relaxed">
        <strong class="text-amber-400">Don't paste a private key.</strong>
        Public keys start with <code class="text-cursed-300">ssh-ed25519</code>,
        <code class="text-cursed-300">ssh-rsa</code>, or <code class="text-cursed-300">ecdsa-sha2-…</code>
        followed by a long base64 blob. Private keys start with
        <code>-----BEGIN OPENSSH PRIVATE KEY-----</code> and stay on YOUR machine, never here.
      </p>
    </section>

    <section class="bg-ink-900 border border-ink-700 rounded-xl p-6 space-y-3">
      <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
        Authorized keys
        <span class="text-zinc-500 ml-2">({keys.length})</span>
      </h2>
      {#if loading}
        <p class="text-zinc-500 text-sm">loading…</p>
      {:else if keys.length === 0}
        <p class="text-zinc-500 text-sm italic">
          No keys yet. Until you add one, SSH falls back to password auth with
          the default <code class="text-cursed-300">admin / aeon-default-change-me</code>.
        </p>
      {:else}
        <div class="space-y-2">
          {#each keys as k (k.id)}
            <div class="p-3 rounded-lg bg-ink-950 border border-ink-800
                        flex items-start justify-between gap-3">
              <div class="space-y-1 flex-1 min-w-0">
                <div class="flex items-center gap-2 flex-wrap">
                  <span class="text-[10px] font-mono px-1.5 py-0.5 rounded
                               border bg-cursed-500/15 text-cursed-300 border-cursed-500/40">
                    {k.type}
                  </span>
                  {#if k.comment}
                    <span class="text-xs text-zinc-200">{k.comment}</span>
                  {/if}
                </div>
                <div class="text-[10px] font-mono text-zinc-500 break-all"
                     title="SHA256 fingerprint of the key">
                  {k.fingerprint}
                </div>
                <div class="text-[10px] text-zinc-600">
                  added {fmtAge(k.added_at_ms)}
                </div>
              </div>
              <button class="text-xs text-zinc-500 hover:text-red-400
                             border border-ink-700 hover:border-red-500/50
                             rounded px-2 py-1 transition-colors flex-shrink-0"
                      on:click={() => remove(k.id, k.fingerprint)}>
                remove
              </button>
            </div>
          {/each}
        </div>
      {/if}
    </section>

    <section class="text-xs text-zinc-500 space-y-2 leading-relaxed">
      <p>
        <strong class="text-zinc-300">How keys are stored:</strong>
        we append to <code class="text-cursed-300">/home/admin/.ssh/authorized_keys</code>
        with strict file perms (0600 user-only).
        The <code>admin</code> user is the only SSH-accessible account; root login is disabled.
      </p>
      <p>
        <strong class="text-zinc-300">Hardening tip:</strong>
        once you've added a key and confirmed SSH works,
        SSH in and disable password auth in <code class="text-cursed-300">/etc/ssh/sshd_config</code>
        (<code>PasswordAuthentication no</code>) — locks down the device against brute-force.
      </p>
    </section>
  </main>
</div>
