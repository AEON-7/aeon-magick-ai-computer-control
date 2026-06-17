<script lang="ts">
  // Pi-side maintenance + diagnostics.
  //
  // This page is deliberately separate from /target/* (which controls
  // the USB-connected machine). It surfaces Pi health metrics and the
  // rare-but-needed Pi reboot / poweroff actions that v53 removed
  // from the header alongside the misleadingly-labelled target
  // controls.
  //
  // Reboot/poweroff actions are double-confirmed (yes/no, then type
  // the action word) so a stray click can't kill an in-flight AI
  // session.

  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let info: api.SystemInfo | null = null;
  let loading = true;
  let error = '';
  let msg = '';
  let poll: ReturnType<typeof setInterval>;

  // v63: live streamer tuning. fps + jpeg_quality knobs go through
  // GET/PUT /api/streamer/config which writes streamer.toml + bounces
  // aeon-streamer.service. The page also surfaces the read-only
  // format/resolution/hw_accel values so operators know what they're
  // dialing in latency against.
  let streamerCfg: {
    fps: number;
    jpeg_quality: number;
    width: number;
    height: number;
    match_source: boolean;
    format: string;
    hw_accel: boolean;
  } | null = null;
  let stagedFps = 24;
  let stagedQuality = 70;
  // 'match' = track source res (capped 1080p); 'WxH' = fixed encode size.
  // Lower sizes cut the Pi 5 software-H.264 CPU/power on the capture paths.
  let stagedResolution = 'match';
  const RES_PRESETS = ['match', '1920x1080', '1280x720', '960x540', '640x360'];
  let streamerSaving = false;
  let streamerMsg = '';

  async function refreshStreamer() {
    try {
      const r = await fetch('/api/streamer/config', { credentials: 'same-origin' })
        .then((r) => r.json());
      if (r.ok) {
        streamerCfg = {
          fps: r.fps,
          jpeg_quality: r.jpeg_quality,
          width: r.width,
          height: r.height,
          match_source: r.match_source ?? true,
          format: r.format,
          hw_accel: r.hw_accel,
        };
        // Pre-fill the sliders with the saved values so the user sees
        // where they currently are; let them tweak without losing
        // context.
        if (!streamerSaving) {
          stagedFps = r.fps;
          stagedQuality = r.jpeg_quality;
          stagedResolution = (r.match_source ?? true) ? 'match' : `${r.width}x${r.height}`;
        }
      }
    } catch {
      // Best-effort — older supervisor builds won't have this endpoint.
      streamerCfg = null;
    }
  }

  async function saveStreamer() {
    streamerSaving = true;
    streamerMsg = '';
    try {
      // 'match' tracks the source; otherwise force a fixed W×H to cut encode CPU.
      const resPatch =
        stagedResolution === 'match'
          ? { match_source: true }
          : (() => {
              const [w, h] = stagedResolution.split('x').map(Number);
              return { match_source: false, width: w, height: h };
            })();
      const r = await fetch('/api/streamer/config', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({
          fps: stagedFps,
          jpeg_quality: stagedQuality,
          ...resPatch,
        }),
      }).then((r) => r.json());
      if (r.ok) {
        streamerMsg = `✓ saved + restarted aeon-streamer (fps=${stagedFps}, res=${stagedResolution}, q=${stagedQuality})`;
        await refreshStreamer();
      } else {
        streamerMsg = `✗ ${r.err ?? 'save failed'}`;
      }
    } catch (e: any) {
      streamerMsg = `✗ ${e?.message ?? 'save failed'}`;
    } finally {
      streamerSaving = false;
      setTimeout(() => (streamerMsg = ''), 5000);
    }
  }

  async function refresh() {
    try {
      info = await api.getSystemInfo();
      loading = false;
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
      loading = false;
    }
  }

  onMount(() => {
    refresh();
    refreshStreamer();
    poll = setInterval(refresh, 5000);
  });
  onDestroy(() => { if (poll) clearInterval(poll); });

  function fmtUptime(s: number): string {
    const d = Math.floor(s / 86400);
    const h = Math.floor((s % 86400) / 3600);
    const m = Math.floor((s % 3600) / 60);
    const parts = [];
    if (d > 0) parts.push(`${d}d`);
    if (h > 0 || d > 0) parts.push(`${h}h`);
    parts.push(`${m}m`);
    return parts.join(' ');
  }
  function fmtBytesKB(kb: number): string {
    if (kb < 1024) return `${kb} kB`;
    if (kb < 1024 * 1024) return `${(kb / 1024).toFixed(1)} MB`;
    return `${(kb / 1024 / 1024).toFixed(2)} GB`;
  }

  /// Same double-confirm pattern as the header's target buttons —
  /// confirm() prompts a yes/no, then prompt() asks the user to type
  /// the action word. Avoids a stray click killing the box.
  async function onPiReboot() {
    if (!confirm(
      'Reboot the Pi?\n\n' +
      'The web UI disconnects for ~30-60 s while the Pi reboots. ' +
      'Any in-flight HID input, capture, MCP session, AI agent run, ' +
      'or open SSH sessions will be lost. The USB gadget will re-' +
      'enumerate — the target sees a ~1 s blip but stays on.'
    )) return;
    const phrase = prompt('Type REBOOT to confirm:');
    if (phrase !== 'REBOOT') return;
    try {
      const r = await api.rebootPi();
      msg = r.message;
    } catch (e: any) {
      error = 'Reboot failed: ' + (e?.message ?? 'unknown');
    }
  }

  async function onPiPoweroff() {
    if (!confirm(
      'Power off the Pi?\n\n' +
      'There is NO remote way to bring it back — you\'ll need to ' +
      'physically reach the device and unplug/replug power, or hit ' +
      'a remote-controlled smart plug if you have one. Only do this ' +
      'if you\'re moving the Pi or shutting down a long-running ' +
      'session you don\'t need anymore.'
    )) return;
    const phrase = prompt('Type POWEROFF to confirm:');
    if (phrase !== 'POWEROFF') return;
    try {
      const r = await api.poweroffPi();
      msg = r.message;
    } catch (e: any) {
      error = 'Poweroff failed: ' + (e?.message ?? 'unknown');
    }
  }

  // ── Configuration backup / restore ──
  // Password-encrypted snapshot of /etc/aeon + agent-connect keys/registry +
  // Tailscale identity. Export streams an encrypted blob to download; import
  // base64s the file back up, the supervisor decrypts + restores, then reboot.
  let bkExportPw = '';
  let bkImportPw = '';
  let bkFile: File | null = null;
  let bkBusy: '' | 'export' | 'import' = '';
  let bkMsg = '';
  let bkOk = false;
  let bkRestored = false;

  function onBkFile(e: Event) {
    const t = e.target as HTMLInputElement;
    bkFile = t.files && t.files[0] ? t.files[0] : null;
  }

  async function doExport() {
    bkBusy = 'export'; bkMsg = ''; bkRestored = false;
    try {
      const res = await fetch('/api/system/config/export', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: bkExportPw }),
      });
      if (!res.ok) { bkOk = false; bkMsg = `✗ export failed (${res.status})`; return; }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url; a.download = 'aeon-config-backup.aeonbackup';
      document.body.appendChild(a); a.click(); a.remove();
      URL.revokeObjectURL(url);
      bkOk = true;
      bkMsg = `✓ backup downloaded (${(blob.size / 1024).toFixed(1)} kB) — store the file + password safely`;
    } catch (e: any) {
      bkOk = false; bkMsg = `✗ ${e?.message ?? 'export failed'}`;
    } finally { bkBusy = ''; }
  }

  async function doImport() {
    if (!bkFile) return;
    if (!confirm(
      'Restore configuration from this backup?\n\n' +
      'This OVERWRITES the current device + network config, API tokens, the ' +
      'admin password, and the connected-systems registry, then needs a reboot ' +
      'to apply. Continue?'
    )) return;
    bkBusy = 'import'; bkMsg = ''; bkRestored = false;
    try {
      const b64 = await new Promise<string>((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(((r.result as string).split(',')[1]) ?? '');
        r.onerror = () => reject(new Error('read failed'));
        r.readAsDataURL(bkFile as File);
      });
      const r = await fetch('/api/system/config/import', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: bkImportPw, data_b64: b64 }),
      }).then((r) => r.json());
      if (r.ok) { bkOk = true; bkMsg = `✓ ${r.message ?? 'restored'}`; bkRestored = true; }
      else { bkOk = false; bkMsg = `✗ ${r.err ?? 'restore failed'}`; }
    } catch (e: any) {
      bkOk = false; bkMsg = `✗ ${e?.message ?? 'restore failed'}`;
    } finally { bkBusy = ''; }
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← AEON MAGICK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">Pi system</span>
    </div>
  </header>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-3xl mx-auto w-full space-y-6">

      {#if loading}<p class="text-zinc-500 text-sm">loading…</p>{/if}
      {#if error}<p class="text-red-400 text-sm">{error}</p>{/if}
      {#if msg}<p class="text-live-400 text-sm">{msg}</p>{/if}

      <!-- ─── Health ─── -->
      {#if info}
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <header class="space-y-1">
            <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              Health
            </h2>
            <p class="text-xs text-zinc-500">
              Pi-side resource state. Polls every 5 s. CPU temp above 80°C
              triggers Pi thermal throttling — usually fine to ignore on a
              passive heatsink under sustained load.
            </p>
          </header>

          <div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
            <div class="p-3 rounded bg-ink-950/40 border border-ink-800">
              <p class="text-[10px] uppercase tracking-wider text-zinc-500">uptime</p>
              <p class="font-mono text-base text-zinc-200 mt-1">{fmtUptime(info.uptime_seconds)}</p>
            </div>
            <div class="p-3 rounded bg-ink-950/40 border border-ink-800">
              <p class="text-[10px] uppercase tracking-wider text-zinc-500">cpu temp</p>
              <p class="font-mono text-base mt-1
                        {info.cpu_temp_c >= 80 ? 'text-red-400' :
                         info.cpu_temp_c >= 70 ? 'text-amber-400' :
                         'text-live-300'}">
                {info.cpu_temp_c.toFixed(1)}°C
              </p>
            </div>
            <div class="p-3 rounded bg-ink-950/40 border border-ink-800">
              <p class="text-[10px] uppercase tracking-wider text-zinc-500">load (1m / 5m / 15m)</p>
              <p class="font-mono text-base text-zinc-200 mt-1">
                {info.loadavg['1m'].toFixed(2)} / {info.loadavg['5m'].toFixed(2)} / {info.loadavg['15m'].toFixed(2)}
              </p>
              <p class="text-[10px] text-zinc-600 mt-0.5">cores: {info.cpu_count}</p>
            </div>
            <div class="p-3 rounded bg-ink-950/40 border border-ink-800">
              <p class="text-[10px] uppercase tracking-wider text-zinc-500">memory</p>
              <p class="font-mono text-base text-zinc-200 mt-1">
                {fmtBytesKB(info.mem_available_kb)}
              </p>
              <p class="text-[10px] text-zinc-600 mt-0.5">
                of {fmtBytesKB(info.mem_total_kb)} avail
              </p>
            </div>
          </div>
        </section>
      {/if}

      <!-- ─── Stream tuning (v63) ─── -->
      {#if streamerCfg}
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
          <header class="space-y-1">
            <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              Stream tuning
            </h2>
            <p class="text-xs text-zinc-500 leading-relaxed">
              Live knobs for the H.264 capture pipeline. The Cam Link
              captures at 60fps, so frame rate is restricted to its clean
              divisors (15 / 30 / 60) — that keeps frame-drops even (every
              4th / 2nd / all). 60 is smoothest but only sustainable at lower
              source resolutions; use 30 or 15 at 1080p. JPEG quality now
              only affects the /snapshot image + MJPEG fallback, not the live
              H.264 stream.
            </p>
            <p class="text-[11px] text-zinc-600 leading-relaxed pt-1">
              Source resolution {streamerCfg.width}×{streamerCfg.height},
              format <code>{streamerCfg.format}</code>,
              hw_accel <code>{streamerCfg.hw_accel ? 'on' : 'off'}</code>.
              Saving these settings restarts aeon-streamer (the live
              stream drops for ~2 s).
            </p>
          </header>

          <!-- fps: discrete divisors of the 60fps capture, so frame-dropping
               is deterministic (keep every 4th / 2nd / all frame). -->
          <div class="space-y-1">
            <div class="flex justify-between text-[11px] uppercase tracking-wider">
              <span class="text-zinc-500">Frame rate</span>
              <span class="font-mono text-cursed-300">{stagedFps} fps</span>
            </div>
            <div class="flex gap-2">
              {#each [15, 30, 60] as f}
                <button type="button"
                        on:click={() => (stagedFps = f)}
                        disabled={streamerSaving}
                        class="flex-1 rounded-md border px-3 py-2 font-mono text-sm transition
                               {stagedFps === f
                                 ? 'border-cursed-500 bg-cursed-500/20 text-cursed-300'
                                 : 'border-ink-800 text-zinc-400 hover:border-zinc-700'}">
                  {f} fps
                </button>
              {/each}
            </div>
            <div class="flex justify-between text-[10px] text-zinc-600 font-mono">
              <span>15 (low bandwidth)</span>
              <span>30 (default)</span>
              <span>60 (smoothest — low-res only)</span>
            </div>
          </div>

          <!-- encode resolution: the Pi 5 has no HW H.264 encoder, so lowering
               this cuts software-libx264 CPU + power on the capture-card paths.
               'Match source' tracks the input (capped 1080p). -->
          <div class="space-y-1">
            <div class="flex justify-between text-[11px] uppercase tracking-wider">
              <span class="text-zinc-500">Encode resolution</span>
              <span class="font-mono text-cursed-300">
                {stagedResolution === 'match' ? 'match source' : stagedResolution}
              </span>
            </div>
            <div class="flex flex-wrap gap-2">
              {#each RES_PRESETS as r}
                <button type="button"
                        on:click={() => (stagedResolution = r)}
                        disabled={streamerSaving}
                        class="flex-1 min-w-[88px] rounded-md border px-3 py-2 font-mono text-xs transition
                               {stagedResolution === r
                                 ? 'border-cursed-500 bg-cursed-500/20 text-cursed-300'
                                 : 'border-ink-800 text-zinc-400 hover:border-zinc-700'}">
                  {r === 'match' ? 'match' : r.replace('x', '×')}
                </button>
              {/each}
            </div>
            <p class="text-[10px] text-zinc-600 font-mono">
              Lower = less CPU/power (Pi 5 software-encodes H.264). 720p ≈ ½ the
              encode load of 1080p; pair with 15 fps on marginal power.
            </p>
          </div>

          <!-- quality slider -->
          <div class="space-y-1">
            <div class="flex justify-between text-[11px] uppercase tracking-wider">
              <span class="text-zinc-500">JPEG quality</span>
              <span class="font-mono text-cursed-300">{stagedQuality}</span>
            </div>
            <input type="range" min="40" max="95" step="1"
                   bind:value={stagedQuality}
                   disabled={streamerSaving}
                   class="w-full accent-cursed-500" />
            <div class="flex justify-between text-[10px] text-zinc-600 font-mono">
              <span>40 (mushy, low bandwidth)</span>
              <span>70 (default, lossless-feeling)</span>
              <span>95 (zero compression artifacts)</span>
            </div>
          </div>

          <div class="flex items-center gap-3 pt-2 border-t border-ink-800">
            <button class="btn-primary text-sm"
                    on:click={saveStreamer}
                    disabled={streamerSaving ||
                              (stagedFps === streamerCfg.fps
                                && stagedQuality === streamerCfg.jpeg_quality)}>
              {streamerSaving ? 'saving + restarting…' : 'Save & restart streamer'}
            </button>
            {#if streamerMsg}
              <span class="text-xs font-mono
                           {streamerMsg.startsWith('✓') ? 'text-live-300' : 'text-red-300'}">
                {streamerMsg}
              </span>
            {/if}
          </div>

          <details class="pt-3 border-t border-ink-800">
            <summary class="cursor-pointer text-[11px] uppercase tracking-wider
                            text-zinc-500 hover:text-zinc-300">
              ▸ Where does the latency come from?
            </summary>
            <div class="mt-3 space-y-2 text-[11px] text-zinc-500 leading-relaxed">
              <p>
                The capture chain on a Pi 4 is roughly:
                <strong>Cam Link (~50 ms internal queue)</strong> →
                <strong>ffmpeg/ustreamer encode (~30 ms)</strong> →
                <strong>axum HTTPS body stream (~5 ms)</strong> →
                <strong>browser multipart parser (variable)</strong>.
                The first three add up to ~85 ms steady state — fast.
                The fourth is where multi-second "lag" usually lives:
                browsers buffer multipart-MJPEG aggressively, and over
                WiFi a slow drain piles frames into that buffer faster
                than the consumer reads them.
              </p>
              <p>
                The real fix is dropping the MJPEG transport entirely
                and shipping H.264 over WebSocket — Pi 4's hardware H.264
                encoder is already there. That's a focused upcoming PR
                (v64+); for now the levers above are what we have.
              </p>
            </div>
          </details>
        </section>
      {/if}

      <!-- ─── Configuration backup ─── -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
        <header class="space-y-1">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Configuration backup
          </h2>
          <p class="text-xs text-zinc-500 leading-relaxed">
            A <strong>password-encrypted</strong> snapshot of everything you'd want back
            after a re-flash: device + network / VPN settings, saved WiFi networks, API tokens, the admin
            password, macros + prompts, the <strong>agent-connect SSH key + connected-systems
            registry</strong> (so restored boxes are still trusted — no re-register), and the
            Tailscale identity. Uploaded ISOs + staged files are excluded (re-upload those).
            The file is AES-256 encrypted with your password —
            <strong>store both safely; the password is the only way to decrypt it.</strong>
          </p>
        </header>

        <!-- Export -->
        <div class="space-y-2">
          <div class="text-[11px] uppercase tracking-wider text-zinc-500">Export</div>
          <div class="flex flex-wrap items-center gap-2">
            <input type="password" bind:value={bkExportPw} placeholder="encryption password"
                   autocomplete="new-password"
                   class="flex-1 min-w-[180px] rounded-md border border-ink-800 bg-ink-950/40 px-3 py-2 text-sm font-mono text-zinc-200 placeholder-zinc-600" />
            <button class="btn-primary text-sm" on:click={doExport} disabled={bkBusy !== '' || !bkExportPw}>
              {bkBusy === 'export' ? 'encrypting…' : '↓ Export & download'}
            </button>
          </div>
        </div>

        <!-- Restore -->
        <div class="space-y-2 pt-3 border-t border-ink-800">
          <div class="text-[11px] uppercase tracking-wider text-zinc-500">Restore</div>
          <p class="text-[11px] text-zinc-500 leading-relaxed">
            Restoring <strong>overwrites</strong> the current config with the backup, then
            needs a reboot. Use the same password the backup was created with.
          </p>
          <div class="flex flex-wrap items-center gap-2">
            <input type="file" accept=".aeonbackup,application/octet-stream" on:change={onBkFile}
                   class="text-xs text-zinc-400 file:mr-2 file:rounded file:border-0 file:bg-ink-800 file:px-3 file:py-1.5 file:text-zinc-300 file:cursor-pointer" />
            <input type="password" bind:value={bkImportPw} placeholder="backup password"
                   autocomplete="off"
                   class="flex-1 min-w-[160px] rounded-md border border-ink-800 bg-ink-950/40 px-3 py-2 text-sm font-mono text-zinc-200 placeholder-zinc-600" />
            <button class="btn-primary text-sm hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40"
                    on:click={doImport} disabled={bkBusy !== '' || !bkImportPw || !bkFile}>
              {bkBusy === 'import' ? 'restoring…' : '↑ Import & restore'}
            </button>
          </div>
        </div>

        {#if bkMsg}
          <p class="text-xs font-mono leading-relaxed {bkOk ? 'text-live-300' : 'text-red-300'}">{bkMsg}</p>
        {/if}
        {#if bkRestored}
          <button class="btn-primary text-sm hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40"
                  on:click={onPiReboot}>
            ⟳ Reboot now to apply
          </button>
        {/if}
      </section>

      <!-- ─── Pi maintenance ─── -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
        <header class="space-y-1">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Pi maintenance
          </h2>
          <p class="text-xs text-zinc-500 leading-relaxed">
            These control the <strong>Raspberry Pi itself</strong> — not the
            USB-connected target machine. (Target power lives in the
            header buttons.) The Pi is meant to stay up indefinitely;
            you only need these if you're moving the device, applying
            a firmware change that requires a reboot, or shutting down
            a session you no longer need.
          </p>
        </header>

        <div class="flex flex-wrap gap-3 pt-2 border-t border-ink-800">
          <button class="btn-primary text-sm
                         hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40"
                  on:click={onPiReboot}>
            ⟳ reboot the Pi
          </button>
          <button class="btn-primary text-sm
                         hover:bg-red-500/20 hover:text-red-300 hover:border-red-500/40"
                  on:click={onPiPoweroff}>
            ⏻ power off the Pi
          </button>
        </div>

        <p class="text-[11px] text-zinc-500 leading-relaxed pt-2 border-t border-ink-800">
          Both prompt twice — confirmation dialog, then a typed
          REBOOT / POWEROFF — so a stray click can't take the device
          down in the middle of a job. After poweroff there's no way
          to bring the Pi back without physical access (or a smart
          plug). Reboot takes 30-60 seconds; the USB gadget
          re-enumerates so the target sees a brief blip but stays on.
        </p>
      </section>
    </div>
  </main>
</div>
