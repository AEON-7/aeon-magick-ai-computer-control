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

  // ── Mobile / fullscreen state ──
  // We track fullscreen separately from `captured` because on mobile the
  // pointer-lock pattern doesn't apply (no mouse to lock) — we run a
  // touch-mapping mode instead.
  let fullscreen = false;
  let isTouchDevice = false;
  let kbdInput: HTMLInputElement | undefined;
  // Touch-tracking state — populated in onTouch*. Times are ms epoch.
  let touchStartTs = 0;
  let touchStartX = 0;
  let touchStartY = 0;
  let touchLastX = 0;
  let touchLastY = 0;
  let touchMoved = false;
  let touchCount = 0;
  let twoFingerStartY = 0;
  let twoFingerLastY = 0;
  let longPressTimer: ReturnType<typeof setTimeout> | null = null;
  // When the user taps the on-screen keyboard button, we focus a hidden
  // input that triggers iOS/Android's soft keyboard. `kbdVisible` is the
  // hint to the UI to show the close-keyboard button instead.
  let kbdVisible = false;
  // Hamburger menu open state — only relevant on small screens.
  let menuOpen = false;
  function closeMenu() { menuOpen = false; }

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
    document.addEventListener('fullscreenchange', onFullscreenChange);
    document.addEventListener('webkitfullscreenchange', onFullscreenChange);
    // Touch detection — used to gate touch handlers + auto-show the
    // mobile-friendly UX hints. `ontouchstart` is the most reliable
    // single-signal feature check for "this device has touch".
    isTouchDevice = 'ontouchstart' in window
      || (navigator.maxTouchPoints ?? 0) > 0;
    mounted = true;
  });

  onDestroy(() => {
    clearInterval(poll_iv);
    clearInterval(net_poll_iv);
    window.removeEventListener('keydown', onKey);
    window.removeEventListener('keyup', onKey);
    document.removeEventListener('pointerlockchange', onPointerLockChange);
    document.removeEventListener('fullscreenchange', onFullscreenChange);
    document.removeEventListener('webkitfullscreenchange', onFullscreenChange);
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
    // Click fires immediately for both modes. last_x/y was already set on
    // mouseenter (casual) or doesn't matter (captured uses movementX/Y).
    if (!captured) {
      dragging = true;
      last_x = ev.clientX;
      last_y = ev.clientY;
    }
    const button = (['left', 'middle', 'right'] as const)[ev.button] ?? 'left';
    api.click(button, ev.detail || 1).catch(console.warn);
  }

  // Set the baseline coordinate when the cursor enters the canvas. Without
  // this, the first delta after entering would be huge (from 0,0 or
  // whatever the previous in-canvas position was).
  function onMouseEnter(ev: MouseEvent) {
    if (!captured) {
      last_x = ev.clientX;
      last_y = ev.clientY;
    }
  }

  function onMouseLeave() {
    // No active drag once the cursor leaves — and no implicit hover-
    // tracking either, so moving over the header/sidebar doesn't push
    // bogus deltas at the target.
    dragging = false;
  }

  function onMouseMove(ev: MouseEvent) {
    // Captured: pointer is locked. movementX/Y are deltas; absolute
    // coords are meaningless. Forward every move so the remote pointer
    // tracks ours.
    if (captured) {
      const dx = ev.movementX;
      const dy = ev.movementY;
      if (Math.abs(dx) + Math.abs(dy) >= 1) {
        api.moveMouse(Math.trunc(dx), Math.trunc(dy)).catch(console.warn);
      }
      return;
    }
    // Casual mode: hover-track. As the cursor moves over the canvas,
    // send the relative delta so the target cursor follows 1:1. Without
    // this, the target cursor only updates while you're dragging — so
    // wheel-scrolling "in the middle" worked (because that's where the
    // last click left the target cursor) but "in the corner" felt broken.
    // Throttle at ≥2px so micro-jitter doesn't flood the HID API.
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
    // Forward wheel events from anywhere on the canvas. The target's
    // cursor has already been synced to our position by onMouseMove
    // (in casual mode) or pointer-lock movementX/Y (in captured mode),
    // so scrolling scrolls under wherever you're hovering — no need to
    // click-into-center first.
    ev.preventDefault();
    const dy = -Math.sign(ev.deltaY) * 3;
    api.scroll(dy).catch(console.warn);
  }

  // ── Touch input mapping (iPhone / iPad / Android) ─────────────────────
  // Gestures map to HID ops:
  //   single short tap  → left click
  //   single long press → right click (>= 500 ms, minimal motion)
  //   two-finger tap    → right click
  //   single drag       → mouse move (relative delta)
  //   two-finger drag   → scroll
  //   double tap        → double-click (browser-native dblclick on tap)
  //
  // We deliberately don't implement pinch — the remote target has no
  // concept of pinch via boot-mouse HID.

  const LONG_PRESS_MS = 500;
  const TAP_MOVE_THRESHOLD = 8;       // px before a tap is reclassified as a drag
  const TOUCH_MOVE_THROTTLE = 4;      // px before we send a /move event

  function onTouchStart(ev: TouchEvent) {
    ev.preventDefault();
    touchCount = ev.touches.length;
    touchStartTs = Date.now();
    touchMoved = false;
    if (touchCount === 1) {
      const t = ev.touches[0];
      touchStartX = touchLastX = t.clientX;
      touchStartY = touchLastY = t.clientY;
      // Long-press → right click. Schedule, cancelled by move or end.
      longPressTimer = setTimeout(() => {
        if (!touchMoved) {
          api.click('right').catch(console.warn);
          // Subsequent touchend should NOT also fire a left-click — flip
          // touchMoved so onTouchEnd treats this as already-consumed.
          touchMoved = true;
        }
      }, LONG_PRESS_MS);
    } else if (touchCount === 2) {
      // Cancel any pending long-press from the first finger.
      cancelLongPress();
      const mid = (ev.touches[0].clientY + ev.touches[1].clientY) / 2;
      twoFingerStartY = twoFingerLastY = mid;
    }
  }

  function onTouchMove(ev: TouchEvent) {
    ev.preventDefault();
    if (ev.touches.length === 1 && touchCount === 1) {
      const t = ev.touches[0];
      const dx = t.clientX - touchLastX;
      const dy = t.clientY - touchLastY;
      const totalDx = t.clientX - touchStartX;
      const totalDy = t.clientY - touchStartY;
      if (!touchMoved &&
          Math.abs(totalDx) + Math.abs(totalDy) > TAP_MOVE_THRESHOLD) {
        touchMoved = true;
        cancelLongPress();
      }
      if (touchMoved && (Math.abs(dx) + Math.abs(dy) >= TOUCH_MOVE_THROTTLE)) {
        api.moveMouse(Math.trunc(dx), Math.trunc(dy)).catch(console.warn);
        touchLastX = t.clientX;
        touchLastY = t.clientY;
      }
    } else if (ev.touches.length === 2) {
      const mid = (ev.touches[0].clientY + ev.touches[1].clientY) / 2;
      const dy = mid - twoFingerLastY;
      if (Math.abs(dy) >= 6) {
        // Negative deltaY in our wheel handler scrolls UP. Here we make
        // dragging two fingers DOWN scroll DOWN (natural scrolling).
        api.scroll(-Math.sign(dy) * 2).catch(console.warn);
        twoFingerLastY = mid;
      }
      touchMoved = true;
      cancelLongPress();
    }
  }

  function onTouchEnd(ev: TouchEvent) {
    ev.preventDefault();
    const duration = Date.now() - touchStartTs;
    cancelLongPress();
    if (touchCount === 1 && !touchMoved && duration < LONG_PRESS_MS) {
      api.click('left').catch(console.warn);
    } else if (touchCount === 2 && !touchMoved && duration < LONG_PRESS_MS) {
      api.click('right').catch(console.warn);
    }
    // Reset state regardless.
    touchCount = ev.touches.length;
    touchMoved = false;
    // Bring keyboard back if it was open. iOS Safari blurs the hidden
    // input the moment the user taps anywhere else; without this,
    // tapping a text field on the remote screen would close the soft
    // keyboard mid-typing-session.
    maybeReclaimKeyboard();
  }

  function cancelLongPress() {
    if (longPressTimer) {
      clearTimeout(longPressTimer);
      longPressTimer = null;
    }
  }

  // ── Fullscreen (mobile + desktop) ─────────────────────────────────────
  //
  // Two paths, transparently:
  //
  //   1. Real browser fullscreen (`requestFullscreen()`) — Android Chrome,
  //      desktop, iPad Safari. Hides the browser chrome too.
  //
  //   2. CSS pseudo-fullscreen — used as a fallback on iPhone Safari
  //      where the standards API throws or is unavailable. We can't
  //      truly hide Safari's bottom toolbar, but we can fix:inset:0
  //      the canvas to fill the dynamic viewport and ditch every
  //      other layout element so it's *as close* to fullscreen as
  //      iOS allows. Combined with the user dragging Safari to
  //      "hide toolbar" mode, it's a real fullscreen experience.

  let pseudoFullscreen = false;       // True when we fell back to CSS

  async function enterFullscreen() {
    if (!canvas) return;
    const el = document.documentElement as any;
    let realWorked = false;
    if (el.requestFullscreen) {
      try {
        await el.requestFullscreen({ navigationUI: 'hide' as any });
        realWorked = true;
      } catch (e) {
        console.warn('requestFullscreen failed, falling back to CSS', e);
      }
    } else if (el.webkitRequestFullscreen) {
      try {
        el.webkitRequestFullscreen();
        realWorked = true;
      } catch (e) {
        console.warn('webkitRequestFullscreen failed, falling back to CSS', e);
      }
    }
    if (!realWorked) {
      // Pseudo-fullscreen: relies on CSS in the markup below
      // (#aeon-pseudo-fs styles). Setting `pseudoFullscreen` triggers
      // it via class binding.
      pseudoFullscreen = true;
    }
    // Best-effort landscape lock. iOS Safari ignores; Android honors.
    try {
      if ((screen as any).orientation?.lock) {
        await (screen as any).orientation.lock('landscape').catch(() => {});
      }
    } catch { /* fine */ }
    fullscreen = true;
  }

  async function exitFullscreen() {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else if ((document as any).webkitFullscreenElement) {
        (document as any).webkitExitFullscreen();
      }
    } catch (e) { console.warn('exit fullscreen failed', e); }
    hideKeyboard();
    pseudoFullscreen = false;
    fullscreen = false;
  }

  function onFullscreenChange() {
    const real = !!(document.fullscreenElement || (document as any).webkitFullscreenElement);
    // pseudoFullscreen is separate — only flips when our exitFullscreen
    // runs. The standards API event fires only for the real path.
    fullscreen = real || pseudoFullscreen;
    if (!real && !pseudoFullscreen) hideKeyboard();
  }

  // ── On-screen keyboard bridge (iOS / Android) ─────────────────────────
  // OS keyboards on mobile only appear when a text input is focused. We
  // park a hidden input on the page; tapping the "keyboard" overlay
  // button focuses it, which triggers the soft keyboard. Each typed
  // character fires an `input` event with the inserted text — we forward
  // it to /api/hid/type then clear the input so the next char arrives
  // cleanly. Special keys (Enter, Backspace, Tab, arrows, Esc) still
  // fire keydown on most mobile keyboards and go through the normal
  // onKey path when captured.

  // "kbdVisible" is the USER'S INTENT — they tapped the keyboard
  // button, they want it open. iOS will steal focus the moment they
  // tap somewhere else (like the canvas to click on the target), so
  // we re-focus the hidden input on every touch in fullscreen mode
  // until the user explicitly closes the keyboard with the ⌨ ⏷ hide
  // button. That way "tap to click on remote" doesn't accidentally
  // dismiss the keyboard.
  function showKeyboard() {
    if (!kbdInput) return;
    kbdInput.value = '';
    kbdInput.focus();
    // iOS Safari sometimes needs a second hit before the keyboard pops.
    setTimeout(() => kbdInput?.focus(), 50);
    kbdVisible = true;
    // Enable capture so keyboard events get forwarded.
    captured = true;
  }
  function hideKeyboard() {
    if (kbdInput) {
      kbdInput.blur();
      kbdInput.value = '';
    }
    kbdVisible = false;
  }
  /// Called whenever the user touches the canvas while in fullscreen
  /// mode with keyboard intended open. Refocuses the hidden input on
  /// the *next* tick so the canvas's pointer/touch handler runs first
  /// (registering the click on the remote), then keyboard comes back.
  function maybeReclaimKeyboard() {
    if (kbdVisible && kbdInput && document.activeElement !== kbdInput) {
      setTimeout(() => kbdInput?.focus(), 0);
    }
  }
  function onKbdInput(ev: Event) {
    const target = ev.target as HTMLInputElement;
    const text = target.value;
    if (text) {
      api.typeText(text).catch(console.warn);
      // Clear so the next character arrives as a fresh `input` event.
      target.value = '';
    }
  }
  function onKbdKeydown(ev: KeyboardEvent) {
    // Special keys → forward via /api/hid/key. We DON'T do this for
    // printable characters because the `input` event already handled
    // them via typeText.
    const k = ev.key;
    if (k === 'Enter' || k === 'Backspace' || k === 'Tab' || k === 'Escape'
        || k === 'ArrowUp' || k === 'ArrowDown' || k === 'ArrowLeft' || k === 'ArrowRight') {
      ev.preventDefault();
      api.sendKey([translateKeyName(k)], 30).catch(console.warn);
    }
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

  /// Double-confirm reboot. First prompt is "are you sure"; the second
  /// pops a one-shot "type REBOOT to confirm" so a stray click can't
  /// accidentally kill a session mid-AI-job. Same for poweroff but
  /// with stronger language since wake requires a power cycle.
  async function onReboot() {
    if (!confirm(
      'Reboot the Pi?\n\n' +
      'The web UI will disconnect for ~30-60 seconds while the device ' +
      'comes back up. Any in-flight HID input + capture will be lost.'
    )) return;
    const phrase = prompt('Type REBOOT to confirm:');
    if (phrase !== 'REBOOT') return;
    try {
      const r = await api.rebootSystem();
      alert(r.message);
    } catch (e: any) {
      alert('Reboot failed: ' + (e?.message ?? 'unknown'));
    }
  }
  async function onPoweroff() {
    if (!confirm(
      'Power off the Pi?\n\n' +
      'You will need to physically power-cycle the device to bring it ' +
      'back. The web UI cannot turn it back on.'
    )) return;
    const phrase = prompt('Type POWEROFF to confirm:');
    if (phrase !== 'POWEROFF') return;
    try {
      const r = await api.poweroffSystem();
      alert(r.message);
    } catch (e: any) {
      alert('Poweroff failed: ' + (e?.message ?? 'unknown'));
    }
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

<div class="h-full flex flex-col"
     class:aeon-pseudo-fs={pseudoFullscreen}>
  <!-- Top bar. Layout strategy:
       • lg+ (≥1024px): TWO rows. Row 1 = status (brand, LIVE, mode,
                       network pills, persona). Row 2 = action buttons
                       grouped into three clusters — Input | Navigate
                       | System — with subtle vertical dividers so the
                       row reads as three sections instead of a wall.
       • <lg: brand + capture + fullscreen + hamburger; everything
              else collapses into the menuOpen dropdown.
       Keeping all buttons visible at full-Mac sizes was the explicit
       ask — the dividers + 2-row layout makes the cluster cohabit
       with the status line without overlapping. -->
  <header class="border-b border-ink-700 bg-ink-900"
          class:hidden={fullscreen}>
    <!-- Row 1: brand + status + persona -->
    <div class="flex items-center justify-between gap-2 px-3 sm:px-5 pt-3 pb-2">
      <div class="flex items-center gap-2 sm:gap-3 min-w-0 flex-wrap">
        <span class="text-cursed-400 font-mono text-xs sm:text-sm tracking-widest truncate">
          <span class="hidden sm:inline">AEON MAGICK AI COMPUTER CONTROL</span>
          <span class="sm:hidden">AEON MAGICK</span>
        </span>
        {#if state}
          <span class={state.online ? 'pill-live' : 'pill-offline'}>
            <span class="h-1.5 w-1.5 rounded-full {state.online ? 'bg-live-400' : 'bg-red-400'}"></span>
            {state.online ? 'LIVE' : 'OFFLINE'}
          </span>
        {/if}
        {#if state?.mode}
          <span class="hidden md:inline text-xs font-mono text-zinc-400">
            {state.mode.resolution} · {state.mode.format} · {state.captured_fps} fps
          </span>
        {/if}
        {#if vpnOn}
          <a href="/network" class="pill-net hidden sm:inline-flex" title="Click to manage VPN">
            <span class="h-1.5 w-1.5 rounded-full bg-cursed-400 animate-pulse"></span>
            {vpnProvider === 'tor' ? 'TOR' : vpnProvider === 'tailscale' ? 'TAILSCALE'
              : vpnProvider === 'wireguard' ? 'WIREGUARD' : vpnProvider === 'openvpn' ? 'OPENVPN'
              : vpnProvider === 'i2p' ? 'I2P' : 'VPN'}
          </a>
        {/if}
        {#if dnscryptOn}
          <a href="/network" class="pill-net hidden sm:inline-flex" title="DNSCrypt encrypted DNS — click to configure">
            <span class="h-1.5 w-1.5 rounded-full bg-live-400"></span>
            DNSCrypt
          </a>
        {/if}
      </div>
      <!-- Right side of row 1: HID persona + mobile hamburger. -->
      <div class="flex items-center gap-2 shrink-0">
        {#if hid}
          <label class="hidden lg:flex items-center gap-1.5 text-xs font-mono text-cursed-400/80">
            HID:
            <select
              value={hid.persona}
              on:change={onPersonaChange}
              disabled={persona_switching}
              class="bg-ink-800 border border-ink-700 rounded px-1.5 py-0.5
                     text-cursed-300 focus:outline-none focus:ring-1 focus:ring-cursed-500
                     disabled:opacity-50 disabled:cursor-wait"
              title="Switch USB HID persona — triggers a 1-second re-enumeration on the target.">
              <option value="generic-composite">generic-composite</option>
              <option value="logitech-mx">logitech-mx</option>
              <option value="apple-magic-stable">apple-magic-stable</option>
              <option value="apple-magic">apple-magic ⚠</option>
            </select>
          </label>
          {#if persona_message}
            <span class="hidden lg:inline text-xs font-mono text-zinc-500">{persona_message}</span>
          {/if}
        {/if}
        <!-- Hamburger: shown below lg, opens the mobile dropdown. -->
        <button class="btn text-xs lg:hidden"
                on:click={() => (menuOpen = !menuOpen)}
                aria-label="Open menu"
                aria-expanded={menuOpen}>
          {menuOpen ? '✕' : '☰'}
        </button>
      </div>
    </div>

    <!-- Row 2: action buttons, lg+ only. Three groups with vertical
         dividers between them so the eye reads them as logical
         clusters rather than a wall of buttons.
         Group A — Input control (capture, fullscreen)
         Group B — Navigate (network, security, DNS, storage, SSH, tokens, audit)
         Group C — System (release keys, relaunch streamer, sign out)
    -->
    <div class="hidden lg:flex items-center flex-wrap gap-2 px-5 pb-3">
      <!-- Group A -->
      <div class="flex items-center gap-2 pr-3">
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
        <button class="btn text-xs" on:click={enterFullscreen}
                title="Fullscreen control mode — best on phones / tablets">
          ⛶ fullscreen
        </button>
      </div>
      <!-- divider -->
      <span class="h-6 w-px bg-ink-700 mx-1" aria-hidden="true"></span>
      <!-- Group B: nav -->
      <div class="flex items-center gap-2 px-3">
        <a href="/network"   class="btn text-xs">network</a>
        <a href="/security"  class="btn text-xs">security</a>
        <a href="/dns"       class="btn text-xs">DNS</a>
        <a href="/storage"   class="btn text-xs">disk&nbsp;drive</a>
        <a href="/ssh-keys"  class="btn text-xs">SSH&nbsp;keys</a>
        <a href="/tokens"    class="btn text-xs">API&nbsp;tokens</a>
        <a href="/audit"     class="btn text-xs">audit&nbsp;log</a>
      </div>
      <!-- divider -->
      <span class="h-6 w-px bg-ink-700 mx-1" aria-hidden="true"></span>
      <!-- Group C: system -->
      <div class="flex items-center gap-2 pl-3">
        <button class="btn text-xs" on:click={onReleaseAll}>release&nbsp;all&nbsp;keys</button>
        <button class="btn text-xs" on:click={onRelaunch}>relaunch&nbsp;streamer</button>
        <button class="btn text-xs hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40"
                on:click={onReboot}
                title="Reboot the Pi (requires double-confirm; web UI disconnects for ~30-60 s)">
          ⟳ reboot
        </button>
        <button class="btn text-xs hover:bg-red-500/20 hover:text-red-300 hover:border-red-500/40"
                on:click={onPoweroff}
                title="Power off the Pi (requires double-confirm; needs physical power-cycle to wake)">
          ⏻ power&nbsp;off
        </button>
        <button class="btn text-xs" on:click={onLogout}>sign&nbsp;out</button>
      </div>
    </div>

    <!-- On mobile, capture + fullscreen are the only inline action
         buttons (in addition to the hamburger on the right of row 1).
         Stash them in row 2 here for sm+ but pre-lg. At <sm even the
         capture/fullscreen labels collapse to icons. -->
    <div class="lg:hidden flex items-center gap-2 px-3 pb-3">
      {#if captured}
        <button class="btn-primary text-xs animate-pulse" on:click={exitCapture}
                title="Release input capture (Ctrl+Alt+Esc)">
          ⏏ <span class="hidden sm:inline">release&nbsp;capture</span>
        </button>
      {:else}
        <button class="btn text-xs" on:click={enterCapture}
                title="Lock pointer + capture all keys for the remote system">
          ⌨ <span class="hidden sm:inline">capture&nbsp;input</span>
        </button>
      {/if}
      <button class="btn text-xs" on:click={enterFullscreen}
              title="Fullscreen control mode — best on phones / tablets">
        ⛶ <span class="hidden sm:inline">fullscreen</span>
      </button>
    </div>
  </header>

  <!-- Mobile dropdown menu (lg:hidden). Closes when any item is tapped. -->
  {#if menuOpen}
    <!-- svelte-ignore a11y-click-events-have-key-events -->
    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div class="lg:hidden border-b border-ink-700 bg-ink-900/95 backdrop-blur-sm
                px-3 py-3 space-y-3 z-30"
         on:click={closeMenu}
         role="menu"
         tabindex="-1">
      {#if hid}
        <!-- HID persona dropdown — full-width in the menu. -->
        <label class="flex items-center justify-between gap-2 text-xs font-mono text-cursed-400/80">
          <span>HID persona:</span>
          <select value={hid.persona} on:change={onPersonaChange}
                  disabled={persona_switching}
                  class="bg-ink-800 border border-ink-700 rounded px-1.5 py-0.5
                         text-cursed-300 flex-1 disabled:opacity-50">
            <option value="generic-composite">generic-composite</option>
            <option value="logitech-mx">logitech-mx</option>
            <option value="apple-magic-stable">apple-magic-stable</option>
            <option value="apple-magic">apple-magic ⚠</option>
          </select>
        </label>
      {/if}
      <!-- Status mini-grid (mode + pills) for phone view. -->
      <div class="flex flex-wrap items-center gap-2 text-xs font-mono text-zinc-400">
        {#if state?.mode}
          <span>{state.mode.resolution} · {state.captured_fps} fps</span>
        {/if}
        {#if vpnOn}
          <a href="/network" class="pill-net">
            <span class="h-1.5 w-1.5 rounded-full bg-cursed-400 animate-pulse"></span>
            {vpnProvider === 'tor' ? 'TOR' : vpnProvider === 'tailscale' ? 'TAILSCALE' : 'VPN'}
          </a>
        {/if}
        {#if dnscryptOn}
          <a href="/network" class="pill-net">
            <span class="h-1.5 w-1.5 rounded-full bg-live-400"></span>
            DNSCrypt
          </a>
        {/if}
      </div>
      <!-- Nav links — two columns for thumb reach. -->
      <div class="grid grid-cols-2 gap-2 pt-1 border-t border-ink-800">
        <a href="/network"  class="btn text-xs">network</a>
        <a href="/security" class="btn text-xs">security</a>
        <a href="/dns"      class="btn text-xs">DNS</a>
        <a href="/storage"  class="btn text-xs">disk drive</a>
        <a href="/ssh-keys" class="btn text-xs">SSH keys</a>
        <a href="/tokens"   class="btn text-xs">API tokens</a>
        <a href="/audit"    class="btn text-xs col-span-2">audit log</a>
      </div>
      <div class="grid grid-cols-2 gap-2 pt-1 border-t border-ink-800">
        <button class="btn text-xs" on:click={onReleaseAll}>release keys</button>
        <button class="btn text-xs" on:click={onRelaunch}>relaunch streamer</button>
        <button class="btn text-xs hover:bg-amber-500/20 hover:text-amber-300 hover:border-amber-500/40"
                on:click={onReboot}>⟳ reboot</button>
        <button class="btn text-xs hover:bg-red-500/20 hover:text-red-300 hover:border-red-500/40"
                on:click={onPoweroff}>⏻ power off</button>
        <button class="btn text-xs col-span-2" on:click={onLogout}>sign out</button>
      </div>
    </div>
  {/if}

  <!-- video canvas -->
  <main class="flex-1 relative bg-ink-950">
    <!-- svelte-ignore a11y-no-noninteractive-element-interactions -->
    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div
      bind:this={canvas}
      class="absolute inset-0 outline-none touch-none"
      class:cursor-none={captured}
      tabindex="-1"
      on:mousedown={onMouseDown}
      on:mousemove={onMouseMove}
      on:mouseup={onMouseUp}
      on:mouseenter={onMouseEnter}
      on:mouseleave={onMouseLeave}
      on:wheel={onWheel}
      on:touchstart={onTouchStart}
      on:touchmove={onTouchMove}
      on:touchend={onTouchEnd}
      on:touchcancel={onTouchEnd}
      role="application"
    >
      <img
        src={stream_url}
        alt="target screen"
        class="w-full h-full object-contain select-none pointer-events-none"
        draggable="false"
      />
    </div>

    <!-- Hidden text input parked at top-left as a 1px transparent target.
         Focused by the floating "show keyboard" button so iOS / Android
         pop their soft keyboards. Each typed char fires an `input` event
         we forward to /api/hid/type.
         autocomplete/spellcheck/autocapitalize OFF so the OS doesn't eat
         single characters or auto-capitalize sentences.
         iOS Safari refuses to focus truly-off-screen inputs, so we keep
         this 1px+opacity-0 trick (visible to the browser, invisible to
         the user) instead of -top-96 hiding. -->
    <input
      bind:this={kbdInput}
      type="text"
      autocomplete="off"
      autocorrect="off"
      autocapitalize="off"
      spellcheck="false"
      inputmode="text"
      aria-label="Remote keyboard input"
      on:input={onKbdInput}
      on:keydown={onKbdKeydown}
      style="position: fixed; top: 0; left: 0; width: 1px; height: 1px;
             opacity: 0.001; z-index: -1; border: 0; padding: 0; margin: 0;
             font-size: 16px;"
    />

    <!-- Capture-mode banner — only shown when NOT in fullscreen, since
         fullscreen has its own overlay buttons. -->
    {#if mounted && captured && !fullscreen}
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

    <!-- ───────────────────────────────────────────────────────────────── -->
    <!-- Fullscreen mobile overlay — floating semi-transparent controls    -->
    <!-- pinned to corners so they don't obstruct the stream center.       -->
    <!-- Tap the ⏏ to escape out completely, ⌨ to pop the soft keyboard,   -->
    <!-- ⏎ for Enter, ⌫ for Backspace, ⎋ for Esc.                          -->
    <!-- ───────────────────────────────────────────────────────────────── -->
    {#if fullscreen}
      <!-- Top-right: escape (the always-visible "I want out" button).
           Bigger touch target than desktop buttons; safe-area-aware so
           it stays inside the notch on iPhones. -->
      <div class="absolute top-0 right-0 z-20 p-3"
           style="padding-top: max(0.75rem, env(safe-area-inset-top));">
        <button
          class="px-4 py-2.5 rounded-full
                 bg-red-900/80 border border-red-500/60 backdrop-blur-md
                 text-red-100 font-mono text-sm tracking-wider
                 shadow-lg active:scale-95 transition-transform"
          on:click={exitFullscreen}
          aria-label="Exit fullscreen + release capture"
        >
          ⏏ escape
        </button>
      </div>

      <!-- Top-left: persona pill + name. Tap nothing — just a status hint
           so you remember which keyboard the target is seeing. -->
      {#if hid}
        <div class="absolute top-0 left-0 z-20 p-3 pointer-events-none"
             style="padding-top: max(0.75rem, env(safe-area-inset-top));">
          <span class="px-3 py-1.5 rounded-full
                       bg-ink-900/70 border border-ink-700 backdrop-blur-md
                       text-zinc-300 font-mono text-xs tracking-wider">
            {hid.persona}
          </span>
        </div>
      {/if}

      <!-- Bottom-right: keyboard toggle + special-key shortcuts.
           Stacked column so each button is finger-sized. -->
      <div class="absolute bottom-0 right-0 z-20 p-3 flex flex-col gap-2 items-end"
           style="padding-bottom: max(0.75rem, env(safe-area-inset-bottom));">
        {#if kbdVisible}
          <button class="px-4 py-2.5 rounded-full
                         bg-cursed-900/80 border border-cursed-500/60 backdrop-blur-md
                         text-cursed-100 font-mono text-sm
                         shadow-lg active:scale-95 transition-transform"
                  on:click={hideKeyboard}
                  aria-label="Hide on-screen keyboard">
            ⌨ ⏷ hide
          </button>
        {:else}
          <button class="px-4 py-2.5 rounded-full
                         bg-ink-900/80 border border-cursed-500/40 backdrop-blur-md
                         text-cursed-200 font-mono text-sm
                         shadow-lg active:scale-95 transition-transform"
                  on:click={showKeyboard}
                  aria-label="Show on-screen keyboard">
            ⌨ keyboard
          </button>
        {/if}
        <!-- Special-keys row. Saves a keyboard-toggle round-trip for
             the most common non-text keys. Only shown when keyboard is up. -->
        {#if kbdVisible}
          <div class="flex gap-1.5">
            <button class="w-12 h-12 rounded-full bg-ink-900/80 border border-ink-700
                           backdrop-blur-md text-zinc-300 active:scale-95"
                    on:click={() => api.sendKey(['ESC'])} aria-label="Send Esc">⎋</button>
            <button class="w-12 h-12 rounded-full bg-ink-900/80 border border-ink-700
                           backdrop-blur-md text-zinc-300 active:scale-95"
                    on:click={() => api.sendKey(['TAB'])} aria-label="Send Tab">⇥</button>
            <button class="w-12 h-12 rounded-full bg-ink-900/80 border border-ink-700
                           backdrop-blur-md text-zinc-300 active:scale-95"
                    on:click={() => api.sendKey(['BACKSPACE'])} aria-label="Send Backspace">⌫</button>
            <button class="w-12 h-12 rounded-full bg-ink-900/80 border border-ink-700
                           backdrop-blur-md text-zinc-300 active:scale-95"
                    on:click={() => api.sendKey(['ENTER'])} aria-label="Send Enter">⏎</button>
          </div>
        {/if}
      </div>

      <!-- Bottom-left: release-all-keys panic button + a help hint on
           first entry. The hint disappears after a tap or 6s. -->
      <div class="absolute bottom-0 left-0 z-20 p-3"
           style="padding-bottom: max(0.75rem, env(safe-area-inset-bottom));">
        <button class="px-4 py-2.5 rounded-full
                       bg-amber-900/70 border border-amber-500/40 backdrop-blur-md
                       text-amber-100 font-mono text-xs
                       shadow-lg active:scale-95 transition-transform"
                on:click={onReleaseAll}
                aria-label="Release all held keys + buttons">
          ⌨ release&nbsp;keys
        </button>
      </div>

      <!-- One-time gesture hint on first entry (fades after 6s) -->
      {#if isTouchDevice}
        <div class="absolute inset-x-0 top-1/2 -translate-y-1/2 z-10
                    flex justify-center pointer-events-none">
          <div class="px-4 py-2 rounded-lg
                      bg-ink-900/40 border border-cursed-500/30 backdrop-blur-sm
                      text-zinc-400 font-mono text-[11px] text-center
                      max-w-[260px] animate-pulse"
               style="animation-iteration-count: 3; animation-duration: 2s;">
            tap = click · long-press = right · two-finger tap = right · drag = move · two-finger drag = scroll
          </div>
        </div>
      {/if}
    {/if}
  </main>

  <!-- bottom: console/log preview area, hidden by default; future -->
  <footer class="px-5 py-2 border-t border-ink-700 bg-ink-900 text-xs font-mono text-zinc-500">
    relaunches: {state?.relaunch_count ?? 0}
  </footer>
</div>
