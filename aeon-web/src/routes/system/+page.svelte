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

  import PageHeader from '$lib/components/PageHeader.svelte';
  import { confirmRite } from '$lib/confirm';
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
    loadImageUpdates();
    loadAuto();
    primeOsStatus();
    poll = setInterval(refresh, 5000);
  });
  onDestroy(() => { if (poll) clearInterval(poll); if (osPoll) clearInterval(osPoll); });

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

  /// A typed action word gates both — a stray click can never kill
  /// the box.
  async function onPiReboot() {
    if (!(await confirmRite({
      title: 'Reboot the Pi',
      body:
        'The web UI disconnects for ~30-60 s while the Pi reboots. ' +
        'Any in-flight HID input, capture, MCP session, AI agent run, ' +
        'or open SSH sessions will be lost. The USB gadget will re-' +
        'enumerate — the target sees a ~1 s blip but stays on.',
      danger: true,
      phrase: 'REBOOT',
      confirmLabel: 'reboot Pi',
    }))) return;
    try {
      const r = await api.rebootPi();
      msg = r.message;
    } catch (e: any) {
      error = 'Reboot failed: ' + (e?.message ?? 'unknown');
    }
  }

  async function onPiPoweroff() {
    if (!(await confirmRite({
      title: 'Power off the Pi',
      body:
        'There is NO remote way to bring it back — you\'ll need to ' +
        'physically reach the device and unplug/replug power, or hit ' +
        'a remote-controlled smart plug if you have one. Only do this ' +
        'if you\'re moving the Pi or shutting down a long-running ' +
        'session you don\'t need anymore.',
      danger: true,
      phrase: 'POWEROFF',
      confirmLabel: 'power off Pi',
    }))) return;
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
    if (!(await confirmRite({
      title: 'Restore config backup',
      body:
        'Restore configuration from this backup?\n\n' +
        'This OVERWRITES the current device + network config, API tokens, the ' +
        'admin password, and the connected-systems registry, then needs a reboot ' +
        'to apply.',
      danger: true,
      confirmLabel: 'restore',
    }))) return;
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

  // ── OS updates + new-image notifier ──
  // Two separate things: (1) keep the underlying Pi OS patched (apt), (2) notice
  // when a whole new Orb IMAGE has been published (v111/v112 → …) and guide the
  // user to back up + re-flash from Patreon. The Orb never self-flashes.
  let img: api.ImageUpdates | null = null;

  let osCheck: api.OsUpdateCheck | null = null;
  let osStatus: api.OsUpdateStatus | null = null;
  let osChecking = false;
  let osPoll: ReturnType<typeof setInterval> | null = null;

  let autoOn: boolean | null = null;
  let autoBusy = false;

  // Guided image upgrade: back up (encrypted, password) → then open Patreon.
  let showUpgrade = false;
  let upgradeStage: 'backup' | 'done' = 'backup';
  let upgradePw = '';
  let upgradeBusy = false;
  let upgradeMsg = '';

  $: osBusy = !!osStatus && !osStatus.done && osStatus.phase !== 'idle';

  async function loadImageUpdates() {
    try { img = await api.imageUpdates(); } catch { img = null; }
  }
  async function loadAuto() {
    try { const r = await api.autoUpdatesGet(); if (r.ok) autoOn = !!r.enabled; } catch {}
  }
  // If an apt upgrade was already running when the page opened, resume polling.
  async function primeOsStatus() {
    try {
      osStatus = await api.osUpdateStatus();
      if (osStatus && !osStatus.done) startOsPoll();
    } catch {}
  }

  async function doOsCheck() {
    osChecking = true;
    try { osCheck = await api.osUpdateCheck(); }
    catch (e: any) { osCheck = { ok: false, err: e?.message ?? 'check failed' }; }
    osChecking = false;
  }

  function startOsPoll() {
    if (osPoll) clearInterval(osPoll);
    osPoll = setInterval(async () => {
      try {
        osStatus = await api.osUpdateStatus();
        if (osStatus.done && osPoll) {
          clearInterval(osPoll); osPoll = null;
          if (osStatus.ok) osCheck = { ok: true, count: 0, security: 0 };
        }
      } catch {}
    }, 2500);
  }

  async function doOsApply() {
    if (!(await confirmRite({
      title: 'Install OS updates',
      body:
        'Download + install all pending Raspberry Pi OS package updates now. ' +
        'The Orb keeps running through it; if a kernel or firmware package is ' +
        'updated you\'ll be prompted to reboot afterward to apply it.',
      confirmLabel: 'install updates',
    }))) return;
    error = ''; msg = '';
    try {
      const r = await api.osUpdateApply();
      if (!r.ok) { error = 'Update failed: ' + (r.err ?? 'unknown'); return; }
      osStatus = { phase: 'starting', percent: 0, done: false, ok: false, log: '', reboot_required: false };
      startOsPoll();
    } catch (e: any) { error = 'Update failed: ' + (e?.message ?? 'unknown'); }
  }

  async function toggleAuto() {
    autoBusy = true; error = '';
    try {
      const r = await api.autoUpdatesSet(!autoOn);
      if (r.ok) autoOn = !!r.enabled; else error = r.err ?? 'toggle failed';
    } catch (e: any) { error = e?.message ?? 'toggle failed'; }
    autoBusy = false;
  }

  function openUpgrade() {
    showUpgrade = true; upgradeStage = 'backup'; upgradePw = ''; upgradeMsg = '';
  }
  async function upgradeBackup() {
    if (!upgradePw) return;
    upgradeBusy = true; upgradeMsg = '';
    try {
      const res = await fetch('/api/system/config/export', {
        method: 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: upgradePw }),
      });
      if (!res.ok) { upgradeMsg = `✗ backup failed (${res.status})`; upgradeBusy = false; return; }
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url; a.download = 'aeon-config-backup.aeonbackup';
      document.body.appendChild(a); a.click(); a.remove();
      URL.revokeObjectURL(url);
      upgradeMsg = `✓ backup saved (${(blob.size / 1024).toFixed(1)} kB) — keep the file + password safe`;
      upgradeStage = 'done';
    } catch (e: any) { upgradeMsg = `✗ ${e?.message ?? 'backup failed'}`; }
    upgradeBusy = false;
  }
  function goPatreon() {
    const u = img?.patreon_url || 'https://www.patreon.com/AeonForge7';
    window.open(u, '_blank', 'noopener');
    showUpgrade = false;
  }
</script>

<div class="h-full flex flex-col">
  <PageHeader title="Pi system" />

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-3xl mx-auto w-full space-y-6">

      {#if loading}<p class="text-zinc-500 text-sm">loading…</p>{/if}
      {#if error}<p class="text-red-400 text-sm">{error}</p>{/if}
      {#if msg}<p class="text-live-400 text-sm">{msg}</p>{/if}

      <!-- ─── New Orb image available ─── -->
      {#if img?.update_available}
        <section class="rounded-xl border border-amber-500/50 bg-amber-500/10 p-5 space-y-3">
          <div class="flex items-start gap-3">
            <span class="text-2xl leading-none" aria-hidden="true">✨</span>
            <div class="space-y-1">
              <h2 class="font-mono text-sm uppercase tracking-wider text-amber-200">
                New Orb image available — {img.latest_name ?? `v${img.latest}`}
              </h2>
              <p class="text-xs text-amber-100/80 leading-relaxed">
                You're running {img.installed != null ? `v${img.installed}` : 'an unversioned image'}{#if img.published} · published {img.published}{/if}.
                Re-flash for the most complete set of features and the latest security.
                {#if img.notes}<br /><span class="text-amber-100/60">{img.notes}</span>{/if}
              </p>
            </div>
          </div>
          <div class="flex flex-wrap items-center gap-2 pt-1">
            <button class="btn-primary text-sm hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40"
                    on:click={openUpgrade}>
              ↑ Back up &amp; get {img.latest_name ?? `v${img.latest}`}
            </button>
            <span class="text-[11px] text-amber-100/60">Backs up your config first, then opens Patreon to download.</span>
          </div>
        </section>
      {:else if img && img.installed_known === false && img.latest != null}
        <p class="text-[11px] text-zinc-500">
          Couldn't read this device's image version (legacy image). Latest published is
          {img.latest_name ?? `v${img.latest}`} — consider re-flashing for the newest features + security.
        </p>
      {/if}

      <!-- ─── Health ─── -->
      {#if info}
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <header class="space-y-1">
            <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              Health
            </h2>
            {#if info.image_version != null || info.track}
              <p class="text-[11px] font-mono text-zinc-500">
                Image {info.image_version != null ? `v${info.image_version}` : 'unstamped'}{#if info.track} · {info.track}{/if}{#if info.codename} · {info.codename}{/if}
              </p>
            {/if}
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

      <!-- ─── System update (OS packages) ─── -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
        <header class="space-y-1">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            System update
          </h2>
          <p class="text-xs text-zinc-500 leading-relaxed">
            Keep the underlying Raspberry Pi OS patched — <strong>operating-system packages</strong>
            (security + bug fixes). This is separate from re-flashing a whole new Orb <em>image</em>
            (that's the banner up top when one's published).
          </p>
        </header>

        <div class="space-y-3">
          <div class="flex flex-wrap items-center gap-2">
            <button class="btn-primary text-sm" on:click={doOsCheck} disabled={osChecking || osBusy}>
              {osChecking ? 'checking…' : 'Check for updates'}
            </button>
            {#if osCheck}
              {#if osCheck.ok}
                {#if (osCheck.count ?? 0) > 0}
                  <span class="text-sm text-amber-300 font-mono">
                    {osCheck.count} update{(osCheck.count ?? 0) === 1 ? '' : 's'}{#if (osCheck.security ?? 0) > 0} · {osCheck.security} security{/if}
                  </span>
                {:else}
                  <span class="text-sm text-live-300 font-mono">✓ up to date</span>
                {/if}
              {:else}
                <span class="text-sm text-red-300 font-mono">✗ {osCheck.err ?? 'check failed'}</span>
              {/if}
            {/if}
            <button class="btn-primary text-sm hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40"
                    on:click={doOsApply}
                    disabled={osBusy || (osCheck?.ok === true && (osCheck.count ?? 0) === 0)}>
              {osBusy ? 'installing…' : '↓ Install updates'}
            </button>
          </div>

          {#if osCheck?.packages}
            <p class="text-[10.5px] text-zinc-600 font-mono break-words leading-relaxed">
              {osCheck.packages.split(',').join(' · ')}
            </p>
          {/if}

          {#if osStatus && osStatus.phase !== 'idle'}
            <div class="space-y-1.5">
              <div class="flex justify-between text-[11px] font-mono">
                <span class="{osStatus.phase === 'failed' ? 'text-red-300' : osStatus.done ? 'text-live-300' : 'text-amber-300'}">
                  {#if osStatus.done && osStatus.ok}✓ updates installed{:else if osStatus.phase === 'failed'}✗ update failed{:else}{osStatus.phase}…{/if}
                </span>
                <span class="text-zinc-500 tabular-nums">{Math.round(osStatus.percent)}%</span>
              </div>
              <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
                <div class="h-full rounded-full bg-gradient-to-r from-cursed-600 to-cursed-400 transition-all duration-500"
                     style="width:{osStatus.percent}%"></div>
              </div>
              {#if osStatus.log}
                <details class="text-[10.5px]">
                  <summary class="cursor-pointer text-zinc-500 hover:text-zinc-300 font-mono">apt log</summary>
                  <pre class="mt-1 whitespace-pre-wrap break-all font-mono text-[10px] leading-snug text-zinc-500 bg-ink-950 border border-ink-800 rounded p-2 max-h-48 overflow-y-auto">{osStatus.log}</pre>
                </details>
              {/if}
              {#if osStatus.done && osStatus.reboot_required}
                <div class="flex flex-wrap items-center gap-2 pt-1">
                  <span class="text-[11px] text-amber-300">A kernel/firmware update needs a reboot to apply.</span>
                  <button class="btn-primary text-xs hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40" on:click={onPiReboot}>⟳ Reboot now</button>
                </div>
              {/if}
            </div>
          {/if}
        </div>

        <div class="flex items-center justify-between gap-3 pt-3 border-t border-ink-800">
          <div class="space-y-0.5">
            <p class="text-sm text-zinc-300">Automatic security updates</p>
            <p class="text-[11px] text-zinc-500 leading-relaxed">
              Install security patches automatically in the background. Never auto-reboots —
              you reboot on your own schedule.
            </p>
          </div>
          <button type="button" role="switch" aria-checked={autoOn === true}
                  on:click={toggleAuto} disabled={autoBusy || autoOn === null}
                  class="shrink-0 relative inline-flex h-6 w-11 items-center rounded-full transition
                         {autoOn ? 'bg-live-500/70' : 'bg-ink-700'} disabled:opacity-50">
            <span class="inline-block h-4 w-4 transform rounded-full bg-white transition
                         {autoOn ? 'translate-x-6' : 'translate-x-1'}"></span>
          </button>
        </div>
      </section>

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
                The capture chain is roughly:
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
                and shipping H.264 over WebSocket — hardware-encoded on
                the Pi 4 (h264_v4l2m2m), software-encoded on the Pi 5
                (libx264). That path already exists; the levers above
                tune the MJPEG fallback.
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

  <!-- ─── Guided image upgrade: back up → Patreon ─── -->
  {#if showUpgrade}
    <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
         role="presentation" on:click|self={() => (showUpgrade = false)}>
      <div class="w-full max-w-md rounded-xl border border-ink-700 bg-ink-900 p-5 space-y-4 shadow-2xl">
        <header class="space-y-1">
          <h2 class="font-mono text-sm uppercase tracking-wider text-amber-200">
            Get {img?.latest_name ?? 'the new image'}
          </h2>
          <p class="text-xs text-zinc-500 leading-relaxed">
            Re-flashing wipes the SD card. <strong>Back up your config first</strong> — device +
            network settings, identities, keys, macros, and your Model Share library — then download
            the new image from Patreon and restore the backup after flashing.
          </p>
        </header>

        {#if upgradeStage === 'backup'}
          <div class="space-y-2">
            <div class="text-[11px] uppercase tracking-wider text-zinc-500">Backup password</div>
            <input type="password" bind:value={upgradePw} placeholder="choose a strong password"
                   autocomplete="new-password"
                   class="w-full rounded-md border border-ink-800 bg-ink-950/40 px-3 py-2 text-sm font-mono text-zinc-200 placeholder-zinc-600" />
            <p class="text-[11px] text-zinc-600 leading-relaxed">
              You'll need this exact password to restore after flashing — it's the only way to decrypt
              the backup. Store it safely.
            </p>
          </div>
          {#if upgradeMsg}<p class="text-xs font-mono text-red-300">{upgradeMsg}</p>{/if}
          <div class="flex items-center justify-end gap-2 pt-1">
            <button class="text-sm text-zinc-400 hover:text-zinc-200 px-3 py-1.5" on:click={() => (showUpgrade = false)}>Cancel</button>
            <button class="btn-primary text-sm" on:click={upgradeBackup} disabled={!upgradePw || upgradeBusy}>
              {upgradeBusy ? 'backing up…' : '↓ Back up &amp; continue'}
            </button>
          </div>
        {:else}
          <div class="rounded-lg border border-live-600/30 bg-live-500/5 p-3">
            <p class="text-sm text-live-300 font-mono">{upgradeMsg}</p>
          </div>
          <p class="text-xs text-zinc-500 leading-relaxed">
            Now grab {img?.latest_name ?? 'the latest image'} from Patreon, flash it with Raspberry Pi
            Imager, boot the Orb, and restore your backup from the <strong>Configuration backup</strong>
            section on this page.
          </p>
          <div class="flex items-center justify-end gap-2 pt-1">
            <button class="text-sm text-zinc-400 hover:text-zinc-200 px-3 py-1.5" on:click={() => (showUpgrade = false)}>Close</button>
            <button class="btn-primary text-sm hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40" on:click={goPatreon}>
              Open Patreon ↗
            </button>
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>
