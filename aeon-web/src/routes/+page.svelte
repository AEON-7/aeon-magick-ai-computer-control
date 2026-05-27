<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let stream_url = '';
  let state: api.StreamerState | null = null;
  let hid: api.HidStatus | null = null;
  let poll_iv: ReturnType<typeof setInterval>;
  // Network status pills — refreshed every 5s. We only care about the
  // small "is it on?" booleans on this page, not the full config — the
  // /network page is for that.
  let vpnOn = false;
  let vpnProvider: string = 'none';
  let dnscryptOn = false;
  let net_poll_iv: ReturnType<typeof setInterval>;

  let canvas: HTMLDivElement;
  let dragging = false;
  let last_x = 0;
  let last_y = 0;

  // ── Input capture mode ─────────────────────────────────────────────────
  // When ON: pointer is locked to the canvas (mouse stays inside, all
  // movement turns into deltas we send via /api/hid/move), and we eat
  // every keystroke before the browser/OS can react. Release shortcut:
  // Ctrl+Alt+Esc (matches no real OS shortcut, so it's safe).
  let captured = false;
  // Don't render the overlay until after the page mounts — Svelte's
  // hydration needs a deterministic first render.
  let mounted = false;

  async function refreshState() {
    try {
      const sys = await api.getSystemState();
      state = sys.streamer;
      hid = sys.hid;
    } catch (e) {
      console.warn('state refresh failed', e);
    }
  }

  // Refresh VPN + DNSCrypt enabled flags. Polled less often than the
  // streamer state (5s vs 2s) — these change rarely.
  async function refreshNet() {
    try {
      const [v, d] = await Promise.all([api.getVpn(), api.getDnscrypt()]);
      vpnOn = v.enabled;
      vpnProvider = v.provider;
      dnscryptOn = d.enabled;
    } catch (e) {
      // Permission errors (read-only token, etc.) are silent — pills
      // just disappear in that case.
      console.warn('net refresh failed', e);
    }
  }

  onMount(() => {
    stream_url = api.streamURL();
    refreshState();
    refreshNet();
    poll_iv = setInterval(refreshState, 2000);
    net_poll_iv = setInterval(refreshNet, 5000);
    window.addEventListener('keydown', onKey);
    window.addEventListener('keyup', onKey);
    document.addEventListener('pointerlockchange', onPointerLockChange);
    mounted = true;
  });

  onDestroy(() => {
    clearInterval(poll_iv);
    clearInterval(net_poll_iv);
    window.removeEventListener('keydown', onKey);
    window.removeEventListener('keyup', onKey);
    document.removeEventListener('pointerlockchange', onPointerLockChange);
    if (document.pointerLockElement) document.exitPointerLock();
  });

  // The browser fires this when it grants OR loses the pointer lock —
  // including when the user presses Esc (which we WANT to forward; the
  // browser's default Esc-to-release intercepts before our keydown
  // handler can see it). We treat any unexpected unlock as "user wants
  // out" and exit capture mode cleanly.
  function onPointerLockChange() {
    if (!document.pointerLockElement && captured) {
      captured = false;
    }
  }

  function enterCapture() {
    if (captured) return;
    canvas.requestPointerLock();
    captured = true;
  }

  function exitCapture() {
    if (!captured) return;
    if (document.pointerLockElement) document.exitPointerLock();
    captured = false;
    // Release any modifier keys the OS might think we're still holding.
    api.releaseAll().catch(console.warn);
  }

  // ── input capture ─────────────────────────────────────────────────────
  // We capture keystrokes on the document and forward each as a chord op
  // to the HID API. The HID daemon handles press+release atomically.

  function onKey(ev: KeyboardEvent) {
    // Release shortcut — Ctrl+Alt+Esc — works whether captured or not so
    // users can always escape if a key event sneaks through.
    if (ev.type === 'keydown' && ev.ctrlKey && ev.altKey && ev.key === 'Escape') {
      ev.preventDefault();
      exitCapture();
      return;
    }
    // Only forward keystrokes when captured. Without capture we'd
    // hijack ordinary typing in any other input on the page (login form,
    // etc.).
    if (!captured) return;
    if (ev.type === 'keydown' && !ev.repeat) {
      ev.preventDefault();
      const keys: string[] = [];
      if (ev.ctrlKey) keys.push('CTRL');
      if (ev.altKey) keys.push('ALT');
      if (ev.shiftKey) keys.push('SHIFT');
      if (ev.metaKey) keys.push('GUI');
      keys.push(translateKeyName(ev.key));
      api.sendKey(keys, 30).catch(console.warn);
    } else if (ev.type === 'keydown') {
      // Suppress browser default for held keys too (e.g., F-keys,
      // Cmd+Tab attempts). Browser still won't pass Cmd+Tab to us —
      // the OS catches that first — but everything else gets eaten.
      ev.preventDefault();
    }
  }

  function translateKeyName(k: string): string {
    if (k === ' ') return 'SPACE';
    if (k === 'Escape') return 'ESC';
    if (k === 'Backspace') return 'BACKSPACE';
    if (k === 'Enter') return 'ENTER';
    if (k === 'Tab') return 'TAB';
    if (k.startsWith('Arrow')) return k.slice(5).toUpperCase();
    return k;
  }

  function onMouseDown(ev: MouseEvent) {
    canvas.focus();
    // Two click-modes:
    //   - Not captured: click → click. Drag-to-move (legacy behavior).
    //   - Captured: click → click. movementX/Y handles motion.
    if (!captured) {
      dragging = true;
      last_x = ev.clientX;
      last_y = ev.clientY;
    }
    const button = (['left', 'middle', 'right'] as const)[ev.button] ?? 'left';
    api.click(button, ev.detail || 1).catch(console.warn);
  }

  function onMouseMove(ev: MouseEvent) {
    // Captured: pointer is locked. movementX/Y are deltas; coords are
    // meaningless. Forward every move so the remote pointer tracks ours.
    if (captured) {
      const dx = ev.movementX;
      const dy = ev.movementY;
      if (Math.abs(dx) + Math.abs(dy) >= 1) {
        api.moveMouse(Math.trunc(dx), Math.trunc(dy)).catch(console.warn);
      }
      return;
    }
    // Drag-to-move when not captured.
    if (!dragging) return;
    const dx = ev.clientX - last_x;
    const dy = ev.clientY - last_y;
    if (Math.abs(dx) + Math.abs(dy) >= 2) {
      api.moveMouse(Math.trunc(dx), Math.trunc(dy)).catch(console.warn);
      last_x = ev.clientX;
      last_y = ev.clientY;
    }
  }

  function onMouseUp() {
    dragging = false;
  }

  function onWheel(ev: WheelEvent) {
    // Always forward wheel events when interacting with the canvas, so
    // capture-mode and casual-mode both scroll the remote.
    ev.preventDefault();
    const dy = -Math.sign(ev.deltaY) * 3;
    api.scroll(dy).catch(console.warn);
  }

  async function onReleaseAll() {
    await api.releaseAll();
  }
  async function onRelaunch() {
    await api.relaunchStreamer();
    setTimeout(refreshState, 1000);
  }
  async function onLogout() {
    try {
      await api.logout();
    } catch (e) {
      console.warn('logout failed', e);
    }
    window.location.href = '/login';
  }

  // ── Persona switching ──────────────────────────────────────────────────
  // Triggers a USB re-enumeration on the target — ~1s blip. The choice is
  // persisted on the device in /etc/aeon/persona.state and survives reboots.

  let persona_switching = false;
  let persona_message = '';

  async function onPersonaChange(ev: Event) {
    const select = ev.target as HTMLSelectElement;
    const newPersona = select.value;
    if (!newPersona || newPersona === hid?.persona) return;
    if (!confirm(
      `Switch HID persona to "${newPersona}"?\n\n` +
      `The USB device will re-enumerate (~1 second blip on the target). ` +
      `The selection will persist across reboots.`
    )) {
      select.value = hid?.persona ?? '';
      return;
    }
    persona_switching = true;
    persona_message = `switching to ${newPersona}…`;
    try {
      await api.setPersona(newPersona);
      // aeon-hid exits + systemd respawns. Poll until /state comes back
      // with the new persona, then unlock the UI.
      let attempts = 0;
      while (attempts++ < 20) {
        await new Promise(r => setTimeout(r, 500));
        try {
          const sys = await api.getSystemState();
          if (sys.hid?.persona === newPersona) {
            persona_message = `now: ${newPersona}`;
            await refreshState();
            setTimeout(() => (persona_message = ''), 3000);
            return;
          }
        } catch {}
      }
      persona_message = `timeout — refresh and check`;
    } catch (e: any) {
      persona_message = `error: ${e?.message ?? 'unknown'}`;
      select.value = hid?.persona ?? '';
    } finally {
      persona_switching = false;
    }
  }
</script>

<div class="h-full flex flex-col">
  <!-- top bar -->
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <span class="text-cursed-400 font-mono text-sm tracking-widest">AEON MAGICK AI COMPUTER CONTROL</span>
      {#if state}
        <span class={state.online ? 'pill-live' : 'pill-offline'}>
          <span class="h-1.5 w-1.5 rounded-full {state.online ? 'bg-live-400' : 'bg-red-400'}"></span>
          {state.online ? 'LIVE' : 'OFFLINE'}
        </span>
      {/if}
      {#if state?.mode}
        <span class="text-xs font-mono text-zinc-400">
          {state.mode.resolution} · {state.mode.format} · {state.captured_fps} fps
        </span>
      {/if}
      <!-- Network status pills — clickable to the network page. -->
      {#if vpnOn}
        <a href="/network" class="pill-net" title="Click to manage VPN">
          <span class="h-1.5 w-1.5 rounded-full bg-cursed-400 animate-pulse"></span>
          {vpnProvider === 'tor' ? 'TOR' : vpnProvider === 'tailscale' ? 'TAILSCALE'
            : vpnProvider === 'wireguard' ? 'WIREGUARD' : vpnProvider === 'openvpn' ? 'OPENVPN'
            : vpnProvider === 'i2p' ? 'I2P' : 'VPN'}
        </a>
      {/if}
      {#if dnscryptOn}
        <a href="/network" class="pill-net" title="DNSCrypt encrypted DNS — click to configure">
          <span class="h-1.5 w-1.5 rounded-full bg-live-400"></span>
          DNSCrypt
        </a>
      {/if}
      {#if hid}
        <!-- Persona selector — switch the HID descriptor (Generic / Logitech / Apple). -->
        <label class="flex items-center gap-1.5 text-xs font-mono text-cursed-400/80">
          HID:
          <select
            value={hid.persona}
            on:change={onPersonaChange}
            disabled={persona_switching}
            class="bg-ink-800 border border-ink-700 rounded px-1.5 py-0.5
                   text-cursed-300 focus:outline-none focus:ring-1 focus:ring-cursed-500
                   disabled:opacity-50 disabled:cursor-wait"
            title="Switch USB HID persona — triggers a 1-second re-enumeration on the target."
          >
            <option value="generic-composite">generic-composite</option>
            <option value="logitech-mx">logitech-mx</option>
            <option value="apple-magic-stable">apple-magic-stable</option>
            <option value="apple-magic">apple-magic ⚠</option>
          </select>
        </label>
        {#if persona_message}
          <span class="text-xs font-mono text-zinc-500">{persona_message}</span>
        {/if}
      {/if}
    </div>
    <div class="flex items-center gap-2">
      {#if captured}
        <button class="btn-primary text-xs animate-pulse" on:click={exitCapture}
                title="Release input capture (Ctrl+Alt+Esc)">
          ⏏ release&nbsp;capture
        </button>
      {:else}
        <button class="btn text-xs" on:click={enterCapture}
                title="Lock pointer + capture all keys for the remote system">
          ⌨ capture&nbsp;input
        </button>
      {/if}
      <a href="/network" class="btn text-xs">network</a>
      <a href="/security" class="btn text-xs">security</a>
      <a href="/dns" class="btn text-xs">DNS</a>
      <a href="/storage" class="btn text-xs">disk&nbsp;drive</a>
      <a href="/ssh-keys" class="btn text-xs">SSH&nbsp;keys</a>
      <a href="/tokens" class="btn text-xs">API&nbsp;tokens</a>
      <button class="btn" on:click={onReleaseAll}>release&nbsp;all&nbsp;keys</button>
      <button class="btn" on:click={onRelaunch}>relaunch&nbsp;streamer</button>
      <button class="btn text-xs" on:click={onLogout}>sign&nbsp;out</button>
    </div>
  </header>

  <!-- video canvas -->
  <main class="flex-1 relative bg-ink-950">
    <!-- svelte-ignore a11y-no-noninteractive-element-interactions -->
    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div
      bind:this={canvas}
      class="absolute inset-0 outline-none"
      class:cursor-none={captured}
      tabindex="-1"
      on:mousedown={onMouseDown}
      on:mousemove={onMouseMove}
      on:mouseup={onMouseUp}
      on:wheel={onWheel}
      role="application"
    >
      <img
        src={stream_url}
        alt="target screen"
        class="w-full h-full object-contain select-none pointer-events-none"
        draggable="false"
      />
    </div>

    <!-- Capture-mode overlay. Floats at the top of the canvas in the
         pillarbox area so it doesn't cover content. object-contain
         leaves blank space above/below or left/right of the stream;
         this banner uses it. Click anywhere on the banner to release. -->
    {#if mounted && captured}
      <div class="pointer-events-none absolute top-0 left-0 right-0 z-10
                  flex justify-center pt-2">
        <button
          class="pointer-events-auto px-4 py-1.5 rounded-full
                 bg-red-900/70 border border-red-500/60 backdrop-blur-sm
                 text-red-200 font-mono text-xs tracking-wider
                 shadow-lg hover:bg-red-800/80"
          on:click={exitCapture}
          title="Release capture"
        >
          ● INPUT CAPTURED &nbsp;·&nbsp; Ctrl+Alt+Esc to release
        </button>
      </div>
    {/if}
  </main>

  <!-- bottom: console/log preview area, hidden by default; future -->
  <footer class="px-5 py-2 border-t border-ink-700 bg-ink-900 text-xs font-mono text-zinc-500">
    relaunches: {state?.relaunch_count ?? 0}
  </footer>
</div>
