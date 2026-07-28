<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { fly } from 'svelte/transition';
  import * as api from '$lib/api';
  import H264Canvas from '$lib/components/H264Canvas.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import OrbMark from '$lib/components/OrbMark.svelte';
  import TargetPowerMenu from '$lib/components/TargetPowerMenu.svelte';
  import SpecialKeys from '$lib/components/SpecialKeys.svelte';
  import { toast } from '$lib/toast';
  import { confirmRite } from '$lib/confirm';

  // Menus "condense" in over 120ms instead of teleporting. in: only —
  // an out-transition would leave a 120ms ghost panel that could
  // swallow a click near the trigger. 0ms for reduced-motion users.
  const reduceMotion =
    typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  const menuIn = { y: -4, duration: reduceMotion ? 0 : 120 };

  // v99: launcher IA v2 — three "super apps" (OrbNet, Agent Dash, GPIO) stand
  // alone as color-coded buttons; everything else collapses into two dropdowns:
  // Settings (configuration) and Monitor (logs + read-only monitoring). KVM
  // controls stay inline. The registry in $lib/nav.ts drives the desktop
  // toolbar, the mobile menu, AND the Cmd+K palette.
  import { SUPER_APPS, SETTINGS_ITEMS, MONITOR_ITEMS } from '$lib/nav';
  import { inputCaptured } from '$lib/capture';
  // Static class strings (Tailwind scans source text — keep them literal).
  //
  // The super-app row is a COLONNADE: uniform columns, identical treatment, the
  // icon carries identity. It used to map seven accent colours (cursed/sky/amber/
  // emerald/flame/violet/rose) onto eight buttons, which made the flagship screen
  // read as a toy launcher and diluted the one violet sigil that's supposed to
  // mean "this is the action". One accent, locked — colour now means STATE
  // (live / warn / fault), never identity.
  const APP_BTN =
    'app-module border-steel-700 bg-ink-900/70 text-zinc-300 ' +
    'hover:border-cursed-500/45 hover:bg-cursed-600/10 hover:text-zinc-100';
  const APP_ICON = 'text-cursed-300/70 group-hover:text-cursed-200 transition-colors';

  let stream_url = '';
  // v64: prefer the low-latency H.264 WebCodecs canvas when the browser
  // supports it. H264Canvas dispatches `fallback` (no WebCodecs, or no
  // keyframe ⇒ streamer still in MJPEG mode) and we revert to the <img>.
  //
  // v67: only attempt H.264 when the streamer is ACTUALLY producing it.
  // Previously useH264 was set to "browser has WebCodecs" unconditionally
  // in onMount — so on Chrome/Brave with the default MJPEG streamer, the
  // page mounted H264Canvas, got no keyframes, and showed a BLACK CANVAS
  // for ~4.5s until the no-keyframe timer fell back to MJPEG. On a reload
  // that reads as "the stream is broken". Now useH264 is derived from the
  // streamer's advertised format (state.mode.format), so an MJPEG streamer
  // shows the <img> instantly with no black-canvas probe delay, and an
  // H.264 streamer engages WebCodecs as soon as state reports it.
  let ws_url = '';
  let webCodecsOk = false;
  // Set when H264Canvas reports a fallback so we don't flap straight back
  // into a known-bad H.264 attempt on the next poll. Sticky ONLY for real
  // capability failures (no WebCodecs / configure / decode errors). For
  // transient reasons — WS closed by a streamer restart (every config PUT
  // does one), no keyframe yet, network blip — we retry after a short hold:
  // the old always-sticky behavior stranded the session on the 6 fps
  // live.jpg MJPEG fallback (or a frozen canvas) until a manual reload.
  let h264FellBack = false;
  let h264RetryTimer: ReturnType<typeof setTimeout> | undefined;
  const H264_TRANSIENT = ['ws-closed', 'ws-closed-live', 'ws-error', 'no-keyframe'];
  function onH264Fallback(reason: string) {
    console.info('[aeon] H.264 view fell back:', reason);
    h264FellBack = true;
    if (H264_TRANSIENT.includes(reason)) {
      clearTimeout(h264RetryTimer);
      h264RetryTimer = setTimeout(() => (h264FellBack = false), 5000);
    }
  }
  let state: api.StreamerState | null = null;
  // Derived: use H.264 only if the browser supports it, the streamer is
  // emitting h264, and we haven't already hit a fallback this session.
  $: useH264 =
    webCodecsOk &&
    !h264FellBack &&
    (state?.mode?.format ?? '').includes('h264');
  let hid: api.HidStatus | null = null;
  let poll_iv: ReturnType<typeof setInterval>;
  // One derived state drives every orb on the page (header, footer,
  // signal-lost veil) plus the canvas frame glow.
  $: orbMode = (captured || rec.active)
    ? 'captured' as const
    : state
      ? (state.online ? 'live' as const : 'offline' as const)
      : 'idle' as const;
  // Mirror capture into the global store so app-wide hotkeys (Cmd+K
  // palette) stand down while keystrokes belong to the target.
  $: inputCaptured.set(captured);
  // Network status pills — we only care about the small "is it on?"
  // booleans here, not the full config (the /network page owns that).
  // These change rarely, so we fetch once on mount + whenever the tab
  // regains focus — NOT on a timer. The old 5s poll needlessly re-pulled
  // the ~141 KB DNSCrypt catalog every time (see getDnscrypt's opt-in).
  let vpnOn = false;
  let vpnProvider: string = 'none';
  let dnscryptOn = false;
  // Battery (Waveshare UPS HAT (E)). null until first poll / no UPS fitted →
  // the meter simply doesn't render. Refreshed on the 2s state poll.
  let ups: api.UpsStatus | null = null;

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
  // Desktop nav dropdowns (Settings / Monitor); a fixed backdrop closes them.
  let settingsOpen = false;
  let monitorOpen = false;
  let sessionOpen = false;
  function closeDropdowns() { settingsOpen = false; monitorOpen = false; sessionOpen = false; }

  let canvas: HTMLDivElement;
  let dragging = false;
  // Which mouse button is currently held down (for click-and-drag), so we
  // know which to release on mouseup / pointer-unlock. null = none held.
  let down_button: 'left' | 'right' | 'middle' | null = null;
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
    // Battery is a separate tiny endpoint; its own try so a UPS read error
    // (or no UPS HAT) never blanks the stream state.
    try {
      ups = await api.getUps();
    } catch (e) {
      console.warn('ups refresh failed', e);
    }
    refreshRec();
  }

  // Refresh VPN + DNSCrypt enabled flags. getDnscrypt() with no args
  // returns the tiny status only (no 141 KB catalog). Called on mount +
  // on tab-focus, never on a timer.
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

  // Re-check the status pills when the user returns to the tab, instead
  // of polling on an interval.
  function onVisibility() {
    if (typeof document !== 'undefined' && !document.hidden) refreshNet();
  }

  // ── Battery meter (UPS HAT (E)) — derived display values ──────────────────
  $: batteryPct = ups?.present ? Math.max(0, Math.min(100, Math.round(ups.percent ?? 0))) : null;
  $: batteryCharging = !!ups?.charging || /charg/i.test(ups?.state ?? '');
  // Green when charging or healthy, amber ≤40%, red ≤15% — and red whenever
  // we're on battery so an unplugged Orb reads as "draining" at a glance.
  $: batteryColor =
       batteryPct == null ? 'text-zinc-400'
       : batteryCharging ? 'text-live-400'
       : batteryPct <= 15 ? 'text-red-400'
       : ups?.on_battery ? 'text-amber-300'
       : batteryPct <= 40 ? 'text-amber-400'
       : 'text-live-400';
  $: batteryTip = ups?.present
       ? `Battery ${batteryPct}% · ${ups.state ?? 'unknown'}`
         + (ups.battery_mv ? ` · ${(ups.battery_mv / 1000).toFixed(2)} V` : '')
         + (ups.minutes_to_empty ? ` · ~${ups.minutes_to_empty} min left` : '')
         + (ups.minutes_to_full ? ` · ~${ups.minutes_to_full} min to full` : '')
         + (ups.on_battery ? ' · ON BATTERY' : '')
         + (ups.shutdown_pending_s ? ` · ⚠ LOW: shutdown in ${ups.shutdown_pending_s}s` : '')
       : '';

  onMount(() => {
    stream_url = api.streamURL();
    ws_url = api.streamWsURL();
    webCodecsOk =
      typeof window !== 'undefined' &&
      'VideoDecoder' in window &&
      'EncodedVideoChunk' in window;
    refreshState();
    refreshNet();
    loadPickers();
    poll_iv = setInterval(refreshState, 2000);
    document.addEventListener('visibilitychange', onVisibility);
    window.addEventListener('keydown', onKey);
    window.addEventListener('keyup', onKey);
    // Wheel MUST be a non-passive window listener: window wheel listeners are
    // passive-by-default (browser intervention), and only a non-passive one
    // lets onWheel.preventDefault() stop the local page from scrolling while it
    // forwards the scroll to the target. Window scope (not the canvas) means it
    // also works in captured mode when the cursor isn't over the canvas.
    window.addEventListener('wheel', onWheel, { passive: false });
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
    inputCaptured.set(false);
    if (coachTimer) clearTimeout(coachTimer);
    clearInterval(poll_iv);
    clearTimeout(h264RetryTimer);
    document.removeEventListener('visibilitychange', onVisibility);
    window.removeEventListener('keydown', onKey);
    window.removeEventListener('keyup', onKey);
    window.removeEventListener('wheel', onWheel);
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
      // Release any held button so an interrupted drag (Esc / lost lock)
      // can't leave it stuck down.
      if (down_button) {
        api.mouseButton(false, down_button).catch(console.warn);
        down_button = null;
      }
    }
  }

  function enterCapture() {
    if (captured) return;
    canvas.requestPointerLock();
    // v74: Keyboard Lock API — capture browser/OS-reserved keys (F11, F12,
    // Ctrl/Cmd+W, Esc, etc.) so they reach our keydown handler and forward to
    // the target instead of firing local browser actions. Best-effort:
    // Chromium-only, captures the most keys in fullscreen, and CANNOT override
    // macOS hardware fn-key mappings (brightness / Mission Control happen below
    // the browser). For those, use the on-screen "Keys" pad or enable macOS
    // "Use F1, F2, etc. keys as standard function keys". Released on exit.
    try { (navigator as any).keyboard?.lock?.(); } catch { /* unsupported */ }
    captured = true;
  }

  function exitCapture() {
    if (!captured) return;
    if (document.pointerLockElement) document.exitPointerLock();
    try { (navigator as any).keyboard?.unlock?.(); } catch { /* noop */ }
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
    // Press and HOLD (not an atomic click) so motion before mouseup becomes a
    // drag. A quick down→up with no move between is just a normal click; two
    // quick pairs read as a double-click.
    down_button = button;
    api.mouseButton(true, button).catch(console.warn);
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
    // Safety: release a held button if the cursor leaves mid-drag (casual
    // mode) so it can't stick. Captured mode is pointer-locked, so leave
    // doesn't fire there.
    if (down_button) {
      api.mouseButton(false, down_button).catch(console.warn);
      down_button = null;
    }
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
    if (down_button) {
      api.mouseButton(false, down_button).catch(console.warn);
      down_button = null;
    }
  }

  function onWheel(ev: WheelEvent) {
    // Forward wheel / two-finger-scroll to the TARGET. This is a window-level
    // listener registered {passive:false} in onMount — mirroring onKey — for
    // two reasons:
    //   1. In captured mode EVERY wheel event must reach the target no matter
    //      where the OS cursor sits (Safari's pointer-lock is flaky and may
    //      leave the cursor free), exactly like keystrokes do.
    //   2. preventDefault() only stops the local page from scrolling /
    //      rubber-banding when the listener is NON-passive. The previous
    //      on:wheel binding was passive-by-default, so the scroll leaked to
    //      the local browser (the reported "Aeon Magick UI bounces, target
    //      doesn't scroll" bug).
    // When NOT captured we only hijack the wheel while it's over the video
    // canvas, so the rest of the Aeon Magick page scrolls normally.
    const overCanvas = !!canvas &&
      (ev.target === canvas || canvas.contains(ev.target as Node));
    if (!captured && !overCanvas) return;
    ev.preventDefault();
    const dy = -Math.sign(ev.deltaY) * 3;
    if (dy !== 0) api.scroll(dy).catch(console.warn);
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

  // ── First-run gesture coaching (touch devices) ────────────────────────
  // The touch mapping (tap=click, long-press=right-click, two-finger
  // drag=scroll) is invisible until you know it. Shown ONCE, on the
  // first fullscreen entry on a touch device; a tap or 12s dismisses it
  // forever (localStorage).
  const COACH_KEY = 'aeon-gesture-coach-seen';
  let showGestureCoach = false;
  let coachTimer: ReturnType<typeof setTimeout> | null = null;
  const GESTURES: [string, string][] = [
    ['tap', 'left click'],
    ['long-press', 'right click'],
    ['drag', 'move the pointer'],
    ['two-finger drag', 'scroll'],
  ];
  function maybeCoach() {
    if (!isTouchDevice) return;
    try {
      if (localStorage.getItem(COACH_KEY)) return;
    } catch { return; }
    showGestureCoach = true;
    coachTimer = setTimeout(dismissCoach, 12000);
  }
  function dismissCoach() {
    if (coachTimer) { clearTimeout(coachTimer); coachTimer = null; }
    showGestureCoach = false;
    try { localStorage.setItem(COACH_KEY, '1'); } catch { /* private mode */ }
  }

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
    maybeCoach();
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
    // Desktop: tie input capture to fullscreen. Entering fullscreen on a
    // pointer device auto-captures (pointer-lock + keyboard-lock) so the
    // user's mouse + keystrokes drive the target immediately — no separate
    // "Capture" click. Requesting pointer-lock HERE (after the fullscreen
    // transition completes) rather than in enterFullscreen avoids the
    // "pointer lock while transitioning" rejection, and the event still
    // carries the user-gesture activation from the fullscreen click.
    // Touch devices opt out — there's no pointer to lock; they use the
    // touch-mapping + on-screen-keyboard flow instead.
    if (!isTouchDevice) {
      if (real && !captured) enterCapture();
      else if (!real && captured) exitCapture();
    }
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
    // This keydown bubbles to the window-level `onKey`, which (because the
    // soft keyboard set captured=true) would ALSO forward the character via
    // /api/hid/key. On phones a symbol-layer key fires with ev.shiftKey=false,
    // so that path drops the Shift and types the base key ('&' → '7'). Stop it
    // here so printable chars go ONLY through the correct input→typeText path.
    ev.stopPropagation();
    // IME / composition sentinel — let the `input` event (typeText) handle it.
    if (ev.isComposing || ev.keyCode === 229) return;
    // Special keys → forward via /api/hid/key. We DON'T do this for printable
    // characters because the `input` event already handled them via typeText.
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

  // ── Screen recording ──
  let rec: {
    active: api.RecordingInfo | null;
    recordings: api.RecordingInfo[];
    note?: string | null;
  } = { active: null, recordings: [] };
  let recBusy = false;
  let recListOpen = false;
  async function refreshRec() {
    try {
      rec = await api.getRecordingState();
    } catch {
      /* recording API may be unavailable (e.g. MJPEG mode) */
    }
  }
  async function toggleRecord() {
    if (recBusy) return;
    recBusy = true;
    try {
      if (rec.active) await api.recordStop();
      else await api.recordStart(0); // open-ended — records until stopped (3h hard cap)
      await refreshRec();
    } catch (e) {
      toast.error('recording: ' + ((e as any)?.message ?? e));
    } finally {
      recBusy = false;
    }
  }
  async function onDeleteRecording(id: string) {
    try {
      await api.deleteRecording(id);
      await refreshRec();
    } catch (e) {
      console.warn(e);
    }
  }
  function fmtRecTime(ms: number): string {
    return new Date(ms).toLocaleString();
  }
  function fmtRecName(ms: number): string {
    const d = new Date(ms);
    const p = (n: number) => String(n).padStart(2, '0');
    return `aeon-recording-${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}_${p(
      d.getHours(),
    )}-${p(d.getMinutes())}-${p(d.getSeconds())}.mp4`;
  }

  async function onLogout() {
    try {
      await api.logout();
    } catch (e) {
      console.warn('logout failed', e);
    }
    window.location.href = '/login';
  }

  /// v53: header buttons control the USB-CONNECTED TARGET, not the Pi.
  /// (Pi reboot/poweroff lives in System maintenance now — rarely needed.)
  ///
  /// The common workflow these support: load an ISO via the storage
  /// drive, reboot the target, hit F12/Del during POST to enter the
  /// boot menu, pick the USB-CDROM, install an OS.
  ///
  /// We double-confirm both — power events on the target are
  /// disruptive enough that an accidental click should never trigger
  /// one without an explicit typed phrase.
  async function onTargetReboot() {
    if (!(await confirmRite({
      title: 'Reboot target machine',
      body:
        'Reboot the USB-connected target machine?\n\n' +
        '1. Hold the HID power button for 8s (forces the target to power off)\n' +
        '2. Wait 5 seconds\n' +
        '3. Send a Wake-on-LAN magic packet over usb0\n\n' +
        'Requires WoL enabled in the target\'s BIOS/UEFI. If the target ' +
        'doesn\'t support WoL, you\'ll need to press its power button by hand ' +
        'after step 1.',
      danger: true,
      phrase: 'REBOOT',
      confirmLabel: 'reboot target',
    }))) return;
    try {
      const r = await api.targetReboot();
      toast.success(`Reboot sequence sent · ${r.phases.join(' → ')} · MAC ${r.mac}`);
    } catch (e: any) {
      toast.error('Target reboot failed: ' + (e?.message ?? 'unknown'));
    }
  }
  async function onTargetPoweroff() {
    if (!(await confirmRite({
      title: 'Force power-off target',
      body:
        'This holds the HID power button for 8 seconds. Every modern ' +
        'motherboard treats that as a hardware-level shutdown — the OS ' +
        'will NOT get a chance to flush state.\n\n' +
        'Use the soft tap instead if you want a graceful OS shutdown.',
      danger: true,
      phrase: 'POWEROFF',
      confirmLabel: 'force power-off',
    }))) return;
    try {
      const r = await api.targetPowerHold();
      toast.success(`Force power-off sent (${r.hold_ms}ms hold).`);
    } catch (e: any) {
      toast.error('Target poweroff failed: ' + (e?.message ?? 'unknown'));
    }
  }
  /// Short tap — most OSes interpret this the same as a tap on the
  /// physical chassis power button. Windows: shows the power menu.
  /// macOS: shows the shutdown dialog. Linux: usually starts a clean
  /// shutdown. Far less destructive than the 8-second hold.
  async function onTargetPowerTap() {
    if (!(await confirmRite({
      title: 'Power-button tap',
      body:
        'Send a short power-button tap to the target?\n\n' +
        'Most OSes will treat this as a graceful "shutdown please" — ' +
        'Windows shows the power menu, macOS the shutdown dialog, Linux ' +
        'starts the shutdown sequence. Cancel any unsaved work first.',
      confirmLabel: 'send tap',
    }))) return;
    try {
      const r = await api.targetPowerTap();
      toast.success(`Soft tap sent (${r.hold_ms}ms).`);
    } catch (e: any) {
      toast.error('Power tap failed: ' + (e?.message ?? 'unknown'));
    }
  }
  /// Wake-on-LAN. Won't do anything if the target is already on.
  async function onTargetWake() {
    try {
      const r = await api.targetWake();
      toast.success(`WoL magic packet sent (mac=${r.mac}, via ${r.iface}).`);
    } catch (e: any) {
      toast.error(
        'Wake failed: ' + (e?.message ?? 'unknown') +
        '\n\nMake sure usb0 is up and the target has DHCP\'d at least once ' +
        '(so the ARP cache knows its MAC), or set a MAC override at ' +
        'PUT /api/target/config.'
      );
    }
  }

  // ── Persona switching ──────────────────────────────────────────────────
  // Triggers a USB re-enumeration on the target — ~1s blip. The choice is
  // persisted on the device in /etc/aeon/persona.state and survives reboots.

  // Single source of truth for the persona dropdown (both the desktop toolbar
  // and the mobile menu render from this). `agent: true` marks an agent-focused
  // persona — one whose pointer is an ABSOLUTE device, so an AI agent's
  // click_at / move_abs land on exact pixel coordinates with no relative drift.
  // The selector surfaces that with a badge so a human can see at a glance when
  // the box is in agent-drive mode (the persona can also be set over the API by
  // the agent itself). `warn` flags the experimental Apple multi-touch persona.
  // Labels: theme-facing names; slugs stay API-stable.
  const HID_PERSONAS: { value: string; label: string; agent?: boolean; warn?: boolean }[] = [
    { value: 'generic-composite', label: 'generic hub' },
    { value: 'generic-absolute', label: 'Wacom tablet (absolute)', agent: true },
    { value: 'logitech-mx', label: 'Logitech MX' },
    { value: 'apple-magic-stable', label: 'Apple Magic Kbd/Mouse' },
    { value: 'apple-magic', label: 'Apple Magic Trackpad', warn: true },
  ];
  const isAgentPersona = (p: string | undefined): boolean =>
    !!p && HID_PERSONAS.some((x) => x.value === p && x.agent);

  let persona_switching = false;
  let persona_message = '';

  async function onPersonaChange(ev: Event) {
    const select = ev.target as HTMLSelectElement;
    const newPersona = select.value;
    if (!newPersona || newPersona === hid?.persona) return;
    if (!(await confirmRite({
      title: 'Switch HID persona',
      body:
        `Switch HID persona to "${newPersona}"?\n\n` +
        `The USB device will re-enumerate (~1 second blip on the target). ` +
        `The selection will persist across reboots.`,
      confirmLabel: 'switch persona',
    }))) {
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

  // ── Video-source picker (view) + Webcam picker ──────────────────────────
  // View source = which capture source the console shows (streamer.toml).
  // Webcam = which source is passed through the USB gadget to the target host.
  let streamerCfg: api.StreamerConfig | null = null;
  let webcamCfg: api.WebcamConfig | null = null;
  let source_switching = false;
  let source_message = '';
  let webcam_switching = false;
  let webcam_message = '';

  const SOURCE_LABELS: Record<string, string> = {
    'auto': 'auto', 'cam-link-usb': 'USB capture',
    'hdmi-csi': 'HDMI (KVM)', 'camera-csi': 'camera', 'off': 'off',
  };
  const sourceLabel = (s: string) => SOURCE_LABELS[s] ?? s;

  async function loadPickers() {
    try { streamerCfg = await api.getStreamerConfig(); syncOrient(); } catch (e) { console.warn('streamer cfg', e); }
    try { webcamCfg = await api.getWebcamConfig(); } catch (e) { console.warn('webcam cfg', e); }
  }

  // ── Camera orientation (server-side rotation/flip + instant CSS preview) ──
  // The pipeline rotates the frames themselves, so the live stream, /snapshot,
  // recordings, and the vision/OCR tap all share one orientation. The control
  // is optimistic: it shows the target via a CSS delta over the still-current
  // frames while the streamer restarts (~1–2 s), then clears once the real
  // rotated frames flow in (no double-rotation because the gap is masked by the
  // brief reconnect).
  const orientBtn =
    'px-1.5 py-0.5 rounded-sm border border-steel-700 bg-ink-800 text-cursed-300 leading-none ' +
    'hover:bg-ink-700 focus:outline-none focus:ring-1 focus:ring-cursed-500/50 ' +
    'disabled:opacity-50 disabled:cursor-wait';
  let orient_switching = false;
  let orient_message = '';
  // What the pipeline is CURRENTLY emitting (frames already carry this).
  let appliedRot = 0, appliedH = false, appliedV = false;
  // Preview deltas applied to the on-screen video while the pipeline restarts.
  let previewRot = 0, previewFlipH = false, previewFlipV = false;

  function syncOrient() {
    appliedRot = streamerCfg?.rotation ?? 0;
    appliedH = streamerCfg?.hflip ?? false;
    appliedV = streamerCfg?.vflip ?? false;
    previewRot = 0; previewFlipH = false; previewFlipV = false;
  }

  // CSS transform on the inner video during a switch. A 90/270 delta swaps the
  // aspect, so scale-to-fit the rotated frame inside the view box. Empty string
  // (no transform) once committed → the real rotated frames render untouched.
  $: orientPreviewStyle = (() => {
    if (!previewRot && !previewFlipH && !previewFlipV) return '';
    const parts: string[] = [];
    if (previewRot % 180 !== 0) {
      const w = canvas?.clientWidth || 16, h = canvas?.clientHeight || 9;
      const s = Math.min(w, h) / Math.max(w, h);
      parts.push(`rotate(${previewRot}deg)`, `scale(${s})`);
    } else if (previewRot) {
      parts.push(`rotate(${previewRot}deg)`);
    }
    if (previewFlipH) parts.push('scaleX(-1)');
    if (previewFlipV) parts.push('scaleY(-1)');
    return `transform: ${parts.join(' ')}; transition: transform .25s ease;`;
  })();

  async function applyOrientation(patch: { rotation?: number; hflip?: boolean; vflip?: boolean }) {
    if (!streamerCfg || orient_switching) return;
    const newRot = ((((patch.rotation ?? appliedRot) % 360) + 360) % 360);
    const newH = patch.hflip ?? appliedH;
    const newV = patch.vflip ?? appliedV;
    if (newRot === appliedRot && newH === appliedH && newV === appliedV) return;
    // Optimistic: reflect the target in the control + preview it over the frames.
    previewRot = (((newRot - appliedRot) % 360) + 360) % 360;
    previewFlipH = newH !== appliedH;
    previewFlipV = newV !== appliedV;
    streamerCfg = { ...streamerCfg, rotation: newRot, hflip: newH, vflip: newV };
    orient_switching = true;
    orient_message = 'rotating…';
    try {
      await api.setStreamerOrientation({ rotation: newRot, hflip: newH, vflip: newV });
      // Streamer restarted; new frames now carry the orientation → drop the CSS.
      appliedRot = newRot; appliedH = newH; appliedV = newV;
      previewRot = 0; previewFlipH = false; previewFlipV = false;
      await refreshState();
      orient_message = 'rotated';
      setTimeout(() => (orient_message = ''), 2500);
    } catch (e) {
      // Revert control + preview to what's actually on the wire.
      streamerCfg = { ...streamerCfg, rotation: appliedRot, hflip: appliedH, vflip: appliedV };
      previewRot = 0; previewFlipH = false; previewFlipV = false;
      orient_message = 'error';
      setTimeout(() => (orient_message = ''), 3000);
    } finally {
      orient_switching = false;
    }
  }
  const rotateBy = (delta: number) => applyOrientation({ rotation: appliedRot + delta });
  const toggleFlipH = () => applyOrientation({ hflip: !appliedH });
  const toggleFlipV = () => applyOrientation({ vflip: !appliedV });

  async function onSourceChange(ev: Event) {
    const select = ev.target as HTMLSelectElement;
    const src = select.value;
    if (!src || src === streamerCfg?.source) return;
    source_switching = true;
    source_message = `→ ${sourceLabel(src)}…`;
    try {
      const r = await api.setStreamerSource(src) as { ok?: boolean; uvc_released?: boolean };
      if (streamerCfg) streamerCfg = { ...streamerCfg, source: src };
      // Console view wins the single-consumer camera: webcam is auto-disabled
      // when it was holding the same source (see streamer_config release_uvc).
      if (r?.uvc_released && webcamCfg) {
        webcamCfg = { ...webcamCfg, enabled: false };
        webcam_message = 'webcam auto-off (same cam)';
        setTimeout(() => (webcam_message = ''), 4000);
      }
      await refreshState();
      await loadPickers();
      source_message = `view: ${sourceLabel(src)}`;
      setTimeout(() => (source_message = ''), 3000);
    } catch (e: any) {
      source_message = 'error';
      if (streamerCfg) select.value = streamerCfg.source;
    } finally {
      source_switching = false;
    }
  }

  // ── Pi-camera encode (resolution + fps independent of HDMI/Cam-Link) ──
  let camera_switching = false;
  let camera_message = '';
  const cameraModeOf = (cfg: api.StreamerConfig | null) =>
    cfg?.camera_mode
    ?? ((cfg?.camera_height ?? 720) >= 1000 || (cfg?.camera_width ?? 1280) >= 1800 ? '1080p' : '720p');

  async function onCameraModeChange(ev: Event) {
    const mode = (ev.target as HTMLSelectElement).value;
    if (!streamerCfg || camera_switching) return;
    if (mode === cameraModeOf(streamerCfg)) return;
    camera_switching = true;
    camera_message = `→ cam ${mode}…`;
    try {
      await api.setCameraEncode({ camera_mode: mode });
      await loadPickers();
      await refreshState();
      camera_message = `cam ${mode}`;
      setTimeout(() => (camera_message = ''), 3000);
    } catch {
      camera_message = 'error';
      setTimeout(() => (camera_message = ''), 3000);
    } finally {
      camera_switching = false;
    }
  }

  async function onCameraFpsChange(ev: Event) {
    const v = Number((ev.target as HTMLSelectElement).value);
    if (!streamerCfg || camera_switching || Number.isNaN(v)) return;
    if (v === (streamerCfg.camera_fps ?? 24)) return;
    camera_switching = true;
    camera_message = `→ cam ${v} fps…`;
    try {
      await api.setCameraEncode({ camera_fps: v });
      await loadPickers();
      await refreshState();
      camera_message = `cam ${v} fps`;
      setTimeout(() => (camera_message = ''), 3000);
    } catch {
      camera_message = 'error';
      setTimeout(() => (camera_message = ''), 3000);
    } finally {
      camera_switching = false;
    }
  }

  // ── Live audio passthrough (Cam Link / HDMI target / BrainCraft mic) ──
  // Low-latency path: raw s16le PCM via fetch + Web Audio (not <audio src=mp3>,
  // which browsers buffer for ~several seconds and drift behind the H.264 view).
  let listenAudio = false;
  let audioVol: api.AudioVolume | null = null;
  let audioBusy = false;
  let listenMsg = '';
  let listenWatch: ReturnType<typeof setTimeout> | null = null;
  let listenAbort: AbortController | null = null;
  let listenCtx: AudioContext | null = null;

  async function refreshAudioVol() {
    try {
      audioVol = await api.getAudioVolume();
    } catch {
      audioVol = null;
    }
  }

  function stopListenAudio() {
    listenAudio = false;
    listenMsg = '';
    if (listenWatch) {
      clearTimeout(listenWatch);
      listenWatch = null;
    }
    try { listenAbort?.abort(); } catch { /* ignore */ }
    listenAbort = null;
    if (listenCtx) {
      try { listenCtx.close(); } catch { /* ignore */ }
      listenCtx = null;
    }
  }

  /** Play raw little-endian s16 stereo PCM as it arrives; drop backlog to stay
   *  near the live edge (aligned with the low-latency video stream). */
  async function playPcmStream(res: Response, ctx: AudioContext) {
    const rate = Number(res.headers.get('x-aeon-audio-rate') || '48000') || 48000;
    const channels = Number(res.headers.get('x-aeon-audio-channels') || '2') || 2;
    const reader = res.body?.getReader();
    if (!reader) throw new Error('no stream body');

    const bytesPerFrame = 2 * channels; // s16le
    let leftover = new Uint8Array(0);
    // Schedule slightly ahead of the clock; if we fall behind, jump to now.
    let nextTime = ctx.currentTime + 0.04;
    // Max lead we keep in the AudioContext queue (~80 ms). Bigger = smoother
    // but more A/V skew; smaller = tighter sync, more risk of underrun.
    const maxLead = 0.08;
    const minLead = 0.02;

    while (listenAudio) {
      const { done, value } = await reader.read();
      if (done || !value) break;

      // Stitch partial frames across chunk boundaries.
      let buf: Uint8Array;
      if (leftover.length) {
        buf = new Uint8Array(leftover.length + value.length);
        buf.set(leftover, 0);
        buf.set(value, leftover.length);
      } else {
        buf = value;
      }
      const usable = buf.length - (buf.length % bytesPerFrame);
      if (usable < bytesPerFrame) {
        leftover = buf;
        continue;
      }
      leftover = buf.subarray(usable);
      const samples = usable / 2;
      const i16 = new Int16Array(buf.buffer, buf.byteOffset, samples);
      const frames = samples / channels;
      if (frames < 1) continue;

      const abuf = ctx.createBuffer(channels, frames, rate);
      for (let c = 0; c < channels; c++) {
        const ch = abuf.getChannelData(c);
        for (let i = 0, j = c; i < frames; i++, j += channels) {
          ch[i] = i16[j] / 32768;
        }
      }

      // Catch up if the schedule queue grew (network burst / tab throttle).
      const now = ctx.currentTime;
      if (nextTime < now + minLead) nextTime = now + minLead;
      if (nextTime > now + maxLead) {
        // Drop this chunk to shed latency — prefer live edge over perfect audio.
        continue;
      }

      const src = ctx.createBufferSource();
      src.buffer = abuf;
      src.connect(ctx.destination);
      src.start(nextTime);
      nextTime += abuf.duration;
    }
  }

  async function toggleListenAudio() {
    if (listenAudio) {
      stopListenAudio();
      return;
    }
    // Create AudioContext inside the click gesture (autoplay policy).
    listenMsg = 'starting…';
    listenAudio = true;
    const abort = new AbortController();
    listenAbort = abort;
    let ctx: AudioContext;
    try {
      ctx = new AudioContext({ latencyHint: 'interactive', sampleRate: 48000 });
      listenCtx = ctx;
      if (ctx.state === 'suspended') await ctx.resume();
    } catch (e) {
      listenMsg = e instanceof Error ? e.message : 'audio context failed';
      listenAudio = false;
      return;
    }

    if (listenWatch) clearTimeout(listenWatch);
    listenWatch = setTimeout(() => {
      if (!listenAudio) return;
      if (listenMsg === 'starting…' || listenMsg === 'buffering…') {
        listenMsg = 'no audio yet — check target HDMI sound output';
      }
    }, 5000);

    // Fire-and-forget the stream pump so we don't await past the gesture.
    (async () => {
      try {
        const url = `${api.audioStreamUrl('pcm')}&t=${Date.now()}`;
        const res = await fetch(url, {
          credentials: 'same-origin',
          signal: abort.signal,
          cache: 'no-store',
        });
        if (!res.ok) {
          let err = `stream ${res.status}`;
          try {
            const j = await res.json();
            if (j?.err) err = j.err;
          } catch { /* not json */ }
          if (listenAudio) {
            listenMsg = err;
            listenAudio = false;
          }
          return;
        }
        const srcHdr = res.headers.get('x-aeon-audio-source') || '';
        if (listenAudio) {
          listenMsg = srcHdr
            ? `live · ${srcHdr.split(';')[0]}`
            : 'live · low-latency';
        }
        await playPcmStream(res, ctx);
      } catch (e) {
        if (abort.signal.aborted) return;
        if (listenAudio) {
          listenMsg = e instanceof Error ? e.message : 'stream failed';
          listenAudio = false;
        }
      } finally {
        if (!listenAudio) {
          try { await ctx.close(); } catch { /* ignore */ }
          if (listenCtx === ctx) listenCtx = null;
        }
      }
    })();

    refreshAudioVol();
  }

  async function onLiveVol(kind: 'playback' | 'capture', ev: Event) {
    const v = Number((ev.target as HTMLInputElement).value);
    if (Number.isNaN(v)) return;
    audioBusy = true;
    try {
      audioVol = await api.setAudioVolume(
        kind === 'playback' ? { playback: v } : { capture: v },
      );
    } catch { /* best-effort */ }
    finally { audioBusy = false; }
  }

  async function onWebcamChange(ev: Event) {
    const select = ev.target as HTMLSelectElement;
    const src = select.value; // off | camera-csi | hdmi-csi | cam-link-usb
    const cur = webcamCfg?.enabled ? webcamCfg.source : 'off';
    if (src === cur) return;
    const msg = src === 'off'
      ? 'Disable the USB webcam?\n\nThe USB gadget re-enumerates on the target (~1s blip).'
      : `Expose "${sourceLabel(src)}" to the target as a USB webcam?\n\n` +
        `The USB gadget re-enumerates (~1s blip). Avoid using the same source as the console view.`;
    if (!confirm(msg)) { select.value = cur; return; }
    webcam_switching = true;
    webcam_message = src === 'off' ? 'disabling…' : `→ webcam ${sourceLabel(src)}…`;
    try {
      await api.setWebcam(src);
      if (webcamCfg) webcamCfg = {
        ...webcamCfg, enabled: src !== 'off',
        source: src === 'off' ? webcamCfg.source : src,
      };
      webcam_message = src === 'off' ? 'webcam off' : `webcam: ${sourceLabel(src)}`;
      setTimeout(() => (webcam_message = ''), 4000);
    } catch (e: any) {
      webcam_message = 'error';
      select.value = cur;
    } finally {
      webcam_switching = false;
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
  <header class="chrome-header aeon-wardable"
          class:hidden={fullscreen}
          class:aeon-warded={captured}>
    <div class="h-0.5 w-full bg-gradient-to-r from-cursed-500/60 via-cursed-500/15 to-transparent" aria-hidden="true"></div>
    <!-- Row 1: brand + status + persona -->
    <div class="flex items-center justify-between gap-2 px-3 sm:px-5 pt-2.5 pb-2">
      <div class="flex items-center gap-2 sm:gap-3 min-w-0 flex-wrap">
        <span class="flex items-center gap-2 text-cursed-400 font-mono text-2xs sm:text-xs tracking-instrument truncate uppercase">
          <OrbMark class="w-5 h-5" mode={orbMode} />
          <span class="hidden sm:inline">AEON MAGICK · AI COMPUTER CONTROL</span>
          <span class="sm:hidden">AEON MAGICK</span>
        </span>
        {#if state}
          <span class={state.online ? 'pill-live' : 'pill-offline'}>
            <span class={state.online ? 'dot-live' : 'dot-off'}></span>
            {state.online ? 'LIVE' : 'OFFLINE'}
          </span>
        {/if}
        {#if state?.mode}
          <span class="hidden md:inline text-2xs font-mono text-zinc-500 tracking-wide">
            {state.mode.resolution} · {state.mode.format} · {state.captured_fps} fps
          </span>
        {/if}
        {#if streamerCfg && streamerCfg.available_sources.length > 1}
          <label class="hidden md:flex items-center gap-1 text-xs font-mono text-zinc-400"
                 title="Which video source this console shows">
            <select value={streamerCfg.source} on:change={onSourceChange} disabled={source_switching}
                    class="bg-ink-800 border border-steel-700 rounded px-1.5 py-0.5 text-cursed-300
                           focus:outline-none focus:ring-1 focus:ring-cursed-500
                           disabled:opacity-50 disabled:cursor-wait">
              {#each streamerCfg.available_sources as s}
                <option value={s}>{sourceLabel(s)}</option>
              {/each}
            </select>
          </label>
          {#if source_message}
            <span class="hidden md:inline text-xs font-mono text-zinc-500">{source_message}</span>
          {/if}
        {/if}
        {#if streamerCfg && streamerCfg.source === 'camera-csi'}
          <!-- Pi-camera encode knobs (also on System → Stream tuning). -->
          <label class="inline-flex items-center gap-1 text-xs font-mono text-zinc-400"
                 title="Pi camera resolution — full controls under System → Stream tuning">
            cam
            <select value={cameraModeOf(streamerCfg)} on:change={onCameraModeChange}
                    disabled={camera_switching || source_switching}
                    class="bg-ink-800 border border-steel-700 rounded px-1.5 py-0.5 text-cursed-300
                           focus:outline-none focus:ring-1 focus:ring-cursed-500
                           disabled:opacity-50 disabled:cursor-wait">
              {#each (streamerCfg.camera_modes ?? ['720p', '1080p']) as m}
                <option value={m}>{m}</option>
              {/each}
            </select>
          </label>
          <label class="inline-flex items-center gap-1 text-xs font-mono text-zinc-400"
                 title="Pi camera framerate — independent of HDMI capture card">
            <select value={streamerCfg.camera_fps ?? 24} on:change={onCameraFpsChange}
                    disabled={camera_switching || source_switching}
                    class="bg-ink-800 border border-steel-700 rounded px-1.5 py-0.5 text-cursed-300
                           focus:outline-none focus:ring-1 focus:ring-cursed-500
                           disabled:opacity-50 disabled:cursor-wait">
              {#each (streamerCfg.camera_fps_choices ?? [15, 24, 30]) as f}
                <option value={f}>{f} fps</option>
              {/each}
            </select>
          </label>
          {#if camera_message}
            <span class="inline text-xs font-mono text-zinc-500">{camera_message}</span>
          {/if}
        {/if}
        {#if streamerCfg}
          <!-- Camera orientation: rotate ±90°, reset, and mirror H/V. Server-side
               (the whole pipeline rotates), so it applies to the live view,
               snapshots, recordings, and the vision tap. Shown on every Orb
               regardless of source count — a camera mount is often turned. -->
          <div class="hidden md:flex items-center gap-0.5 text-xs font-mono text-zinc-400"
               title="Rotate / flip the camera view — applies to the live stream, snapshots, recordings & vision">
            <button class={orientBtn} on:click={() => rotateBy(-90)} disabled={orient_switching}
                    title="Rotate left 90°" aria-label="Rotate left 90 degrees">↺</button>
            <button class="{orientBtn} {(streamerCfg.rotation ?? 0) !== 0 ? 'text-cursed-200' : ''}"
                    on:click={() => applyOrientation({ rotation: 0, hflip: false, vflip: false })}
                    disabled={orient_switching}
                    title="Reset orientation to 0°">{streamerCfg.rotation ?? 0}°</button>
            <button class={orientBtn} on:click={() => rotateBy(90)} disabled={orient_switching}
                    title="Rotate right 90°" aria-label="Rotate right 90 degrees">↻</button>
            <button class="{orientBtn} {streamerCfg.hflip ? 'ring-1 ring-cursed-500 bg-cursed-900/40 text-cursed-100' : ''}"
                    on:click={toggleFlipH} disabled={orient_switching}
                    title="Mirror horizontally" aria-label="Mirror horizontally">⇆</button>
            <button class="{orientBtn} {streamerCfg.vflip ? 'ring-1 ring-cursed-500 bg-cursed-900/40 text-cursed-100' : ''}"
                    on:click={toggleFlipV} disabled={orient_switching}
                    title="Mirror vertically" aria-label="Mirror vertically">⇅</button>
          </div>
          {#if orient_message}
            <span class="hidden md:inline text-xs font-mono text-zinc-500">{orient_message}</span>
          {/if}
        {/if}
        {#if vpnOn}
          <a href="/network" class="pill-net hidden sm:inline-flex" title="Click to manage VPN">
            <span class="dot-cursed motion-safe:animate-phosphor"></span>
            {vpnProvider === 'tor' ? 'TOR' : vpnProvider === 'tailscale' ? 'TAILSCALE'
              : vpnProvider === 'wireguard' ? 'WIREGUARD' : vpnProvider === 'openvpn' ? 'OPENVPN'
              : vpnProvider === 'i2p' ? 'I2P' : 'VPN'}
          </a>
        {/if}
        {#if dnscryptOn}
          <a href="/network" class="pill-net hidden sm:inline-flex" title="DNSCrypt encrypted DNS — click to configure">
            <span class="dot-live"></span>
            DNSCrypt
          </a>
        {/if}
        {#if batteryPct != null}
          <!-- Battery meter — UPS HAT (E). SVG body fills proportionally + colors
               by charge; ⚡ when charging. Tooltip carries voltage + runtime. -->
          <span class="inline-flex items-center gap-1 font-mono text-xs {batteryColor}"
                title={batteryTip} aria-label={batteryTip}>
            <svg viewBox="0 0 28 14" class="w-6 h-3.5" fill="none" aria-hidden="true">
              <rect x="0.5" y="0.5" width="23" height="13" rx="2.5"
                    stroke="currentColor" stroke-width="1" opacity="0.7"/>
              <rect x="24.5" y="4.5" width="2.5" height="5" rx="1" fill="currentColor" opacity="0.7"/>
              <rect x="2" y="2" height="10" rx="1" fill="currentColor"
                    width={Math.max(1.5, 20 * (batteryPct ?? 0) / 100)}/>
            </svg>
            <span>{batteryPct}%</span>
            {#if batteryCharging}<span aria-hidden="true">⚡</span>{/if}
          </span>
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
              class="bg-ink-800 border border-steel-700 rounded px-1.5 py-0.5
                     text-cursed-300 focus:outline-none focus:ring-1 focus:ring-cursed-500
                     disabled:opacity-50 disabled:cursor-wait"
              title="Switch USB HID persona — triggers a 1-second re-enumeration on the target.">
              {#each HID_PERSONAS as p}
                <option value={p.value}>{p.label}{p.agent ? ' ⌖' : ''}{p.warn ? ' ⚠' : ''}</option>
              {/each}
            </select>
          </label>
          {#if isAgentPersona(hid.persona)}
            <span
              class="hidden lg:inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px]
                     font-mono font-semibold uppercase tracking-wide
                     bg-cursed-500/15 text-cursed-300 border border-cursed-500/40"
              title="Agent-focused HID: an absolute pointer, so an AI agent's click_at / move_abs land on exact pixel coordinates (no relative drift).">
              ⌖ Agent HID
            </span>
          {/if}
          {#if persona_message}
            <span class="hidden lg:inline text-xs font-mono text-zinc-500">{persona_message}</span>
          {/if}
        {/if}
        {#if webcamCfg && webcamCfg.supported && webcamCfg.available_sources.length > 0}
          <label class="hidden lg:flex items-center gap-1.5 text-xs font-mono text-cursed-400/80"
                 title="Expose a video source to the target host as a USB webcam">
            CAM:
            <select value={webcamCfg.enabled ? webcamCfg.source : 'off'}
                    on:change={onWebcamChange} disabled={webcam_switching}
                    class="bg-ink-800 border border-steel-700 rounded px-1.5 py-0.5 text-cursed-300
                           focus:outline-none focus:ring-1 focus:ring-cursed-500
                           disabled:opacity-50 disabled:cursor-wait"
                    title="Which video source is passed through USB to the target as a webcam">
              <option value="off">off</option>
              {#each webcamCfg.available_sources as s}
                <option value={s}>{sourceLabel(s)}</option>
              {/each}
            </select>
          </label>
          {#if webcam_message}
            <span class="hidden lg:inline text-xs font-mono text-zinc-500">{webcam_message}</span>
          {/if}
        {/if}
        <!-- Live target audio: low-latency PCM + Web Audio (tracks video). -->
        <button class="btn text-xs inline-flex items-center gap-1.5 {listenAudio ? 'border-cursed-500/50 text-cursed-200' : ''}"
                on:click={toggleListenAudio}
                title="Play live capture audio (Cam Link / HDMI). Low-latency PCM — target OS must route sound to this HDMI display.">
          {listenAudio ? '🔇 mute' : '🔊 listen'}
        </button>
        {#if listenMsg}
          <span class="text-[10px] font-mono {listenAudio ? 'text-zinc-500' : 'text-amber-400/90'} max-w-[16rem] truncate"
                title={listenMsg}>{listenMsg}</span>
        {/if}
        <!-- Hamburger: shown below lg, opens the mobile dropdown. -->
        <button class="btn text-xs lg:hidden"
                on:click={() => (menuOpen = !menuOpen)}
                aria-label="Open menu"
                aria-expanded={menuOpen}>
          <Icon name={menuOpen ? 'close' : 'menu'} class="w-4 h-4" />
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
      <div class="order-1 flex items-center gap-2 pr-3">
        {#if audioVol?.present && (listenAudio || audioVol.playback)}
          <label class="hidden xl:flex items-center gap-1 text-[10px] font-mono text-zinc-400"
                 title="Speaker / headphone level (BrainCraft WM8960)">
            spk
            <input type="range" min="0" max="100" step="1"
                   value={audioVol.playback?.percent ?? 50}
                   on:change={(e) => onLiveVol('playback', e)}
                   disabled={audioBusy}
                   class="w-16 accent-cursed-500" />
          </label>
          <label class="hidden xl:flex items-center gap-1 text-[10px] font-mono text-zinc-400"
                 title="Mic capture level">
            mic
            <input type="range" min="0" max="100" step="1"
                   value={audioVol.capture?.percent ?? 50}
                   on:change={(e) => onLiveVol('capture', e)}
                   disabled={audioBusy}
                   class="w-16 accent-cursed-500" />
          </label>
        {/if}
        {#if captured}
          <button class="btn-primary text-xs motion-safe:animate-ember inline-flex items-center gap-1.5" on:click={exitCapture}
                  title="Release input capture (Ctrl+Alt+Esc)">
            <Icon name="release" class="w-3.5 h-3.5" />release&nbsp;capture
          </button>
        {:else}
          <button class="btn text-xs inline-flex items-center gap-1.5" on:click={enterCapture}
                  title="Lock pointer + capture all keys for the remote system">
            <Icon name="capture" class="w-3.5 h-3.5" />capture&nbsp;input
          </button>
        {/if}
        <button class="btn text-xs inline-flex items-center gap-1.5" on:click={enterFullscreen}
                title="Fullscreen control mode — best on phones / tablets">
          <Icon name="fullscreen" class="w-3.5 h-3.5" />fullscreen
        </button>
        <SpecialKeys />
      </div>
      <!-- divider -->
      <span class="order-3 h-6 w-px bg-steel-600 mx-1" aria-hidden="true"></span>
      <!-- Group B (v99): three color-coded "super apps" + Settings/Monitor
           dropdowns. Configuration and monitoring collapse into the dropdowns;
           OrbNet / Agent Dash / GPIO stand alone. -->
      <div class="order-4 flex items-center gap-2 px-3">
        {#each SUPER_APPS as app}
          <a href={app.href} class="group {APP_BTN}" title={app.title}>
            <Icon name={app.icon} class="w-4 h-4 {APP_ICON}" />{app.label}
          </a>
        {/each}
        <span class="h-5 w-px bg-ink-800 mx-1" aria-hidden="true"></span>
        <div class="relative">
          <button class="btn text-xs inline-flex items-center gap-1.5" class:active={settingsOpen}
                  on:click={() => { settingsOpen = !settingsOpen; monitorOpen = false; }}>
            <Icon name="cpu" class="w-3.5 h-3.5 text-zinc-400" />Settings <span class="text-zinc-500">▾</span>
          </button>
          {#if settingsOpen}
            <div class="absolute left-0 top-full mt-1 w-48 panel p-1.5 z-50 shadow-xl space-y-0.5"
                 in:fly={menuIn}>
              {#each SETTINGS_ITEMS as it}
                <a href={it.href} class="flex items-center gap-2 px-2 py-1.5 rounded text-xs text-zinc-300 hover:bg-ink-800" title={it.title}>
                  <Icon name={it.icon} class="w-3.5 h-3.5 text-cursed-300/70" />{it.label}
                </a>
              {/each}
            </div>
          {/if}
        </div>
        <div class="relative">
          <button class="btn text-xs inline-flex items-center gap-1.5" class:active={monitorOpen}
                  on:click={() => { monitorOpen = !monitorOpen; settingsOpen = false; }}>
            <Icon name="list" class="w-3.5 h-3.5 text-zinc-400" />Monitor <span class="text-zinc-500">▾</span>
          </button>
          {#if monitorOpen}
            <div class="absolute left-0 top-full mt-1 w-48 panel p-1.5 z-50 shadow-xl space-y-0.5"
                 in:fly={menuIn}>
              {#each MONITOR_ITEMS as it}
                <a href={it.href} class="flex items-center gap-2 px-2 py-1.5 rounded text-xs text-zinc-300 hover:bg-ink-800" title={it.title}>
                  <Icon name={it.icon} class="w-3.5 h-3.5 text-cursed-300/70" />{it.label}
                </a>
              {/each}
            </div>
          {/if}
        </div>
      </div>
      <!-- divider -->
      <span class="order-3 h-6 w-px bg-steel-600 mx-1" aria-hidden="true"></span>
      <!-- Group C: target power + session.
           These buttons control the USB-CONNECTED MACHINE, not the Pi.
           They go through the HID Consumer power-button (soft tap or
           8-second hold) + WoL magic packet over usb0. -->
      <div class="order-2 flex items-center gap-2 pl-3">
        <!-- Screen recording — live H.264 + HDMI/capture audio → MP4 (agents: MCP/REST). -->
        <div class="relative flex items-center gap-1">
          <button class="btn text-xs whitespace-nowrap inline-flex items-center gap-1.5 {rec.active ? 'border-red-500 text-red-300 motion-safe:animate-ember' : ''}"
                  on:click={toggleRecord} disabled={recBusy}
                  title="Record target screen + audio to MP4 (until stopped, 3h cap)">
            {#if rec.active}<Icon name="stop" class="w-3 h-3" />stop&nbsp;rec&nbsp;·&nbsp;{rec.active.elapsed_s ?? 0}s{:else}<Icon name="record" class="w-3 h-3 text-red-400" />record{/if}
          </button>
          {#if rec.recordings.length}
            <button class="btn text-xs" on:click={() => (recListOpen = !recListOpen)} title="Recordings">▾&nbsp;{rec.recordings.length}</button>
            {#if recListOpen}
              <div class="absolute right-0 top-full mt-1 w-72 max-h-72 overflow-y-auto panel p-2 z-50 space-y-1 text-[10px] font-mono shadow-xl"
                   in:fly={menuIn}>
                {#if rec.note}
                  <p class="text-red-400 leading-snug pb-1 mb-1 border-b border-ink-800">{rec.note}</p>
                {/if}
                {#each rec.recordings.slice(0, 12) as r (r.id)}
                  <div class="flex items-center gap-2 py-0.5">
                    {#if r.has_thumb}
                      <img src={api.recordingThumbURL(r.id)} alt="" loading="lazy"
                           class="w-12 h-7 object-cover rounded border border-steel-700 flex-shrink-0" />
                    {/if}
                    <a class="text-cursed-300 hover:underline truncate flex-1 min-w-0"
                       href={api.recordingURL(r.id)} download={fmtRecName(r.started_ms)}>{fmtRecTime(r.started_ms)}</a>
                    <span class="text-zinc-600 whitespace-nowrap">{Math.round((r.size_bytes ?? 0) / 1024)} KB</span>
                    <button class="text-zinc-600 hover:text-red-400 flex-shrink-0" title="delete"
                            on:click={() => onDeleteRecording(r.id)}>✕</button>
                  </div>
                {/each}
              </div>
            {/if}
          {/if}
        </div>
        <TargetPowerMenu
          onWake={onTargetWake}
          onTap={onTargetPowerTap}
          onReboot={onTargetReboot}
          onForceOff={onTargetPoweroff} />
        <!-- Session — recovery + sign-out. These are rare (and two of them are
             disruptive), so they no longer spend a whole toolbar row competing
             with the controls you actually reach for. -->
        <div class="relative">
          <button class="btn text-xs inline-flex items-center gap-1.5"
                  aria-haspopup="menu" aria-expanded={sessionOpen}
                  on:click|stopPropagation={() => { sessionOpen = !sessionOpen; settingsOpen = false; monitorOpen = false; }}>
            <Icon name="cpu" class="w-3.5 h-3.5 text-zinc-400" />Session <span class="text-zinc-500">▾</span>
          </button>
          {#if sessionOpen}
            <div class="absolute right-0 top-full mt-1 w-56 stele-lit has-aether p-1.5 z-50 space-y-0.5"
                 role="menu" in:fly={menuIn}>
              <button role="menuitem" class="w-full text-left flex items-center gap-2ru px-2ru py-1.5 rounded-sm text-xs text-zinc-300 hover:bg-ink-800"
                      on:click={() => { sessionOpen = false; onReleaseAll(); }}>
                <Icon name="release" class="w-3.5 h-3.5 text-cursed-300/70" />release all keys
              </button>
              <button role="menuitem" class="w-full text-left flex items-center gap-2ru px-2ru py-1.5 rounded-sm text-xs text-zinc-300 hover:bg-ink-800"
                      on:click={() => { sessionOpen = false; onRelaunch(); }}>
                <Icon name="refresh" class="w-3.5 h-3.5 text-cursed-300/70" />relaunch streamer
              </button>
              <div class="h-px bg-steel-700/70 my-1" aria-hidden="true"></div>
              <button role="menuitem" class="w-full text-left flex items-center gap-2ru px-2ru py-1.5 rounded-sm text-xs text-zinc-400 hover:bg-ink-800 hover:text-red-300"
                      on:click={() => { sessionOpen = false; onLogout(); }}>
                <Icon name="logout" class="w-3.5 h-3.5" />sign out
              </button>
            </div>
          {/if}
        </div>
      </div>
    </div>

    <!-- On mobile, capture + fullscreen are the only inline action
         buttons (in addition to the hamburger on the right of row 1).
         Stash them in row 2 here for sm+ but pre-lg. At <sm even the
         capture/fullscreen labels collapse to icons. -->
    <div class="lg:hidden flex items-center gap-2 px-3 pb-3">
      {#if captured}
        <button class="btn-primary text-xs motion-safe:animate-ember inline-flex items-center gap-1.5" on:click={exitCapture}
                title="Release input capture (Ctrl+Alt+Esc)">
          <Icon name="release" class="w-3.5 h-3.5" /><span class="hidden sm:inline">release&nbsp;capture</span>
        </button>
      {:else}
        <button class="btn text-xs inline-flex items-center gap-1.5" on:click={enterCapture}
                title="Lock pointer + capture all keys for the remote system">
          <Icon name="capture" class="w-3.5 h-3.5" /><span class="hidden sm:inline">capture&nbsp;input</span>
        </button>
      {/if}
      <button class="btn text-xs inline-flex items-center gap-1.5" on:click={enterFullscreen}
              title="Fullscreen control mode — best on phones / tablets">
        <Icon name="fullscreen" class="w-3.5 h-3.5" /><span class="hidden sm:inline">fullscreen</span>
      </button>
      <SpecialKeys />
    </div>
  </header>

  <!-- Backdrop that closes the desktop Settings/Monitor dropdowns on outside click. -->
  {#if settingsOpen || monitorOpen || sessionOpen}
    <!-- svelte-ignore a11y-click-events-have-key-events -->
    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div class="fixed inset-0 z-40" on:click={closeDropdowns} role="presentation"></div>
  {/if}

  <!-- Mobile dropdown menu (lg:hidden). Closes when any item is tapped. -->
  {#if menuOpen}
    <!-- svelte-ignore a11y-click-events-have-key-events -->
    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div class="lg:hidden chrome-header bg-ink-900/95 backdrop-blur-sm
                px-3 py-3 space-y-3 z-30"
         in:fly={{ y: -6, duration: reduceMotion ? 0 : 120 }}
         on:click={closeMenu}
         role="menu"
         tabindex="-1">
      {#if hid}
        <!-- HID persona dropdown — full-width in the menu. -->
        <label class="flex items-center justify-between gap-2 text-xs font-mono text-cursed-400/80">
          <span class="flex items-center gap-1.5">
            HID persona:
            {#if isAgentPersona(hid.persona)}
              <span
                class="inline-flex items-center rounded px-1 py-0.5 text-[10px] font-semibold
                       uppercase tracking-wide bg-cursed-500/15 text-cursed-300 border border-cursed-500/40"
                title="Agent-focused HID: absolute pointer — click_at / move_abs land on exact pixel coordinates.">
                ⌖ Agent
              </span>
            {/if}
          </span>
          <select value={hid.persona} on:change={onPersonaChange}
                  disabled={persona_switching}
                  class="bg-ink-800 border border-steel-700 rounded px-1.5 py-0.5
                         text-cursed-300 flex-1 disabled:opacity-50">
            {#each HID_PERSONAS as p}
              <option value={p.value}>{p.label}{p.agent ? ' ⌖' : ''}{p.warn ? ' ⚠' : ''}</option>
            {/each}
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
      <!-- Nav: super-apps as a uniform colonnade of tiles, then Settings +
           Monitor sections. Same source + same treatment as the desktop row. -->
      <div class="space-y-3ru pt-1 border-t border-ink-800">
        <div class="grid grid-cols-3 gap-2ru">
          {#each SUPER_APPS as app}
            <a href={app.href} class="group flex flex-col items-center gap-1 py-2ru {APP_BTN}" title={app.title}>
              <Icon name={app.icon} class="w-5 h-5 {APP_ICON}" />{app.label}
            </a>
          {/each}
        </div>
        <div class="space-y-1">
          <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-600">Settings</p>
          <div class="grid grid-cols-2 gap-2">
            {#each SETTINGS_ITEMS as it}
              <a href={it.href} class="btn text-xs inline-flex items-center gap-1.5" title={it.title}>
                <Icon name={it.icon} class="w-3.5 h-3.5 text-cursed-300/80" />{it.label}
              </a>
            {/each}
          </div>
        </div>
        <div class="space-y-1">
          <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-600">Monitor</p>
          <div class="grid grid-cols-2 gap-2">
            {#each MONITOR_ITEMS as it}
              <a href={it.href} class="btn text-xs inline-flex items-center gap-1.5" title={it.title}>
                <Icon name={it.icon} class="w-3.5 h-3.5 text-cursed-300/80" />{it.label}
              </a>
            {/each}
          </div>
        </div>
      </div>
      <div class="space-y-2 pt-1 border-t border-ink-800">
        <p class="text-[10px] font-mono uppercase tracking-wider text-zinc-600">Session &amp; target</p>
        <div class="grid grid-cols-2 gap-2">
          <button class="btn text-xs" on:click={onReleaseAll}>release keys</button>
          <button class="btn text-xs" on:click={onRelaunch}>relaunch streamer</button>
        </div>
        <!-- Screen recording — H.264 video + capture audio → MP4 (until stopped; 3h cap). -->
        <button class="btn text-xs w-full inline-flex items-center justify-center gap-1.5 {rec.active ? 'border-red-500 text-red-300' : ''}"
                on:click={toggleRecord} disabled={recBusy}
                title="Record target screen + audio to MP4">
          {#if rec.active}<Icon name="stop" class="w-3 h-3" />stop recording · {rec.active.elapsed_s ?? 0}s{:else}<Icon name="record" class="w-3 h-3 text-red-400" />record screen+audio{/if}
        </button>
        {#if rec.note}
          <p class="text-red-400 text-[10px] leading-snug">{rec.note}</p>
        {/if}
        {#if rec.recordings.length}
          <div class="max-h-40 overflow-y-auto space-y-1 text-[10px] font-mono text-zinc-400">
            {#each rec.recordings.slice(0, 8) as r (r.id)}
              <div class="flex items-center gap-2">
                {#if r.has_thumb}
                  <img src={api.recordingThumbURL(r.id)} alt="" loading="lazy"
                       class="w-12 h-7 object-cover rounded border border-steel-700 flex-shrink-0" />
                {/if}
                <a class="text-cursed-300 hover:underline truncate flex-1 min-w-0"
                   href={api.recordingURL(r.id)} download={fmtRecName(r.started_ms)}>{fmtRecTime(r.started_ms)}</a>
                <span class="text-zinc-600 whitespace-nowrap">{Math.round((r.size_bytes ?? 0) / 1024)} KB</span>
                <button class="text-zinc-600 hover:text-red-400 flex-shrink-0" title="delete"
                        on:click={() => onDeleteRecording(r.id)}>✕</button>
              </div>
            {/each}
          </div>
        {/if}
        <!-- Target (USB-connected machine) power — consolidated dropdown. -->
        <TargetPowerMenu block
          onWake={onTargetWake}
          onTap={onTargetPowerTap}
          onReboot={onTargetReboot}
          onForceOff={onTargetPoweroff} />
        <button class="btn text-xs w-full" on:click={onLogout}>sign out</button>
      </div>
    </div>
  {/if}

  <!-- video canvas — KVM viewport / instrument glass -->
  <main class="flex-1 relative bg-black m-0 sm:m-1.5 sm:border sm:border-steel-700 min-h-0">
    <!-- corner ticks (desktop) -->
    <div class="pointer-events-none absolute inset-0 z-[6] hidden sm:block" aria-hidden="true">
      <div class="absolute top-0 left-0 w-3 h-3 border-t border-l border-cursed-500/40"></div>
      <div class="absolute top-0 right-0 w-3 h-3 border-t border-r border-cursed-500/40"></div>
      <div class="absolute bottom-0 left-0 w-3 h-3 border-b border-l border-cursed-500/40"></div>
      <div class="absolute bottom-0 right-0 w-3 h-3 border-b border-r border-cursed-500/40"></div>
    </div>
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
      on:touchstart={onTouchStart}
      on:touchmove={onTouchMove}
      on:touchend={onTouchEnd}
      on:touchcancel={onTouchEnd}
      role="application"
    >
      <!-- Inner display-only wrapper carrying the transient rotation PREVIEW.
           The transform lives here, NOT on the input-capture div above, so the
           mouse/touch coordinate frame is never rotated. orientPreviewStyle is
           '' at steady state (layout-neutral) and only set during the ~1-2s
           streamer restart, then self-clears once the truly-rotated server
           stream lands. -->
      <div class="w-full h-full" style={orientPreviewStyle}>
        {#if useH264}
          <H264Canvas url={ws_url} on:fallback={(e) => onH264Fallback(e.detail.reason)} />
        {:else}
          <img
            src={stream_url}
            alt="target screen"
            class="w-full h-full object-contain select-none pointer-events-none"
            draggable="false"
          />
        {/if}
      </div>
    </div>

    <!-- Ambient frame glow — a pointer-events-none SIBLING of the canvas
         (never a wrapper), so the canvas event handlers see identical
         event flow. The edge color answers "who owns my keyboard?":
         violet at rest, green live, smoldering red while captured. -->
    <div class="aeon-frame pointer-events-none absolute inset-0 z-[5] transition-opacity duration-500"
         class:aeon-frame-live={!!state?.online && !captured}
         class:aeon-frame-captured={captured}
         class:aeon-frame-off={!!state && !state.online}
         aria-hidden="true"></div>

    <!-- Signal-lost rite — replaces the silently-frozen <img> with a
         designed failure state + the fix action right where you're
         looking. Keyed on state.online (real outage), NEVER h264FellBack
         (that's a healthy MJPEG fallback). -->
    {#if mounted && state && !state.online}
      <div class="absolute inset-0 z-[6] pointer-events-none flex items-center justify-center bg-ink-950/75">
        <div class="absolute inset-0 aeon-static" aria-hidden="true"></div>
        <div class="pointer-events-auto relative text-center space-y-3 px-6">
          <OrbMark mode="offline" class="w-12 h-12 mx-auto motion-safe:animate-orb-flicker" />
          <p class="font-mono text-xs tracking-[0.35em] text-zinc-300">SIGNAL LOST</p>
          <p class="font-mono text-[11px] text-zinc-500">the orb sees nothing — the capture device is silent</p>
          <button class="btn text-xs inline-flex items-center gap-1.5" on:click={onRelaunch}>
            <Icon name="refresh" class="w-3.5 h-3.5" />relaunch streamer
          </button>
        </div>
      </div>
    {:else if mounted && !state}
      <!-- /api/state itself failed, so `state` is null and the SIGNAL LOST overlay
           above can never render — the designed failure state was gated behind the
           very data that failed, leaving an unlabelled black rectangle. This is the
           day-one shape of "supervisor still booting", "self-signed TLS blocked the
           API" and "this token lacks state scope", so say so. -->
      <div class="absolute inset-0 z-[6] flex items-center justify-center bg-ink-950/75">
        <div class="absolute inset-0 aeon-static" aria-hidden="true"></div>
        <div class="relative text-center space-y-3 px-6 max-w-sm">
          <OrbMark mode="offline" class="w-12 h-12 mx-auto motion-safe:animate-orb-flicker" />
          <p class="font-mono text-xs tracking-[0.35em] text-zinc-300">NO ANSWER</p>
          <p class="font-mono text-[11px] text-zinc-400 leading-relaxed">
            the supervisor isn't responding — it may still be starting, or this
            session may not have permission to read device state
          </p>
          <button class="btn text-xs inline-flex items-center gap-1.5" on:click={refreshState}>
            <Icon name="refresh" class="w-3.5 h-3.5" />retry
          </button>
        </div>
      </div>
    {/if}

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
        <div class="absolute top-0 left-0 right-0 h-px bg-gradient-to-r from-transparent via-red-500/60 to-transparent"
             aria-hidden="true"></div>
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
          <Icon name="release" class="w-4 h-4 inline -mt-0.5" /> escape
        </button>
      </div>

      <!-- Top-left: persona pill + name. Tap nothing — just a status hint
           so you remember which keyboard the target is seeing. -->
      {#if hid}
        <div class="absolute top-0 left-0 z-20 p-3 pointer-events-none"
             style="padding-top: max(0.75rem, env(safe-area-inset-top));">
          <span class="px-3 py-1.5 rounded-full
                       bg-ink-900/70 border border-steel-700 backdrop-blur-md
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
            <Icon name="keyboard" class="w-4 h-4 inline -mt-0.5" /> hide
          </button>
        {:else}
          <button class="px-4 py-2.5 rounded-full
                         bg-ink-900/80 border border-cursed-500/40 backdrop-blur-md
                         text-cursed-200 font-mono text-sm
                         shadow-lg active:scale-95 transition-transform"
                  on:click={showKeyboard}
                  aria-label="Show on-screen keyboard">
            <Icon name="keyboard" class="w-4 h-4 inline -mt-0.5" /> keyboard
          </button>
        {/if}
        <!-- Special-keys row. Saves a keyboard-toggle round-trip for
             the most common non-text keys. Only shown when keyboard is up. -->
        {#if kbdVisible}
          <div class="flex gap-1.5">
            <button class="w-12 h-12 rounded-full bg-ink-900/80 border border-steel-700
                           backdrop-blur-md text-zinc-300 active:scale-95
                           flex items-center justify-center"
                    on:click={() => api.sendKey(['ESC'])} aria-label="Send Esc">
              <Icon name="esc" class="w-5 h-5" /></button>
            <button class="w-12 h-12 rounded-full bg-ink-900/80 border border-steel-700
                           backdrop-blur-md text-zinc-300 active:scale-95
                           flex items-center justify-center"
                    on:click={() => api.sendKey(['TAB'])} aria-label="Send Tab">
              <Icon name="tab" class="w-5 h-5" /></button>
            <button class="w-12 h-12 rounded-full bg-ink-900/80 border border-steel-700
                           backdrop-blur-md text-zinc-300 active:scale-95
                           flex items-center justify-center"
                    on:click={() => api.sendKey(['BACKSPACE'])} aria-label="Send Backspace">
              <Icon name="backspace" class="w-5 h-5" /></button>
            <button class="w-12 h-12 rounded-full bg-ink-900/80 border border-steel-700
                           backdrop-blur-md text-zinc-300 active:scale-95
                           flex items-center justify-center"
                    on:click={() => api.sendKey(['ENTER'])} aria-label="Send Enter">
              <Icon name="enter" class="w-5 h-5" /></button>
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
          <Icon name="keyboard" class="w-4 h-4 inline -mt-0.5" /> release&nbsp;keys
        </button>
      </div>

      <!-- First-run gesture coach — shown once, then never again.
           pointer-events-auto ON PURPOSE: the dismissing tap must not
           forward a stray click to the target. -->
      {#if showGestureCoach}
        <!-- svelte-ignore a11y-click-events-have-key-events -->
        <!-- svelte-ignore a11y-no-static-element-interactions -->
        <div class="absolute inset-0 z-30 flex items-center justify-center bg-ink-950/80 backdrop-blur-sm"
             on:click={dismissCoach}
             on:touchend|preventDefault={dismissCoach}
             role="presentation">
          <div class="text-center space-y-5 px-8 max-w-sm">
            <OrbMark class="w-10 h-10 mx-auto" />
            <p class="font-mono text-xs tracking-[0.3em] text-cursed-300 uppercase">the orb obeys your touch</p>
            <dl class="space-y-2.5 text-left">
              {#each GESTURES as [gesture, effect]}
                <div class="flex items-baseline gap-3">
                  <dt class="font-mono text-sm text-zinc-200 w-36 text-right shrink-0">{gesture}</dt>
                  <dd class="text-sm text-zinc-500">→ {effect}</dd>
                </div>
              {/each}
            </dl>
            <p class="font-mono text-[10px] text-zinc-600 tracking-widest">tap anywhere to begin</p>
          </div>
        </div>
      {/if}
    {/if}
  </main>

  <!-- Whisper line — ambient telemetry from data the page already
       polls (state / hid / vpn); zero new requests. -->
  <footer class="px-5 py-2 border-t border-steel-700 bg-ink-900 text-[11px] font-mono text-zinc-500
                 flex items-center gap-2 overflow-hidden whitespace-nowrap aeon-wardable"
          class:hidden={fullscreen}
          class:aeon-warded={captured}>
    <OrbMark class="w-3 h-3" mode={orbMode} />
    <span class="truncate">
      {#if state?.mode}{state.mode.resolution} <span class="text-cursed-300/50">·</span> {state.mode.format} <span class="text-cursed-300/50">·</span> {state.captured_fps} fps{/if}
      {#if hid}<span class="text-cursed-300/50"> ·</span> persona: {hid.persona}{/if}
      {#if vpnOn}<span class="text-cursed-300/50"> ·</span> {vpnProvider.toUpperCase()}{/if}
      {#if dnscryptOn}<span class="text-cursed-300/50"> ·</span> DNSCRYPT{/if}
      <span class="text-cursed-300/50"> ·</span> relaunches: {state?.relaunch_count ?? 0}
    </span>
    <span class="hidden sm:inline ml-auto shrink-0 text-zinc-600" title="Command palette — jump to any page">⌘K to navigate</span>
    <span class="motion-safe:animate-cursor-blink text-cursed-400/70 shrink-0" aria-hidden="true">▊</span>
  </footer>
</div>
