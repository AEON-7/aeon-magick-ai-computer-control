<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';

  // Hailo AI HAT+ dashboard. Three states, driven by getHailoStatus():
  //   (a) device_present=false → calm "no HAT" empty state
  //   (b) installed=false      → HAT identity + Install button + progress
  //   (c) installed=true       → live NPU stats + model library grid
  // 5s poll like the OrbNet dashboard. While an install or a deploy is in
  // flight we poll those sub-statuses faster (1s) for a responsive bar.

  let status: api.HailoStatus | null = null;
  let loadErr = '';            // shown if the very first fetch fails (perms etc.)
  let poll: ReturnType<typeof setInterval>;

  // ── install flow ──
  let installing = false;                       // local "button pressed" latch
  let installStatus: api.DeployStatus | null = null;
  let installPoll: ReturnType<typeof setInterval> | null = null;
  let installErr = '';

  // ── per-model deploy flow ──
  // We track the in-flight deploy id + its progress, and poll the SHARED
  // install/status endpoint is NOT reused here — deploy progress rides on
  // getHailoStatus() refreshes (state flips available→downloading→loaded)
  // plus a per-id DeployStatus we surface from getHailoModels merges. To
  // keep it simple + honest against the contract (which has no per-deploy
  // status GET), we show a busy spinner on the card until the next status
  // poll reports the model as deployed/loaded.
  let deployingId = '';
  let deployErr = '';
  let unloadingId = '';

  // Separate model list — getHailoModels() is the curated library; the
  // status payload's `loaded` is the live subset. We render the library
  // and overlay live state from `status.loaded` by id.
  let models: api.HailoModel[] = [];

  async function refresh() {
    try {
      status = await api.getHailoStatus();
      loadErr = '';
      // Once installed, keep the library in sync.
      if (status.installed) {
        try {
          const m = await api.getHailoModels();
          models = m.models;
        } catch (e) {
          console.warn('hailo models', e);
        }
      }
      // If an install finished (installed flipped true), tear down its poll.
      if (status.installed && installPoll) stopInstallPoll();
    } catch (e: any) {
      // 403 here means the token isn't admin-scoped. Surface it calmly.
      loadErr = e?.status === 403
        ? 'Admin access required to manage the Hailo HAT.'
        : (e?.message ?? 'Failed to reach the Hailo subsystem.');
    }
  }

  onMount(() => {
    refresh();
    poll = setInterval(refresh, 5000);
  });
  onDestroy(() => {
    clearInterval(poll);
    stopInstallPoll();
  });

  // ── install ──
  async function startInstall() {
    if (installing) return;
    installErr = '';
    installing = true;
    installStatus = { ok: true, phase: 'starting', percent: 0, done: false, log: '' };
    try {
      const r = await api.installHailo();
      if (!r.ok || !r.started) {
        installErr = r.err ?? 'Install did not start.';
        installing = false;
        installStatus = null;
        return;
      }
      // Poll the install bar at 1s for responsiveness.
      stopInstallPoll();
      installPoll = setInterval(pollInstall, 1000);
      pollInstall();
    } catch (e: any) {
      installErr = e?.message ?? 'Install request failed.';
      installing = false;
      installStatus = null;
    }
  }

  async function pollInstall() {
    try {
      installStatus = await api.getHailoInstallStatus();
      if (installStatus.done) {
        installing = false;
        stopInstallPoll();
        // Pull a fresh top-level status so the "reboot required" / installed
        // transition shows immediately rather than on the next 5s tick.
        refresh();
      }
    } catch (e) {
      console.warn('hailo install status', e);
    }
  }

  function stopInstallPoll() {
    if (installPoll) {
      clearInterval(installPoll);
      installPoll = null;
    }
  }

  // ── deploy / unload ──
  async function deploy(id: string) {
    if (deployingId) return;
    deployErr = '';
    deployingId = id;
    try {
      const r = await api.deployHailoModel(id);
      if (!r.ok || !r.started) {
        deployErr = r.err ?? `Could not start deploying ${id}.`;
        deployingId = '';
        return;
      }
      // Optimistically reflect "downloading" until a status poll confirms.
      models = models.map((m) =>
        m.id === id ? { ...m, state: 'downloading' } : m,
      );
      // Refresh shortly so the live state catches up; the 5s poll then
      // takes over and clears the busy latch once state === loaded/deployed.
      setTimeout(refresh, 1200);
    } catch (e: any) {
      deployErr = e?.message ?? `Deploy of ${id} failed.`;
      deployingId = '';
    }
  }

  async function unload(id: string) {
    if (unloadingId) return;
    deployErr = '';
    unloadingId = id;
    try {
      const r = await api.unloadHailoModel(id);
      if (!r.ok) {
        deployErr = r.err ?? `Could not unload ${id}.`;
      }
      await refresh();
    } catch (e: any) {
      deployErr = e?.message ?? `Unload of ${id} failed.`;
    } finally {
      unloadingId = '';
    }
  }

  // ── derived helpers ──
  // Merge the live `loaded` subset from status onto the library list by id,
  // so a card knows whether it's currently loaded + who's consuming it.
  $: loadedById = new Map((status?.loaded ?? []).map((m) => [m.id, m]));
  $: mergedModels = models.map((m) => {
    const live = loadedById.get(m.id);
    return live ? { ...m, ...live } : m;
  });
  // Once a deploy lands (state advanced past downloading), drop the latch.
  $: if (deployingId) {
    const m = mergedModels.find((x) => x.id === deployingId);
    if (m && (m.state === 'loaded' || m.state === 'deployed')) deployingId = '';
  }

  $: memPct = status && status.mem_total_mb > 0
    ? Math.min(100, Math.round((status.mem_used_mb / status.mem_total_mb) * 100))
    : 0;

  const KIND_LABEL: Record<string, string> = {
    llm: 'LLM',
    vlm: 'Vision-LLM',
    stt: 'Speech-to-text',
    tts: 'Text-to-speech',
    vision: 'Vision',
    ocr: 'OCR',
  };
  const kindLabel = (k: string) => KIND_LABEL[k] ?? k;

  // Kind filter for the model library (TTS lives here too — CPU Kokoro until
  // Hailo ships a TTS HEF).
  type KindFilter = 'all' | 'llm' | 'vlm' | 'stt' | 'tts' | 'vision' | 'ocr';
  let kindFilter: KindFilter = 'all';
  const KIND_FILTERS: { id: KindFilter; label: string }[] = [
    { id: 'all', label: 'All' },
    { id: 'llm', label: 'LLM' },
    { id: 'vlm', label: 'VLM' },
    { id: 'stt', label: 'STT' },
    { id: 'tts', label: 'TTS' },
    { id: 'vision', label: 'Vision' },
    { id: 'ocr', label: 'OCR' },
  ];
  $: filteredModels = kindFilter === 'all'
    ? mergedModels
    : mergedModels.filter((m) => m.kind === kindFilter);
  $: kindCounts = KIND_FILTERS.reduce((acc, f) => {
    acc[f.id] = f.id === 'all'
      ? mergedModels.length
      : mergedModels.filter((m) => m.kind === f.id).length;
    return acc;
  }, {} as Record<string, number>);

  // Per-card consumer line: look up consumers attached to the loaded model.
  // The contract puts consumers at the top level; we also accept a model
  // carrying its own (future-proof) — top level wins for the UI.
  $: consumers = status?.consumers ?? [];
</script>

<div class="page-void min-h-screen">
  <header class="flex items-center justify-between px-5 py-3 chrome-header">
    <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
    <h1 class="font-mono text-lg text-orange-300 inline-flex items-center gap-2">
      <Icon name="spark" class="w-5 h-5 text-orange-300" /> Hailo AI
    </h1>
    <div class="w-32"></div>
  </header>

  <main class="max-w-3xl mx-auto px-5 py-6 space-y-5">
    {#if loadErr}
      <div class="rounded-sm border border-red-500/40 bg-red-500/10 p-4 text-sm text-red-300">
        {loadErr}
      </div>
    {/if}

    {#if !status}
      {#if !loadErr}
        <p class="text-ink-400 text-sm">Loading Hailo status…</p>
      {/if}

    {:else if !status.device_present}
      <!-- ── State (a): no HAT over PCIe ──────────────────────────────── -->
      <div class="panel p-8 text-center space-y-3">
        <div class="flex justify-center text-ink-600">
          <Icon name="spark" class="w-10 h-10" />
        </div>
        <div class="font-mono text-ink-300">No Hailo HAT detected over PCIe.</div>
        <p class="text-xs text-ink-500 leading-relaxed max-w-md mx-auto">
          Seat a Hailo-10H AI HAT+ on the Pi 5 PCIe header and reboot. Once the
          kernel enumerates <code class="text-ink-400">/dev/hailo0</code>, this page
          will offer to install the runtime and a model library.
        </p>
      </div>

    {:else if !status.installed}
      <!-- ── State (b): HAT present, packages not installed ───────────── -->
      <div class="panel p-5 space-y-4">
        <div class="flex items-center justify-between gap-3">
          <div class="flex items-center gap-3 min-w-0">
            <div class="text-orange-300 shrink-0">
              <Icon name="spark" class="w-8 h-8" />
            </div>
            <div class="min-w-0">
              <div class="font-mono text-orange-200">
                {status.hat === 'hailo-10h' ? 'Hailo-10H' : status.hat === 'hailo-8' ? 'Hailo-8' : 'Hailo HAT'}
                <span class="text-ink-400"> · 8 GB</span>
              </div>
              <div class="text-xs text-ink-400">Detected over PCIe — runtime not installed yet.</div>
            </div>
          </div>
          <span class="pill bg-amber-500/15 text-amber-300 border border-amber-500/40 shrink-0">not installed</span>
        </div>

        {#if installStatus}
          <!-- Install progress -->
          <div class="space-y-2">
            <div class="flex items-center justify-between text-xs font-mono">
              <span class="text-orange-300">{installStatus.phase ?? 'installing'}…</span>
              <span class="text-ink-400">{installStatus.percent ?? 0}%</span>
            </div>
            <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
              <div class="h-full bg-orange-500 transition-all duration-500"
                   style={`width:${installStatus.percent ?? 0}%`}></div>
            </div>
            {#if installStatus.log}
              <pre class="text-[10px] leading-snug font-mono text-ink-400 bg-ink-950 border border-ink-800 rounded p-2 max-h-40 overflow-y-auto whitespace-pre-wrap">{installStatus.log}</pre>
            {/if}
            {#if installStatus.done}
              <div class="rounded border border-amber-500/40 bg-amber-500/10 p-2.5 text-xs text-amber-200">
                {installStatus.phase === 'failed'
                  ? 'Install failed — check the log above.'
                  : '✓ Packages installed. A reboot is required before the NPU comes online.'}
              </div>
            {:else}
              <p class="text-[11px] text-ink-500">Running apt + model-zoo install. A reboot will be required when this finishes.</p>
            {/if}
          </div>
        {:else}
          {#if installErr}
            <div class="rounded border border-red-500/40 bg-red-500/10 p-2.5 text-xs text-red-300">{installErr}</div>
          {/if}
          <button class="btn-primary text-sm bg-orange-600 hover:bg-orange-500 active:bg-orange-700"
                  on:click={startInstall} disabled={installing}>
            {installing ? 'Starting…' : 'Install packages'}
          </button>
          <p class="text-[11px] text-ink-500 leading-relaxed">
            Installs <code class="text-ink-400">hailo-h10-all</code> plus the GenAI / Ollama
            model-zoo. Non-blocking — you can leave this page; progress resumes on return.
            A reboot is required afterward.
          </p>
        {/if}
      </div>

    {:else}
      <!-- ── State (c): installed — stats + model library ─────────────── -->

      <!-- STATS panel -->
      <div class="panel p-5 space-y-4">
        <div class="flex items-center justify-between">
          <div class="font-mono text-orange-200 inline-flex items-center gap-2">
            <Icon name="spark" class="w-4 h-4 text-orange-300" />
            {status.hat === 'hailo-10h' ? 'Hailo-10H' : status.hat === 'hailo-8' ? 'Hailo-8' : 'Hailo HAT'}
            <span class="text-ink-500 text-xs">· 8 GB</span>
          </div>
          {#if status.stats}
            <span class="pill-live">
              <span class="h-1.5 w-1.5 rounded-full bg-live-400"></span> online
            </span>
          {:else}
            <span class="pill bg-ink-800 text-ink-400 border border-steel-700">idle</span>
          {/if}
        </div>

        {#if status.stats}
          <div class="grid grid-cols-3 gap-3">
            <div class="rounded-sm bg-ink-950 border border-ink-800 p-3">
              <div class="text-[10px] uppercase tracking-wider text-ink-500">NPU util</div>
              <div class="font-mono text-xl text-orange-200">{status.stats.nnc_util_pct}<span class="text-sm text-ink-500">%</span></div>
            </div>
            <div class="rounded-sm bg-ink-950 border border-ink-800 p-3">
              <div class="text-[10px] uppercase tracking-wider text-ink-500">Temp</div>
              <div class="font-mono text-xl text-orange-200">{status.stats.temp_c}<span class="text-sm text-ink-500">°C</span></div>
            </div>
            <div class="rounded-sm bg-ink-950 border border-ink-800 p-3">
              <div class="text-[10px] uppercase tracking-wider text-ink-500">Power</div>
              <div class="font-mono text-xl text-orange-200">{status.stats.power_w}<span class="text-sm text-ink-500">W</span></div>
            </div>
          </div>
          {#if status.stats.cpu_util_pct !== undefined}
            <div class="text-[11px] font-mono text-ink-500">host CPU {status.stats.cpu_util_pct}%</div>
          {/if}
        {:else}
          <p class="text-xs text-ink-500">No live telemetry — the NPU is idle or <code class="text-ink-400">hailortcli</code> is unavailable.</p>
        {/if}

        <!-- Memory ledger bar -->
        <div class="space-y-1.5">
          <div class="flex items-center justify-between text-xs font-mono">
            <span class="text-ink-400">memory (ledger)</span>
            <span class="text-ink-500">
              {status.mem_used_mb} / {status.mem_total_mb} MB used · {status.mem_free_mb} MB free
            </span>
          </div>
          <div class="h-2.5 rounded-full bg-ink-800 overflow-hidden">
            <div class="h-full transition-all duration-500 {memPct > 85 ? 'bg-red-500' : memPct > 60 ? 'bg-amber-500' : 'bg-orange-500'}"
                 style={`width:${memPct}%`}></div>
          </div>
        </div>

        {#if consumers.length}
          <div class="text-[11px] font-mono text-ink-500">
            in use by: <span class="text-cursed-300">{consumers.join(', ')}</span>
          </div>
        {/if}
      </div>

      {#if deployErr}
        <div class="rounded border border-red-500/40 bg-red-500/10 p-2.5 text-xs text-red-300">{deployErr}</div>
      {/if}

      <!-- MODEL LIBRARY -->
      <div class="space-y-3">
        <div class="flex flex-wrap items-end justify-between gap-2">
          <h2 class="font-mono text-sm text-ink-300 tracking-wide">Model library</h2>
          <p class="text-[10px] text-ink-600 font-mono max-w-xs text-right leading-snug">
            TTS runs on the Pi CPU (Kokoro). Hailo-10H GenAI ships STT/LLM/VLM HEFs — not TTS yet.
          </p>
        </div>

        <!-- Kind filters (TTS / STT / …) -->
        <div class="flex flex-wrap gap-1.5">
          {#each KIND_FILTERS as f}
            {@const n = kindCounts[f.id] ?? 0}
            {#if f.id === 'all' || n > 0}
              <button type="button"
                      class="rounded-full border px-2.5 py-1 text-[11px] font-mono transition
                             {kindFilter === f.id
                               ? 'border-orange-500/50 bg-orange-500/15 text-orange-200'
                               : 'border-steel-700 text-ink-400 hover:border-ink-600'}"
                      on:click={() => (kindFilter = f.id)}>
                {f.label}
                <span class="text-ink-600 ml-1">{n}</span>
              </button>
            {/if}
          {/each}
        </div>

        {#if filteredModels.length === 0}
          <p class="text-xs text-ink-500">
            {mergedModels.length === 0
              ? 'No models in the library yet.'
              : `No ${kindFilter.toUpperCase()} models in the library.`}
          </p>
        {:else}
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
            {#each filteredModels as m (m.id)}
              {@const isLoaded = m.state === 'loaded'}
              {@const isDeployed = m.state === 'deployed'}
              {@const isBusy = m.state === 'downloading' || deployingId === m.id || unloadingId === m.id}
              {@const blocked = !m.fits && m.state === 'available'}
              {@const isCpu = m.runtime === 'cpu' || m.kind === 'tts'}
              <div class="rounded-sm border bg-ink-900 p-3.5 flex flex-col gap-2 transition
                          {isLoaded ? 'border-live-500/40' : 'border-steel-700'}
                          {blocked ? 'opacity-50' : ''}">
                <div class="flex items-start justify-between gap-2">
                  <div class="min-w-0">
                    <div class="font-mono text-sm text-orange-200 truncate">{m.name}</div>
                    <div class="text-[11px] text-ink-500 mt-0.5">
                      {kindLabel(m.kind)}
                      {#if isCpu}<span class="text-violet-400/90"> · CPU</span>{:else}<span class="text-orange-400/80"> · NPU</span>{/if}
                      {#if m.params} · {m.params}{/if}{#if m.quant} · {m.quant}{/if}
                    </div>
                  </div>
                  <span class="pill text-[10px] shrink-0
                    {isLoaded ? 'bg-live-500/20 text-live-400 border border-live-500/40'
                    : isDeployed ? 'bg-emerald-500/15 text-emerald-300 border border-emerald-500/40'
                    : m.state === 'downloading' ? 'bg-orange-500/15 text-orange-300 border border-orange-500/40'
                    : 'bg-ink-800 text-ink-400 border border-steel-700'}">
                    {m.state}
                  </span>
                </div>

                {#if m.use_for}
                  <p class="text-[11px] text-ink-400 leading-relaxed">{m.use_for}</p>
                {/if}

                <div class="flex items-center justify-between gap-2 mt-1">
                  <div class="text-[10px] font-mono text-ink-500 flex items-center gap-2 flex-wrap">
                    <span>{m.size_mb} MB</span>
                    {#if m.license}<span class="text-ink-600">·</span><span>{m.license}</span>{/if}
                    {#if blocked}
                      <span class="text-amber-400">· won't fit</span>
                    {/if}
                  </div>

                  {#if isLoaded}
                    <button class="btn text-xs py-1 px-2.5 border-red-500/40 text-red-300 hover:bg-red-500/10"
                            on:click={() => unload(m.id)} disabled={!!unloadingId}>
                      {unloadingId === m.id ? 'Unloading…' : 'Unload'}
                    </button>
                  {:else if isBusy}
                    <button class="btn text-xs py-1 px-2.5" disabled>
                      <span class="inline-block h-3 w-3 rounded-full border-2 border-orange-400 border-t-transparent animate-spin mr-1.5"></span>
                      {m.state === 'downloading' ? 'Downloading…' : 'Deploying…'}
                    </button>
                  {:else}
                    <button class="btn text-xs py-1 px-2.5 border-orange-500/40 text-orange-200 hover:bg-orange-500/10
                                   disabled:opacity-40 disabled:cursor-not-allowed"
                            on:click={() => deploy(m.id)}
                            disabled={blocked || !!deployingId}
                            title={blocked
                              ? 'Not enough free NPU memory — unload another model first'
                              : isCpu
                                ? 'Install on-device (CPU voice stack / sherpa-onnx)'
                                : 'Download + load onto the NPU'}>
                      {isCpu ? 'Install' : 'Deploy'}
                    </button>
                  {/if}
                </div>

                {#if isLoaded && consumers.length}
                  <div class="text-[10px] font-mono text-ink-600 border-t border-ink-800 pt-1.5">
                    consumers: <span class="text-cursed-400">{consumers.join(', ')}</span>
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  </main>
</div>
