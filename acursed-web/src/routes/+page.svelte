<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let stream_url = '';
  let state: api.StreamerState | null = null;
  let hid: api.HidStatus | null = null;
  let poll_iv: ReturnType<typeof setInterval>;

  let canvas: HTMLDivElement;
  let dragging = false;
  let last_x = 0;
  let last_y = 0;

  async function refreshState() {
    try {
      const sys = await api.getSystemState();
      state = sys.streamer;
      hid = sys.hid;
    } catch (e) {
      console.warn('state refresh failed', e);
    }
  }

  onMount(() => {
    stream_url = api.streamURL();
    refreshState();
    poll_iv = setInterval(refreshState, 2000);
    window.addEventListener('keydown', onKey);
    window.addEventListener('keyup', onKey);
  });

  onDestroy(() => {
    clearInterval(poll_iv);
    window.removeEventListener('keydown', onKey);
    window.removeEventListener('keyup', onKey);
  });

  // ── input capture ─────────────────────────────────────────────────────
  // We capture keystrokes on the document and forward each as a chord op
  // to the HID API. The HID daemon handles press+release atomically.

  function onKey(ev: KeyboardEvent) {
    if (ev.type === 'keydown' && !ev.repeat) {
      ev.preventDefault();
      const keys: string[] = [];
      if (ev.ctrlKey) keys.push('CTRL');
      if (ev.altKey) keys.push('ALT');
      if (ev.shiftKey) keys.push('SHIFT');
      if (ev.metaKey) keys.push('GUI');
      keys.push(translateKeyName(ev.key));
      api.sendKey(keys, 30).catch(console.warn);
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
    dragging = true;
    last_x = ev.clientX;
    last_y = ev.clientY;
    const button = (['left', 'middle', 'right'] as const)[ev.button] ?? 'left';
    // For now we send a click — full press/release semantics need an
    // absolute-mouse mode (Apple persona) or a different API.
    api.click(button, ev.detail || 1).catch(console.warn);
  }

  function onMouseMove(ev: MouseEvent) {
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
</script>

<div class="h-full flex flex-col">
  <!-- top bar -->
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <span class="text-cursed-400 font-mono text-sm tracking-widest">AEON CURSED KVM</span>
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
      {#if hid}
        <span class="text-xs font-mono text-cursed-400/80">
          HID:{hid.persona}
        </span>
      {/if}
    </div>
    <div class="flex items-center gap-2">
      <button class="btn" on:click={onReleaseAll}>release&nbsp;all&nbsp;keys</button>
      <button class="btn" on:click={onRelaunch}>relaunch&nbsp;streamer</button>
    </div>
  </header>

  <!-- video canvas -->
  <main class="flex-1 relative bg-ink-950">
    <div
      bind:this={canvas}
      class="absolute inset-0 outline-none"
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
  </main>

  <!-- bottom: console/log preview area, hidden by default; future -->
  <footer class="px-5 py-2 border-t border-ink-700 bg-ink-900 text-xs font-mono text-zinc-500">
    relaunches: {state?.relaunch_count ?? 0}
  </footer>
</div>
