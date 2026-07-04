<script lang="ts">
  // USB-CDROM disk-drive library.
  //
  // Lets the user upload bootable ISOs (Debian installer, Ubuntu, macOS
  // recovery, Windows install media, etc.) and "insert" any of them
  // into the gadget composite. The connected host (Mac, PC) then sees a
  // bootable CDROM appear on the USB-C dock and can pick it from the
  // Option-key / F12 boot menu to install or live-boot.

  import PageHeader from '$lib/components/PageHeader.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { confirmRite } from '$lib/confirm';
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let state: api.StorageState | null = null;
  let loading = true;
  let error = '';
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  // Upload state
  let fileInput: HTMLInputElement;
  let uploadingFile: File | null = null;
  let uploadProgress = 0;       // 0–100
  let uploadBytesLoaded = 0;
  let uploadBytesTotal = 0;
  let uploadError = '';

  async function refresh() {
    try {
      state = await api.getStorage();
      error = '';
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
    } finally {
      loading = false;
    }
  }

  async function eject() {
    if (!state || !state.active) return;
    if (!(await confirmRite({
      title: 'Eject disk',
      body: `Eject "${state.active}"?\n\nThe USB-C host will see the disk drive disappear (~1s blip).`,
      confirmLabel: 'eject',
    }))) return;
    try {
      await api.setActiveIso('');
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'eject failed';
    }
  }

  async function activate(slug: string) {
    if (state?.active === slug) return;
    if (!(await confirmRite({
      title: 'Insert disk',
      body:
        `Make "${slug}" the active disk drive?\n\n` +
        `The USB-C host will see a brief disconnect (~1s) while the gadget ` +
        `rebuilds, then a new bootable CDROM appears in the boot menu.`,
      confirmLabel: 'insert',
    }))) return;
    try {
      await api.setActiveIso(slug);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'activate failed';
    }
  }

  async function deleteIso(slug: string) {
    if (!(await confirmRite({
      title: 'Delete ISO',
      body: `Delete ISO "${slug}"? This frees disk space; not undoable.`,
      danger: true,
      confirmLabel: 'delete',
    }))) return;
    try {
      await api.deleteIso(slug);
      await refresh();
    } catch (e: any) {
      error = e?.message ?? 'delete failed';
    }
  }

  function pickFile() {
    fileInput?.click();
  }

  async function onFileChange(e: Event) {
    const f = (e.target as HTMLInputElement).files?.[0];
    if (!f) return;
    uploadingFile = f;
    uploadProgress = 0;
    uploadBytesLoaded = 0;
    uploadBytesTotal = f.size;
    uploadError = '';
    try {
      await api.uploadIso(f, (loaded, total) => {
        uploadBytesLoaded = loaded;
        uploadBytesTotal = total;
        uploadProgress = total > 0 ? Math.floor((loaded / total) * 100) : 0;
      });
      uploadingFile = null;
      uploadProgress = 0;
      await refresh();
    } catch (e: any) {
      uploadError = e?.message ?? 'upload failed';
    }
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
    return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }

  function fmtTime(ms: number): string {
    if (ms <= 0) return '—';
    return new Date(ms).toLocaleString();
  }

  onMount(() => {
    refresh();
    // Refresh every 5s so the "active" indicator updates if changed
    // from another browser session
    pollTimer = setInterval(refresh, 5000);
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
  });
</script>

<div class="h-full flex flex-col">
  <PageHeader title="disk drive" />

  <main class="flex-1 overflow-auto p-6">
    <div class="max-w-3xl mx-auto space-y-6">

      <!-- Intro -->
      <section class="space-y-2">
        <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
          USB-C boot media
        </h2>
        <p class="text-zinc-400 text-sm leading-relaxed">
          Upload bootable ISOs (Debian, Ubuntu, macOS recovery, Windows
          installer, anything else) and "insert" one as a virtual CDROM
          on the USB-C connection. The host machine sees a bootable
          disk drive on its boot picker (Option key on Mac, F12 on PC)
          and can install or live-boot from it without you needing a
          physical USB stick.
        </p>
        <p class="text-xs text-zinc-500">
          Library lives at <code class="text-cursed-300">{state?.iso_dir ?? '/var/lib/aeon/iso'}</code>.
          {#if state}
            Free space: <strong>{fmtBytes(state.free_bytes)}</strong>.
          {/if}
        </p>
      </section>

      <!-- Currently active -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
        <header class="flex items-center justify-between">
          <h3 class="font-mono text-xs uppercase tracking-wider text-zinc-400">
            Currently inserted
          </h3>
          {#if state?.active}
            <button class="btn text-xs inline-flex items-center gap-1.5" on:click={eject}>
              <Icon name="release" class="w-3.5 h-3.5" />eject
            </button>
          {/if}
        </header>
        {#if state?.active}
          <div class="font-mono text-sm">
            <span class="text-live-400">●</span>
            <span class="text-zinc-200">{state.active}</span>
          </div>
          <p class="text-xs text-zinc-500">
            The host sees a bootable CDROM drive on the USB-C connection.
            Hold Option (Mac) or F12 (PC) at boot to pick it from the menu.
          </p>
        {:else}
          <p class="text-zinc-500 text-sm italic">
            No disk inserted. Pick one from the library below to insert it.
          </p>
        {/if}
      </section>

      <!-- Upload -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
        <header class="flex items-center justify-between">
          <h3 class="font-mono text-xs uppercase tracking-wider text-zinc-400">
            Upload an ISO
          </h3>
        </header>
        <input type="file" bind:this={fileInput} accept=".iso,.img,application/octet-stream"
               class="hidden" on:change={onFileChange} />
        {#if uploadingFile}
          <div class="space-y-2">
            <p class="text-sm text-zinc-200 font-mono truncate">
              {uploadingFile.name}
            </p>
            <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
              <div class="h-full bg-cursed-500 transition-all duration-150"
                   style="width: {uploadProgress}%"></div>
            </div>
            <p class="text-xs font-mono text-zinc-400">
              {fmtBytes(uploadBytesLoaded)} / {fmtBytes(uploadBytesTotal)}
              ({uploadProgress}%)
            </p>
            {#if uploadError}
              <p class="text-sm text-red-400">✗ {uploadError}</p>
            {/if}
          </div>
        {:else}
          <button class="btn-primary" on:click={pickFile}>
            Pick file…
          </button>
          <p class="text-xs text-zinc-500">
            Multi-GB ISOs are fine — the upload streams to disk so the
            web UI doesn't have to hold the whole file in memory. The
            server computes SHA-256 as it writes for integrity tracking.
          </p>
        {/if}
      </section>

      <!-- Library -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl">
        <header class="px-5 py-3 border-b border-ink-700">
          <h3 class="font-mono text-xs uppercase tracking-wider text-zinc-400">
            Library
            {#if state}
              <span class="text-zinc-500 normal-case ml-2">{state.isos.length} ISO{state.isos.length === 1 ? '' : 's'}</span>
            {/if}
          </h3>
        </header>
        {#if loading}
          <p class="px-5 py-8 text-center text-zinc-500 text-sm">loading…</p>
        {:else if error}
          <p class="px-5 py-8 text-center text-red-400 text-sm">{error}</p>
        {:else if state && state.isos.length === 0}
          <p class="px-5 py-8 text-center text-zinc-500 text-sm italic">
            No ISOs uploaded yet. Use the upload section above.
          </p>
        {:else if state}
          <div class="divide-y divide-ink-800">
            {#each state.isos as iso (iso.slug)}
              <div class="px-5 py-3 flex items-center gap-4">
                <div class="flex-1 min-w-0 space-y-0.5">
                  <div class="flex items-center gap-2">
                    {#if state.active === iso.slug}
                      <span class="text-live-400 text-xs">●</span>
                    {:else}
                      <span class="text-zinc-600 text-xs">○</span>
                    {/if}
                    <span class="text-zinc-200 text-sm font-medium truncate">
                      {iso.display}
                    </span>
                  </div>
                  <div class="text-[10px] font-mono text-zinc-500 flex gap-3">
                    <span>{iso.slug}</span>
                    <span>{fmtBytes(iso.size_bytes)}</span>
                    <span>uploaded {fmtTime(iso.uploaded_at_ms)}</span>
                  </div>
                  {#if iso.sha256}
                    <div class="text-[9px] font-mono text-zinc-600 truncate"
                         title={iso.sha256}>
                      sha256: {iso.sha256.slice(0, 16)}…
                    </div>
                  {/if}
                </div>
                <div class="flex gap-2 flex-shrink-0">
                  {#if state.active !== iso.slug}
                    <button class="btn-primary text-xs" on:click={() => activate(iso.slug)}>
                      insert
                    </button>
                  {/if}
                  <button class="btn text-xs text-red-300 hover:border-red-500"
                          on:click={() => deleteIso(iso.slug)}
                          disabled={state.active === iso.slug}
                          title={state.active === iso.slug
                            ? 'eject first before deleting'
                            : 'delete this ISO'}>
                    delete
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <p class="text-xs text-zinc-500 leading-relaxed">
        Inserting or ejecting an ISO triggers a brief USB re-enumeration
        on the host (~1s blip). Active HID input (keyboard / trackpad)
        and USB ethernet stay attached across the swap — only the disk
        drive changes. Setting "insert" with the host actively booted
        from the previous ISO will detach mid-flight; either eject
        cleanly before swapping or accept that the host will see the
        old disk vanish.
      </p>
    </div>
  </main>
</div>
