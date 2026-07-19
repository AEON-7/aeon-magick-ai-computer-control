<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';
  import Icon from '$lib/components/Icon.svelte';

  // BrainCraft HAT dashboard. One page, gated by getBraincraftStatus():
  //   • !enabled            → calm "disabled" card + Enable toggle
  //   • enabled, present    → mode switch (AI/Viewfinder/Voice) + device panel,
  //                           viewfinder controls, and a Voice panel that either
  //                           offers the button-driven install or the live
  //                           local/hosted backend selector + say box.
  //   • enabled, !present   → "HAT not detected / daemon idle" hint.
  // 5s poll; while the voice install runs we poll its bar at 1s. The daemon
  // re-reads braincraft.toml every loop, so a config PUT is live within a tick.

  let status: api.BraincraftStatus | null = null;
  let loadErr = '';
  let saveErr = '';
  let poll: ReturnType<typeof setInterval>;
  let busy = false;              // latch while a PUT is in flight

  // hosted-endpoint editor (local copies so typing doesn't fight the poll)
  let persona = '';
  let llmUrl = '';
  let ttsUrl = '';
  let asrUrl = '';
  let showAdvanced = false;
  let sayText = '';

  // ── voice install flow (mirrors the Hailo install) ──
  let installing = false;
  let installStatus: api.DeployStatus | null = null;
  let installPoll: ReturnType<typeof setInterval> | null = null;
  let installErr = '';

  $: cfg = status?.config ?? null;
  $: dev = status?.daemon ?? null;
  $: voice = status?.voice ?? null;
  $: present = !!dev?.present;
  $: enabled = !!status?.enabled;
  $: mode = cfg?.mode ?? 'ai';
  $: recording = !!dev?.captures?.recording;

  async function refresh() {
    try {
      const s = await api.getBraincraftStatus();
      status = s;
      loadErr = '';
      // Seed the editor fields once (don't clobber active typing).
      if (!editorSeeded && s.config) {
        persona = s.config.backend.persona ?? '';
        llmUrl = s.config.hosted.llm_url ?? '';
        ttsUrl = s.config.hosted.tts_url ?? '';
        asrUrl = s.config.hosted.asr_url ?? '';
        editorSeeded = true;
      }
      if (s.voice_installing && !installPoll) startInstallPoll();
      if (!s.voice_installing && installPoll && installStatus?.done) stopInstallPoll();
    } catch (e: any) {
      loadErr = e?.status === 403
        ? 'Admin access required to manage the BrainCraft HAT.'
        : (e?.message ?? 'Failed to reach the BrainCraft subsystem.');
    }
  }
  let editorSeeded = false;

  // Audio in/out volume (WM8960). null until first fetch / no codec fitted.
  let audioVol: api.AudioVolume | null = null;
  let outVol = 80;
  let inVol = 80;
  let micMuted = false;
  let audioBusy = false;

  async function refreshAudio() {
    try {
      const a = await api.getAudioVolume();
      audioVol = a;
      if (!audioBusy && a.present) {
        if (a.playback) outVol = a.playback.percent;
        if (a.capture) { inVol = a.capture.percent; micMuted = a.capture.muted; }
      }
    } catch {
      audioVol = null;
    }
  }
  async function saveAudio(p: { playback?: number; capture?: number; capture_muted?: boolean }) {
    audioBusy = true;
    try { audioVol = await api.setAudioVolume(p); } catch { /* best-effort */ }
    finally { audioBusy = false; }
  }

  onMount(() => {
    refresh();
    refreshAudio();
    poll = setInterval(() => { refresh(); refreshAudio(); }, 5000);
  });
  onDestroy(() => {
    clearInterval(poll);
    stopInstallPoll();
  });

  async function patch(p: api.BraincraftConfigPatch) {
    if (busy) return;
    busy = true;
    saveErr = '';
    try {
      const r = await api.setBraincraftConfig(p);
      if (!r.ok) saveErr = r.err ?? 'Save failed.';
      if (r.config) status = { ...(status as api.BraincraftStatus), config: r.config, enabled: r.config.enabled };
    } catch (e: any) {
      saveErr = e?.message ?? 'Save request failed.';
    } finally {
      busy = false;
      refresh();
    }
  }

  const setEnabled = (v: boolean) => patch({ enabled: v });
  const setMode = (m: api.BraincraftMode) => patch({ mode: m });
  const setOverlay = (v: boolean) => patch({ overlay: v });
  const setBackend = (m: api.BraincraftBackend) => patch({ backend_mode: m });
  const saveHosted = () =>
    patch({ persona, llm_url: llmUrl, tts_url: ttsUrl, asr_url: asrUrl });

  // ── transient device actions ──
  let actErr = '';
  async function doCapture() {
    actErr = '';
    try { const r = await api.braincraftCapture(); if (!r.ok) actErr = r.err ?? 'Capture failed.'; }
    catch (e: any) { actErr = e?.message ?? 'Capture failed.'; }
  }
  async function toggleRecord() {
    actErr = '';
    try {
      const r = await api.braincraftRecord(recording ? 'stop' : 'start');
      if (!r.ok) actErr = r.err ?? 'Record toggle failed.';
      setTimeout(refresh, 800);
    } catch (e: any) { actErr = e?.message ?? 'Record toggle failed.'; }
  }
  async function say() {
    const t = sayText.trim();
    if (!t) return;
    actErr = '';
    try { const r = await api.braincraftSay(t); if (!r.ok) actErr = r.err ?? 'Say failed.'; else sayText = ''; }
    catch (e: any) { actErr = e?.message ?? 'Say failed.'; }
  }

  // ── voice install ──
  async function startInstall() {
    if (installing) return;
    installErr = '';
    installing = true;
    installStatus = { ok: true, phase: 'starting', percent: 0, done: false, log: '' };
    try {
      const r = await api.installVoiceStack();
      if (!r.ok || !r.started) {
        installErr = r.err ?? 'Install did not start.';
        installing = false;
        installStatus = null;
        return;
      }
      startInstallPoll();
      pollInstall();
    } catch (e: any) {
      installErr = e?.message ?? 'Install request failed.';
      installing = false;
      installStatus = null;
    }
  }
  function startInstallPoll() {
    stopInstallPoll();
    installPoll = setInterval(pollInstall, 1000);
  }
  async function pollInstall() {
    try {
      installStatus = await api.getVoiceInstallStatus();
      if (installStatus.done) {
        installing = false;
        stopInstallPoll();
        refresh();
      }
    } catch (e) {
      console.warn('voice install status', e);
    }
  }
  function stopInstallPoll() {
    if (installPoll) { clearInterval(installPoll); installPoll = null; }
  }

  const MODES: { id: api.BraincraftMode; label: string; icon: string; blurb: string }[] = [
    { id: 'ai',         label: 'AI face',    icon: 'braincraft', blurb: 'Persona + last reply on the TFT' },
    { id: 'viewfinder', label: 'Camera',     icon: 'monitor',    blurb: 'Live camera · btn=photo · ←→ record' },
    { id: 'analyze',    label: 'Analyze',    icon: 'spark',      blurb: 'AI video feed · OCR / Hailo boxes' },
    { id: 'voice',      label: 'Voice chat', icon: 'zap',        blurb: 'Push-to-talk · mic → LLM → speaker' },
    { id: 'volume',     label: 'Volume',     icon: 'cpu',        blurb: 'Speaker + mic levels on the HAT' },
  ];
  const okPill = (ok: boolean | undefined) =>
    ok ? 'bg-live-500/20 text-live-400 border border-live-500/40'
       : 'bg-ink-800 text-ink-400 border border-steel-700';
</script>

<div class="page-void min-h-screen">
  <header class="flex items-center justify-between px-5 py-3 chrome-header">
    <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
    <h1 class="font-mono text-lg text-violet-300 inline-flex items-center gap-2">
      <Icon name="braincraft" class="w-5 h-5 text-violet-300" /> BrainCraft
    </h1>
    <div class="w-32"></div>
  </header>

  <main class="max-w-3xl mx-auto px-5 py-6 space-y-5">
    {#if loadErr}
      <div class="rounded-sm border border-red-500/40 bg-red-500/10 p-4 text-sm text-red-300">{loadErr}</div>
    {/if}

    {#if !status}
      {#if !loadErr}<p class="text-ink-400 text-sm">Loading BrainCraft status…</p>{/if}

    {:else}
      <!-- ── Enable card ─────────────────────────────────────────────── -->
      <div class="panel p-5 flex items-center justify-between gap-3">
        <div class="flex items-center gap-3 min-w-0">
          <div class="text-violet-300 shrink-0"><Icon name="braincraft" class="w-8 h-8" /></div>
          <div class="min-w-0">
            <div class="font-mono text-violet-200">Adafruit BrainCraft HAT</div>
            <div class="text-xs text-ink-400">
              {#if !enabled}Disabled — the daemon is idle.
              {:else if present}Active{#if dev?.display?.driver} · {dev.display.driver} {dev.display.w}×{dev.display.h}{/if}
              {:else}Enabled, but no HAT / display detected yet.{/if}
            </div>
          </div>
        </div>
        <button class="btn text-sm shrink-0 {enabled ? 'border-red-500/40 text-red-300 hover:bg-red-500/10' : 'border-violet-500/40 text-violet-200 hover:bg-violet-500/10'}"
                on:click={() => setEnabled(!enabled)} disabled={busy}>
          {enabled ? 'Disable' : 'Enable'}
        </button>
      </div>

      {#if saveErr}<div class="rounded border border-red-500/40 bg-red-500/10 p-2.5 text-xs text-red-300">{saveErr}</div>{/if}

      {#if enabled}
        <!-- ── Mode switch (matches on-HAT menu: joy press → list, ↑↓, btn) ─ -->
        <div class="grid grid-cols-2 sm:grid-cols-3 gap-3">
          {#each MODES as m (m.id)}
            <button class="rounded-sm border p-3 text-left transition
                          {mode === m.id ? 'border-violet-500/50 bg-violet-600/15' : 'border-steel-700 bg-ink-900 hover:bg-ink-800'}"
                    on:click={() => setMode(m.id)} disabled={busy}>
              <div class="inline-flex items-center gap-2 font-mono text-sm {mode === m.id ? 'text-violet-200' : 'text-ink-200'}">
                <Icon name={m.icon} class="w-4 h-4" /> {m.label}
              </div>
              <div class="text-[11px] text-ink-500 mt-1 leading-snug">{m.blurb}</div>
            </button>
          {/each}
        </div>
        <p class="text-[11px] text-ink-500 font-mono leading-relaxed">
          On the HAT: <span class="text-ink-300">joystick press</span> opens the menu ·
          <span class="text-ink-300">↑↓</span> navigate ·
          <span class="text-ink-300">button</span> select ·
          volume mode uses ↑↓ speaker / ←→ mic.
        </p>

        {#if !present}
          {@const reason = dev?.reason}
          {@const needsInstall = reason === 'libs_missing' || (!!voice && !voice.installed)}
          <div class="panel p-6 space-y-3">
            <div class="flex justify-center text-ink-600"><Icon name="braincraft" class="w-9 h-9" /></div>

            {#if needsInstall}
              <!-- The display+voice stack isn't installed — offer it HERE (the daemon
                   can't report present:true until these libs exist, so the install
                   must be reachable from the not-present state). -->
              <div class="text-center space-y-1.5">
                <div class="font-mono text-violet-200 text-sm">Install the display + voice stack</div>
                <p class="text-xs text-ink-500 leading-relaxed max-w-md mx-auto">
                  The HAT's drivers — ST7789 display + sherpa-onnx Kokoro&nbsp;TTS / Whisper&nbsp;ASR (~550&nbsp;MB) —
                  aren't baked into the image. Install them to light up the screen. Non-blocking;
                  progress resumes if you leave. A reboot may be needed the first time.
                </p>
              </div>
              {#if installStatus}
                <div class="space-y-2 max-w-md mx-auto">
                  <div class="flex items-center justify-between text-xs font-mono">
                    <span class="text-violet-300">{installStatus.phase ?? 'installing'}…</span>
                    <span class="text-ink-400">{installStatus.percent ?? 0}%</span>
                  </div>
                  <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
                    <div class="h-full bg-violet-500 transition-all duration-500" style={`width:${installStatus.percent ?? 0}%`}></div>
                  </div>
                  {#if installStatus.log}
                    <pre class="text-[10px] leading-snug font-mono text-ink-400 bg-ink-950 border border-ink-800 rounded p-2 max-h-40 overflow-y-auto whitespace-pre-wrap">{installStatus.log}</pre>
                  {/if}
                  {#if installStatus.done}
                    <div class="rounded border p-2.5 text-xs {installStatus.phase === 'failed' ? 'border-red-500/40 bg-red-500/10 text-red-300' : 'border-emerald-500/40 bg-emerald-500/10 text-emerald-200'}">
                      {installStatus.phase === 'failed' ? 'Install failed — check the log above.' : '✓ Stack installed. If the screen stays dark, reboot once.'}
                    </div>
                  {/if}
                </div>
              {:else}
                {#if installErr}<div class="rounded border border-red-500/40 bg-red-500/10 p-2.5 text-xs text-red-300 max-w-md mx-auto">{installErr}</div>{/if}
                <div class="flex justify-center">
                  <button class="btn-primary text-sm bg-violet-600 hover:bg-violet-500 active:bg-violet-700"
                          on:click={startInstall} disabled={installing}>
                    {installing ? 'Starting…' : 'Install display + voice stack'}
                  </button>
                </div>
              {/if}

            {:else if reason === 'no_spi'}
              <div class="text-center space-y-1.5">
                <div class="font-mono text-amber-300 text-sm">SPI bus not found</div>
                <p class="text-xs text-ink-500 leading-relaxed max-w-md mx-auto">
                  The display stack is installed but there's no <code class="text-ink-400">/dev/spidev*</code>.
                  Ensure <code class="text-ink-400">dtparam=spi=on</code> is in <code>config.txt</code> and reboot.
                </p>
              </div>

            {:else}
              <!-- Stack installed + SPI up, but the panel didn't respond → seating. -->
              <div class="text-center space-y-1.5">
                <div class="font-mono text-ink-300 text-sm">Waiting for the HAT…</div>
                <p class="text-xs text-ink-500 leading-relaxed max-w-md mx-auto">
                  The driver stack is installed and SPI is up, but the ST7789 didn't respond. Reseat the
                  BrainCraft HAT (on a ~16&nbsp;mm header if it's stacked over an AI&nbsp;HAT+ to clear the
                  heatsink), and check the GPIO pins are fully engaged.
                </p>
              </div>
            {/if}
          </div>
        {:else}
          <!-- ── Device panel ────────────────────────────────────────── -->
          <div class="panel p-5 grid grid-cols-1 sm:grid-cols-3 gap-3">
            <div class="rounded-sm bg-ink-950 border border-ink-800 p-3 space-y-1">
              <div class="text-[10px] uppercase tracking-wider text-ink-500">Display</div>
              <div class="font-mono text-sm text-violet-200">{dev?.display?.driver ?? '—'}</div>
              <span class="pill text-[10px] {okPill(dev?.display?.ok)}">{dev?.display?.ok ? 'ok' : 'off'}</span>
            </div>
            <div class="rounded-sm bg-ink-950 border border-ink-800 p-3 space-y-1">
              <div class="text-[10px] uppercase tracking-wider text-ink-500">Buttons</div>
              <div class="font-mono text-sm text-violet-200">{dev?.buttons?.chip ?? '—'}</div>
              <span class="pill text-[10px] {okPill(dev?.buttons?.ok)}">{dev?.buttons?.ok ? 'ok' : 'off'}</span>
            </div>
            <div class="rounded-sm bg-ink-950 border border-ink-800 p-3 space-y-1">
              <div class="text-[10px] uppercase tracking-wider text-ink-500">Audio</div>
              <div class="font-mono text-sm text-violet-200">{dev?.audio?.device ?? '—'}</div>
              <div class="flex gap-1 flex-wrap">
                <span class="pill text-[10px] {okPill(dev?.audio?.playback)}">out</span>
                <span class="pill text-[10px] {okPill(dev?.audio?.capture)}">mic</span>
              </div>
            </div>
          </div>

          {#if actErr}<div class="rounded border border-red-500/40 bg-red-500/10 p-2.5 text-xs text-red-300">{actErr}</div>{/if}

          <!-- ── Viewfinder controls ─────────────────────────────────── -->
          {#if mode === 'viewfinder'}
            <div class="panel p-5 space-y-3">
              <div class="flex items-center justify-between">
                <h2 class="font-mono text-sm text-ink-300">Viewfinder</h2>
                <label class="inline-flex items-center gap-2 text-xs text-ink-400 cursor-pointer">
                  <input type="checkbox" checked={cfg?.overlay} on:change={(e) => setOverlay(e.currentTarget.checked)} disabled={busy} />
                  AI overlay
                </label>
              </div>
              <div class="flex gap-2 flex-wrap">
                <button class="btn text-sm border-violet-500/40 text-violet-200 hover:bg-violet-500/10" on:click={doCapture}>
                  <Icon name="monitor" class="w-4 h-4 mr-1.5" /> Photo
                </button>
                <button class="btn text-sm {recording ? 'border-red-500/50 text-red-300 hover:bg-red-500/10' : 'border-violet-500/40 text-violet-200 hover:bg-violet-500/10'}"
                        on:click={toggleRecord}>
                  {#if recording}<span class="h-2 w-2 rounded-full bg-red-500 animate-pulse mr-1.5"></span>Stop recording{:else}Record video{/if}
                </button>
              </div>
              {#if dev?.captures?.last_photo}
                <div class="text-[11px] font-mono text-ink-500">last photo: <span class="text-ink-400">{dev.captures.last_photo}</span></div>
              {/if}
              <p class="text-[11px] text-ink-500 leading-relaxed">
                On the HAT itself, <span class="text-ink-400">Button&nbsp;A</span> takes a photo and
                <span class="text-ink-400">Button&nbsp;B</span> toggles recording — these buttons mirror that.
              </p>
            </div>
          {/if}

          <!-- ── Voice panel ─────────────────────────────────────────── -->
          <div class="panel p-5 space-y-4">
            <h2 class="font-mono text-sm text-ink-300 inline-flex items-center gap-2"><Icon name="zap" class="w-4 h-4 text-violet-300" /> Voice</h2>

            {#if !voice?.installed}
              {#if installStatus}
                <div class="space-y-2">
                  <div class="flex items-center justify-between text-xs font-mono">
                    <span class="text-violet-300">{installStatus.phase ?? 'installing'}…</span>
                    <span class="text-ink-400">{installStatus.percent ?? 0}%</span>
                  </div>
                  <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
                    <div class="h-full bg-violet-500 transition-all duration-500" style={`width:${installStatus.percent ?? 0}%`}></div>
                  </div>
                  {#if installStatus.log}
                    <pre class="text-[10px] leading-snug font-mono text-ink-400 bg-ink-950 border border-ink-800 rounded p-2 max-h-40 overflow-y-auto whitespace-pre-wrap">{installStatus.log}</pre>
                  {/if}
                  {#if installStatus.done}
                    <div class="rounded border p-2.5 text-xs {installStatus.phase === 'failed' ? 'border-red-500/40 bg-red-500/10 text-red-300' : 'border-emerald-500/40 bg-emerald-500/10 text-emerald-200'}">
                      {installStatus.phase === 'failed' ? 'Install failed — check the log above.' : '✓ Voice stack installed.'}
                    </div>
                  {/if}
                </div>
              {:else}
                {#if installErr}<div class="rounded border border-red-500/40 bg-red-500/10 p-2.5 text-xs text-red-300">{installErr}</div>{/if}
                <p class="text-[12px] text-ink-400 leading-relaxed">
                  The voice/display stack (sherpa-onnx Kokoro TTS + Whisper ASR + display/audio libs, ~550&nbsp;MB)
                  isn't baked into the image. Install it on demand — non-blocking, progress resumes on return.
                </p>
                <button class="btn-primary text-sm bg-violet-600 hover:bg-violet-500 active:bg-violet-700"
                        on:click={startInstall} disabled={installing}>
                  {installing ? 'Starting…' : 'Install voice stack'}
                </button>
              {/if}
            {:else}
              <!-- Backend selector -->
              <div class="space-y-2">
                <div class="text-[11px] uppercase tracking-wider text-ink-500">Backend</div>
                <div class="grid grid-cols-2 gap-2">
                  <button class="rounded-sm border p-2.5 text-left transition {cfg?.backend.mode === 'local' ? 'border-violet-500/50 bg-violet-600/15 text-violet-200' : 'border-steel-700 bg-ink-950 hover:bg-ink-800 text-ink-200'}"
                          on:click={() => setBackend('local')} disabled={busy}>
                    <div class="font-mono text-sm">Local</div>
                    <div class="text-[11px] text-ink-500 mt-0.5">Kokoro + Whisper on-device</div>
                  </button>
                  <button class="rounded-sm border p-2.5 text-left transition {cfg?.backend.mode === 'hosted' ? 'border-violet-500/50 bg-violet-600/15 text-violet-200' : 'border-steel-700 bg-ink-950 hover:bg-ink-800 text-ink-200'}"
                          on:click={() => setBackend('hosted')} disabled={busy}>
                    <div class="font-mono text-sm">Hosted</div>
                    <div class="text-[11px] text-ink-500 mt-0.5">DGX Spark + persona voice</div>
                  </button>
                </div>
              </div>

              {#if cfg?.backend.mode === 'hosted'}
                <div class="space-y-2">
                  <label class="block space-y-1">
                    <span class="block text-[11px] uppercase tracking-wider text-ink-500">Persona</span>
                    <input class="input w-full text-sm" bind:value={persona} placeholder="e.g. assistant"
                           on:blur={saveHosted} on:keydown={(e) => e.key === 'Enter' && saveHosted()} />
                  </label>
                  <button class="text-[11px] text-violet-400 hover:underline" on:click={() => (showAdvanced = !showAdvanced)}>
                    {showAdvanced ? 'Hide' : 'Edit'} endpoints
                  </button>
                  {#if showAdvanced}
                    <div class="space-y-2 pt-1">
                      <label class="block space-y-1">
                        <span class="block text-[10px] uppercase tracking-wider text-ink-500">LLM (OpenAI-compatible)</span>
                        <input class="input w-full text-xs font-mono" bind:value={llmUrl} on:blur={saveHosted} />
                      </label>
                      <label class="block space-y-1">
                        <span class="block text-[10px] uppercase tracking-wider text-ink-500">TTS</span>
                        <input class="input w-full text-xs font-mono" bind:value={ttsUrl} on:blur={saveHosted} />
                      </label>
                      <label class="block space-y-1">
                        <span class="block text-[10px] uppercase tracking-wider text-ink-500">ASR</span>
                        <input class="input w-full text-xs font-mono" bind:value={asrUrl} on:blur={saveHosted} />
                      </label>
                    </div>
                  {/if}
                  <div class="text-[11px] font-mono {dev?.backend?.reachable ? 'text-live-400' : 'text-amber-400'}">
                    {dev?.backend?.reachable ? '● endpoints reachable' : '○ endpoints unreachable'}
                  </div>
                </div>
              {/if}

              <!-- Last exchange -->
              {#if dev?.last_utterance?.text || dev?.last_response?.text}
                <div class="rounded-sm bg-ink-950 border border-ink-800 p-3 space-y-1.5 text-sm">
                  {#if dev?.last_utterance?.text}<div><span class="text-ink-500 font-mono text-xs">you</span> <span class="text-ink-200">{dev.last_utterance.text}</span></div>{/if}
                  {#if dev?.last_response?.text}<div><span class="text-violet-400 font-mono text-xs">{cfg?.backend.persona || 'orb'}</span> <span class="text-ink-100">{dev.last_response.text}</span></div>{/if}
                </div>
              {/if}

              <!-- Say box -->
              <div class="flex gap-2">
                <input class="input flex-1 text-sm" bind:value={sayText} placeholder="Type for the Orb to speak…"
                       on:keydown={(e) => e.key === 'Enter' && say()} />
                <button class="btn text-sm border-violet-500/40 text-violet-200 hover:bg-violet-500/10" on:click={say} disabled={!sayText.trim()}>Speak</button>
              </div>
              <p class="text-[11px] text-ink-500">On the HAT, <span class="text-ink-400">Button&nbsp;A</span> is push-to-talk.</p>

              <!-- Audio levels: WM8960 in/out volume via /api/audio/volume -->
              {#if audioVol?.present}
                <div class="rounded-sm bg-ink-950 border border-ink-800 p-3 space-y-3">
                  <div class="text-[11px] uppercase tracking-wider text-ink-500">
                    Audio levels <span class="text-ink-600 font-mono">{audioVol.card?.name}</span>
                  </div>
                  {#if audioVol.playback}
                    <label class="block space-y-1">
                      <div class="flex justify-between text-xs">
                        <span class="text-ink-400">Output (speaker)</span>
                        <span class="font-mono text-violet-300">{outVol}%</span>
                      </div>
                      <input type="range" min="0" max="100" step="1" bind:value={outVol}
                             on:change={() => saveAudio({ playback: outVol })}
                             disabled={audioBusy} class="w-full accent-violet-500" />
                    </label>
                  {/if}
                  {#if audioVol.capture}
                    <label class="block space-y-1">
                      <div class="flex justify-between text-xs">
                        <span class="text-ink-400">Input (mic)</span>
                        <span class="font-mono text-violet-300">{micMuted ? 'muted' : inVol + '%'}</span>
                      </div>
                      <div class="flex items-center gap-2">
                        <input type="range" min="0" max="100" step="1" bind:value={inVol}
                               on:change={() => saveAudio({ capture: inVol })}
                               disabled={audioBusy || micMuted} class="flex-1 accent-violet-500" />
                        <button class="btn text-xs px-2 py-1 {micMuted ? 'border-red-500/40 text-red-300' : 'border-steel-700 text-ink-300'}"
                                on:click={() => { micMuted = !micMuted; saveAudio({ capture_muted: micMuted }); }}
                                disabled={audioBusy}>{micMuted ? 'unmute' : 'mute'}</button>
                      </div>
                    </label>
                  {/if}
                </div>
              {/if}
            {/if}
          </div>
        {/if}
      {/if}
    {/if}
  </main>
</div>
