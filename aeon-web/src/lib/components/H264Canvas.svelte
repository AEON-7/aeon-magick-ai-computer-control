<script lang="ts">
  // v64: low-latency H.264 live view.
  //
  // Consumes /api/streamer/ws — each binary WebSocket message is one H.264
  // access unit: [1 byte flags][Annex-B AU], flags bit0 = keyframe. We decode
  // with WebCodecs (hardware-accelerated) and paint only the NEWEST frame to a
  // <canvas>, so display latency is ~one decode + one rAF rather than the
  // seconds of buffering the multipart-MJPEG <img> path incurs.
  //
  // Degrades gracefully: if WebCodecs is missing, the decoder errors, or no
  // keyframe arrives promptly (e.g. the streamer is still in MJPEG mode and
  // nothing is published to /h264), we dispatch `fallback` and the parent
  // reverts to the MJPEG <img>.
  import { onMount, onDestroy, createEventDispatcher } from 'svelte';

  export let url: string; // wss://…/api/streamer/ws

  const dispatch = createEventDispatcher<{ fallback: { reason: string } }>();

  let canvasEl: HTMLCanvasElement;
  let ctx: CanvasRenderingContext2D | null = null;
  let ws: WebSocket | null = null;
  let decoder: any = null; // VideoDecoder
  let configured = false;
  let sawKey = false;
  let timestamp = 0;
  let pending: any = null; // newest decoded VideoFrame awaiting paint
  let raf = 0;
  let keyTimer: ReturnType<typeof setTimeout> | undefined;
  let destroyed = false;

  const hasWebCodecs = () =>
    typeof window !== 'undefined' &&
    'VideoDecoder' in window &&
    'EncodedVideoChunk' in window;

  // Build an `avc1.PPCCLL` codec string from the SPS NAL (type 7) inside an
  // Annex-B access unit, so VideoDecoder.configure() gets the real profile/
  // level. Falls back to baseline 3.0 if no SPS is found.
  function codecFromAu(au: Uint8Array): string {
    for (let i = 0; i + 4 < au.length; i++) {
      if (au[i] === 0 && au[i + 1] === 0 && au[i + 2] === 1) {
        const h = i + 3;
        if ((au[h] & 0x1f) === 7 && h + 3 < au.length) {
          const hex = (x: number) => x.toString(16).padStart(2, '0');
          return `avc1.${hex(au[h + 1])}${hex(au[h + 2])}${hex(au[h + 3])}`;
        }
      }
    }
    return 'avc1.42E01E';
  }

  function paintLoop() {
    raf = requestAnimationFrame(paintLoop);
    if (!pending || !canvasEl) return;
    const frame = pending;
    pending = null;
    if (canvasEl.width !== frame.displayWidth || canvasEl.height !== frame.displayHeight) {
      canvasEl.width = frame.displayWidth;
      canvasEl.height = frame.displayHeight;
      ctx = canvasEl.getContext('2d');
    }
    try {
      ctx?.drawImage(frame, 0, 0);
    } catch {
      /* frame already closed / context lost — ignore, next frame repaints */
    }
    frame.close();
  }

  function teardown() {
    if (keyTimer) {
      clearTimeout(keyTimer);
      keyTimer = undefined;
    }
    if (raf) {
      cancelAnimationFrame(raf);
      raf = 0;
    }
    try {
      ws?.close();
    } catch {}
    ws = null;
    try {
      if (decoder && decoder.state !== 'closed') decoder.close();
    } catch {}
    decoder = null;
    if (pending) {
      try {
        pending.close();
      } catch {}
      pending = null;
    }
  }

  function fallback(reason: string) {
    if (destroyed) return;
    teardown();
    dispatch('fallback', { reason });
  }

  onMount(() => {
    if (!hasWebCodecs()) {
      dispatch('fallback', { reason: 'no-webcodecs' });
      return;
    }

    const VideoDecoderCtor = (window as any).VideoDecoder;
    const EncodedVideoChunkCtor = (window as any).EncodedVideoChunk;

    decoder = new VideoDecoderCtor({
      output: (frame: any) => {
        // Keep only the newest decoded frame — drop any unpainted prior so
        // display latency stays at one frame even if decode outpaces rAF.
        if (pending) {
          try {
            pending.close();
          } catch {}
        }
        pending = frame;
      },
      error: (e: any) => fallback('decoder: ' + (e?.message ?? e)),
    });

    ws = new WebSocket(url);
    ws.binaryType = 'arraybuffer';

    // No keyframe within a few seconds ⇒ streamer isn't producing H.264
    // (MJPEG mode, or the path is dead) ⇒ fall back to the MJPEG <img>.
    keyTimer = setTimeout(() => {
      if (!sawKey) fallback('no-keyframe');
    }, 4500);

    ws.onmessage = (ev: MessageEvent) => {
      const buf = new Uint8Array(ev.data as ArrayBuffer);
      if (buf.length < 2) return;
      const isKey = (buf[0] & 1) === 1;
      const au = buf.subarray(1);
      if (!sawKey && !isKey) return; // wait for the first IDR before decoding
      if (!configured) {
        if (!isKey) return;
        try {
          decoder.configure({ codec: codecFromAu(au), optimizeForLatency: true });
          configured = true;
        } catch (e: any) {
          fallback('configure: ' + (e?.message ?? e));
          return;
        }
      }
      if (isKey && !sawKey) {
        sawKey = true;
        if (keyTimer) {
          clearTimeout(keyTimer);
          keyTimer = undefined;
        }
      }
      try {
        decoder.decode(
          new EncodedVideoChunkCtor({
            type: isKey ? 'key' : 'delta',
            timestamp,
            data: au,
          }),
        );
        timestamp += 16_666; // monotonic µs; spacing is cosmetic for live decode
      } catch (e: any) {
        fallback('decode: ' + (e?.message ?? e));
      }
    };
    ws.onerror = () => fallback('ws-error');
    ws.onclose = () => {
      if (!sawKey) fallback('ws-closed');
    };

    raf = requestAnimationFrame(paintLoop);
  });

  onDestroy(() => {
    destroyed = true;
    teardown();
  });
</script>

<canvas
  bind:this={canvasEl}
  class="w-full h-full object-contain select-none pointer-events-none"
></canvas>
