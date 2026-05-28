<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let files: api.FileEntry[] = [];
  let cfg: api.FileXferConfig | null = null;
  let clip: api.ClipboardState | null = null;
  let clipText = '';
  let loading = true;
  let error = '';
  let msg = '';
  let uploading = false;
  let uploadProgress = 0;
  let typing = false;
  let poll_iv: ReturnType<typeof setInterval>;

  async function refresh() {
    try {
      const [f, c, k] = await Promise.all([
        api.listFiles(), api.getFileXferConfig(), api.getClipboard(),
      ]);
      files = f.files;
      cfg = c;
      clip = k;
      if (!document.activeElement || (document.activeElement as HTMLElement).id !== 'clipboard-input') {
        // Only refresh the textarea content if it's not currently focused
        // (otherwise we stomp the user's in-progress edits).
        clipText = k.text;
      }
      loading = false;
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
      loading = false;
    }
  }

  onMount(() => {
    refresh();
    poll_iv = setInterval(refresh, 5000);
  });
  onDestroy(() => { if (poll_iv) clearInterval(poll_iv); });

  async function onFileChange(ev: Event) {
    const input = ev.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    uploading = true;
    uploadProgress = 0;
    error = '';
    try {
      await api.uploadFile(file, (loaded, total) => {
        uploadProgress = Math.round((loaded / total) * 100);
      });
      msg = `✓ uploaded ${file.name}`;
      setTimeout(() => (msg = ''), 4000);
      input.value = '';
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'upload failed';
    } finally {
      uploading = false;
      uploadProgress = 0;
    }
  }

  async function onDelete(name: string) {
    if (!confirm(`Delete ${name}?`)) return;
    try { await api.deleteFile(name); await refresh(); }
    catch (e: any) { error = e?.message ?? 'delete failed'; }
  }

  async function saveClipboard() {
    try {
      const r = await api.setClipboard(clipText);
      msg = r.trimmed ? `saved (trimmed to ${r.size_bytes} B)` : `saved (${r.size_bytes} B)`;
      setTimeout(() => (msg = ''), 3000);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'save failed';
    }
  }
  async function clearClip() {
    if (!confirm('Clear the shared clipboard?')) return;
    try { await api.clearClipboard(); clipText = ''; await refresh(); }
    catch (e: any) { error = e?.message ?? 'clear failed'; }
  }
  async function typeOnTarget() {
    typing = true;
    error = '';
    try {
      // Save current text first so typing reflects what's in the textarea
      // (not just what's persisted).
      await api.setClipboard(clipText);
      const r = await api.typeClipboardOnTarget();
      msg = `✓ typed ${r.bytes_typed} chars on target`;
      setTimeout(() => (msg = ''), 4000);
    } catch (e: any) {
      error = e?.message ?? 'type failed';
    } finally {
      typing = false;
    }
  }

  async function toggleTargetServer(enabled: boolean) {
    try {
      await api.setFileXferConfig({ enabled });
      await refresh();
      if (enabled) {
        msg = '✓ target server will be available after next supervisor restart';
      } else {
        msg = '✓ target server disabled (effective after next supervisor restart)';
      }
      setTimeout(() => (msg = ''), 5000);
    } catch (e: any) {
      error = e?.message ?? 'toggle failed';
    }
  }
  async function toggleAllowUpload(allow: boolean) {
    try {
      await api.setFileXferConfig({ allow_upload: allow });
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'toggle failed';
    }
  }

  function fmtBytes(b: number): string {
    if (b < 1024) return `${b} B`;
    if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} kB`;
    if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(1)} MB`;
    return `${(b / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }
  function fmtAgo(ms: number): string {
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
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">files + clipboard</span>
    </div>
  </header>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-3xl mx-auto w-full space-y-6">

      {#if loading}<p class="text-zinc-500 text-sm">loading…</p>{/if}
      {#if error}<p class="text-red-400 text-sm">{error}</p>{/if}
      {#if msg}<p class="text-live-400 text-sm">{msg}</p>{/if}

      <!-- ─── Shared clipboard ─── -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
        <header class="space-y-1">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Shared clipboard
          </h2>
          <p class="text-xs text-zinc-500">
            A persistent text buffer. Save snippets, then click
            <em>type on target</em> to send the contents to the
            target's keyboard via HID. Useful for passing long
            passwords, paths, or AI agent prompts without retyping.
            Cap: {clip?.max_bytes ?? 65536} bytes.
          </p>
        </header>

        <textarea
          id="clipboard-input"
          bind:value={clipText}
          rows="6"
          placeholder="Paste or type a snippet here, then click 'type on target'."
          class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-2
                 text-sm text-zinc-200 font-mono"
        ></textarea>

        <div class="flex flex-wrap items-center gap-2">
          <button class="btn-primary text-xs" on:click={saveClipboard}>
            save
          </button>
          <button class="btn text-xs" on:click={typeOnTarget} disabled={typing || !clipText}>
            {typing ? 'typing…' : '↳ type on target'}
          </button>
          <button class="btn text-xs ml-auto hover:bg-red-500/20 hover:text-red-300 hover:border-red-500/40"
                  on:click={clearClip}>
            clear
          </button>
        </div>
        <p class="text-[10px] text-zinc-600">
          The persisted buffer is what "type on target" sends. Saving
          first ensures the textarea contents match what gets typed.
          The typing speed is whatever you configured in the HID
          persona; default is roughly natural typing rate.
        </p>
      </section>

      <!-- ─── HTTP file transfer ─── -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
        <header class="space-y-1">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            File transfer
          </h2>
          <p class="text-xs text-zinc-500">
            A two-way drop folder between the Pi and the USB-connected
            host. The Pi-side admin UI (here) always works; the host
            side is an opt-in plain HTTP server on usb0.
          </p>
        </header>

        <!-- Toggle: target-facing server -->
        {#if cfg}
          <div class="space-y-2 pt-2 border-t border-ink-800">
            <label class="flex items-center gap-3 cursor-pointer">
              <input type="checkbox" checked={cfg.enabled}
                     on:change={(e) => toggleTargetServer(e.currentTarget.checked)}
                     class="w-4 h-4 accent-cursed-500" />
              <span class="text-zinc-200 text-sm">
                Expose files to the USB-connected host on port {cfg.port}
              </span>
            </label>
            <p class="text-[11px] text-zinc-500 pl-7 leading-relaxed">
              When on, the host can browse <code class="text-cursed-300">http://10.55.0.1:{cfg.port}/</code>
              to download files. No password required (assumes the host on
              usb0 is already trusted — same trust model as the USB-CDROM).
              <strong class="text-amber-400">Requires a supervisor restart to take effect.</strong>
            </p>
            {#if cfg.enabled}
              <label class="flex items-center gap-3 cursor-pointer pl-7">
                <input type="checkbox" checked={cfg.allow_upload}
                       on:change={(e) => toggleAllowUpload(e.currentTarget.checked)}
                       class="w-4 h-4 accent-cursed-500" />
                <span class="text-zinc-300 text-xs">
                  Allow the host to upload files back to the Pi
                </span>
              </label>
              <p class="text-[11px] text-zinc-500 pl-14 leading-relaxed">
                Off by default. Turn on if you want to grab logs / screenshots
                / config files OFF the target.
              </p>
            {/if}
          </div>
        {/if}

        <!-- Upload form -->
        <div class="pt-2 border-t border-ink-800 space-y-2">
          <p class="text-xs uppercase tracking-wider text-zinc-500">
            Upload from this device
          </p>
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="file"
                   on:change={onFileChange}
                   disabled={uploading}
                   class="text-xs text-zinc-300
                          file:btn-primary file:text-xs file:mr-3 file:border-0" />
            {#if uploading}
              <span class="text-xs font-mono text-cursed-300">
                {uploadProgress}%
              </span>
            {/if}
          </label>
        </div>

        <!-- File list -->
        {#if files.length === 0}
          <p class="text-xs text-zinc-500 italic">
            No files yet. Drop one above or have the target upload via the
            public HTTP endpoint.
          </p>
        {:else}
          <div class="space-y-1 pt-2 border-t border-ink-800">
            {#each files as f (f.name)}
              <div class="flex items-center gap-3 p-2 rounded bg-ink-950/40
                          border border-ink-800 text-xs font-mono
                          hover:border-ink-700">
                <span class="text-zinc-200 flex-1 truncate" title={f.name}>{f.name}</span>
                <span class="text-zinc-500 w-20 text-right">{fmtBytes(f.size_bytes)}</span>
                <span class="text-zinc-600 w-24 text-right">{fmtAgo(f.modified_ms)}</span>
                <a href={`/api/files/${encodeURIComponent(f.name)}`}
                   class="text-[10px] px-2 py-1 rounded border
                          border-cursed-500/40 text-cursed-300
                          hover:bg-cursed-500/10 transition-colors">
                  download
                </a>
                <button class="text-[10px] text-zinc-500 hover:text-red-400"
                        on:click={() => onDelete(f.name)}>✕</button>
              </div>
            {/each}
          </div>
        {/if}

        <p class="text-[10px] text-zinc-500 pt-1 border-t border-ink-800">
          Files persist at <code class="text-cursed-300">/var/lib/aeon/files/</code>.
          Multi-GB uploads supported via streaming (same path as ISOs).
        </p>
      </section>
    </div>
  </main>
</div>
