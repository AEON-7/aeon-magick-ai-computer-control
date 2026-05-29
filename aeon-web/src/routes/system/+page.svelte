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
