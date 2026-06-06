<script lang="ts">
  // GPIO / Hardware dashboard — a live 40-pin J8 header schematic + HAT, power,
  // IO and bus state. Read-only (backed by GET /api/hardware/state). Pin
  // control / bus enablement / CSI camera arrive with the v97 image.
  import { onMount, onDestroy } from 'svelte';

  type Pin = {
    physical: number;
    kind: string; // 5v | 3v3 | gnd | gpio | id_eeprom
    bcm?: number;
    name: string;
    bus: string;
    mode?: string;
    direction?: string; // input | output | alt
    pull?: string;
    level?: number;
    function?: string;
    active?: boolean;
    consumer?: string;
  };
  type HwState = {
    ok: boolean;
    model: string;
    hat: any;
    power: any;
    gpio: { pins: Pin[] };
    io: any;
    buses: any;
    camera: any;
  };

  let state: HwState | null = null;
  let error = '';
  let loading = true;
  let selected: Pin | null = null;
  let poll: ReturnType<typeof setInterval>;

  async function refresh() {
    try {
      const r = await fetch('/api/hardware/state', { credentials: 'same-origin' }).then((r) => r.json());
      if (r.ok === false) throw new Error(r.err ?? 'introspection failed');
      state = r;
      error = '';
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
    } finally {
      loading = false;
    }
  }
  onMount(() => {
    refresh();
    poll = setInterval(refresh, 4000);
  });
  onDestroy(() => clearInterval(poll));

  function pinClass(p: Pin): string {
    switch (p.kind) {
      case '5v':
        return 'bg-red-500/15 border-red-500/40 text-red-300';
      case '3v3':
        return 'bg-amber-500/15 border-amber-500/40 text-amber-300';
      case 'gnd':
        return 'bg-zinc-600/20 border-zinc-600/50 text-zinc-400';
      case 'id_eeprom':
        return 'bg-sky-500/15 border-sky-500/40 text-sky-300';
      default: // gpio
        if (p.active) return 'bg-cursed-500/25 border-cursed-400/70 text-cursed-100 ring-1 ring-cursed-500/40';
        return 'bg-ink-800 border-ink-700 text-zinc-400 hover:border-ink-600';
    }
  }
  function dirGlyph(p: Pin): string {
    if (p.direction === 'output') return '▲';
    if (p.direction === 'alt') return '◆';
    return '▼';
  }

  $: pins = state?.gpio?.pins ?? [];
  $: leftPins = pins.filter((p) => p.physical % 2 === 1); // 1,3,5…39
  $: rightPins = pins.filter((p) => p.physical % 2 === 0); // 2,4,6…40
  $: pw = state?.power ?? {};
  $: hat = state?.hat ?? {};
  $: io = state?.io ?? {};
  $: buses = state?.buses ?? {};
  $: cam = state?.camera ?? {};
  $: activeCount = pins.filter((p) => p.kind === 'gpio' && p.active).length;
</script>

<div class="h-full flex flex-col bg-ink-950">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-zinc-500 hover:text-cursed-300 font-mono text-xs">← back</a>
      <span class="text-zinc-300 font-mono text-sm uppercase tracking-wider">GPIO / Hardware</span>
    </div>
    {#if state}<span class="text-[11px] font-mono text-zinc-500 truncate max-w-[50%]">{state.model}</span>{/if}
  </header>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-5xl mx-auto w-full space-y-6">
      {#if error}
        <div class="bg-red-900/20 border border-red-500/40 rounded-xl p-4 text-red-300 text-sm font-mono">{error}</div>
      {/if}
      {#if loading && !state}
        <p class="text-zinc-500 text-sm">reading hardware…</p>
      {/if}

      {#if state}
        <!-- Board + power summary -->
        <section class="grid grid-cols-2 md:grid-cols-4 gap-3">
          <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
            <div class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">CPU temp</div>
            <div class="text-2xl font-mono text-zinc-100">{pw.temp_c}<span class="text-sm text-zinc-500">°C</span></div>
          </div>
          <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
            <div class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">Core voltage</div>
            <div class="text-2xl font-mono text-zinc-100">{pw.core_volts}<span class="text-sm text-zinc-500">V</span></div>
          </div>
          <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
            <div class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">Power</div>
            {#if pw.healthy}
              <div class="text-lg font-mono text-live-400">● healthy</div>
            {:else}
              <div class="text-lg font-mono text-red-400">⚠ {pw.undervolt_now ? 'undervolt' : pw.throttled_now ? 'throttled' : 'fault'}</div>
            {/if}
            {#if pw.undervolt_occurred || pw.throttle_occurred}
              <div class="text-[10px] text-amber-400/80 mt-0.5">since boot: {pw.undervolt_occurred ? 'undervolt ' : ''}{pw.throttle_occurred ? 'throttle' : ''}</div>
            {/if}
          </div>
          <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
            <div class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">Active GPIO</div>
            <div class="text-2xl font-mono text-cursed-300">{activeCount}<span class="text-sm text-zinc-500">/26</span></div>
          </div>
        </section>

        <!-- HAT -->
        {#if hat.present}
          <section class="bg-ink-900 border border-cursed-500/30 rounded-xl p-5">
            <div class="flex items-start justify-between gap-3">
              <div>
                <h2 class="font-mono text-sm uppercase tracking-wider text-cursed-300">🎩 HAT detected</h2>
                <div class="mt-1.5 text-zinc-200 text-sm">{hat.vendor} — <span class="font-medium">{hat.product}</span></div>
                <div class="mt-1 text-[11px] font-mono text-zinc-500">
                  id {hat.product_id} · rev {hat.product_ver} · <span class="text-zinc-600">{hat.uuid}</span>
                </div>
              </div>
              <span class="text-[10px] font-mono text-zinc-500 text-right">read from<br />HAT EEPROM</span>
            </div>
          </section>
        {/if}

        <!-- GPIO header schematic -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
          <div class="flex items-center justify-between">
            <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">40-pin header (J8)</h2>
            <div class="flex items-center gap-3 text-[10px] font-mono text-zinc-500">
              <span><span class="text-red-300">■</span> 5V</span>
              <span><span class="text-amber-300">■</span> 3V3</span>
              <span><span class="text-zinc-400">■</span> GND</span>
              <span><span class="text-cursed-300">■</span> active</span>
              <span><span class="text-sky-300">■</span> ID</span>
            </div>
          </div>

          <div class="flex justify-center">
            <div class="grid grid-cols-[1fr_auto_1fr] gap-x-2 gap-y-1.5 w-full max-w-2xl">
              {#each Array(20) as _, i}
                {@const lp = leftPins[i]}
                {@const rp = rightPins[i]}
                <!-- left pin (odd physical) -->
                <button
                  class="flex items-center justify-end gap-2 px-2 py-1 rounded border text-right transition-colors {pinClass(lp)} {selected?.physical === lp.physical ? 'outline outline-1 outline-cursed-300' : ''}"
                  on:click={() => (selected = lp)}
                >
                  <span class="font-mono text-xs truncate">{lp.name}</span>
                  {#if lp.bcm !== undefined && lp.active}
                    <span class="font-mono text-[9px] opacity-80">{dirGlyph(lp)}{lp.level === 1 ? 'HI' : 'LO'}</span>
                  {/if}
                </button>
                <!-- physical pin numbers (center rail) -->
                <div class="flex items-center justify-center gap-1 px-1">
                  <span class="font-mono text-[10px] text-zinc-600 w-4 text-center">{lp.physical}</span>
                  <span class="w-1.5 h-1.5 rounded-full bg-ink-600"></span>
                  <span class="font-mono text-[10px] text-zinc-600 w-4 text-center">{rp.physical}</span>
                </div>
                <!-- right pin (even physical) -->
                <button
                  class="flex items-center justify-start gap-2 px-2 py-1 rounded border text-left transition-colors {pinClass(rp)} {selected?.physical === rp.physical ? 'outline outline-1 outline-cursed-300' : ''}"
                  on:click={() => (selected = rp)}
                >
                  {#if rp.bcm !== undefined && rp.active}
                    <span class="font-mono text-[9px] opacity-80">{dirGlyph(rp)}{rp.level === 1 ? 'HI' : 'LO'}</span>
                  {/if}
                  <span class="font-mono text-xs truncate">{rp.name}</span>
                </button>
              {/each}
            </div>
          </div>

          <!-- selected pin detail -->
          {#if selected}
            <div class="border-t border-ink-700 pt-3 flex flex-wrap items-center gap-x-6 gap-y-1 text-xs font-mono">
              <span class="text-cursed-300">pin {selected.physical} · {selected.name}</span>
              {#if selected.bcm !== undefined}
                <span class="text-zinc-400">BCM{selected.bcm}</span>
                <span class="text-zinc-400">dir: <span class="text-zinc-200">{selected.direction ?? '—'}</span></span>
                <span class="text-zinc-400">level: <span class="text-zinc-200">{selected.level === 1 ? 'HIGH' : 'LOW'}</span></span>
                <span class="text-zinc-400">pull: <span class="text-zinc-200">{selected.pull ?? '—'}</span></span>
                <span class="text-zinc-400">func: <span class="text-zinc-200">{selected.function ?? '—'}</span></span>
                {#if selected.consumer}<span class="text-zinc-400">owner: <span class="text-cursed-200">{selected.consumer}</span></span>{/if}
              {/if}
              {#if selected.bus}<span class="text-zinc-500">{selected.bus}</span>{/if}
            </div>
          {:else}
            <p class="border-t border-ink-700 pt-3 text-[11px] text-zinc-600">Click a pin for its live state, function, and owning software. Pin control + I2C/SPI arrive with the v97 image.</p>
          {/if}
        </section>

        <!-- IO + buses -->
        <div class="grid md:grid-cols-2 gap-6">
          <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
            <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">IO &amp; network</h2>
            <div class="space-y-1.5">
              {#each io.interfaces ?? [] as iface}
                <div class="flex items-center gap-2 text-xs font-mono">
                  <span class="w-2 h-2 rounded-full {iface.operstate === 'up' ? 'bg-live-400' : iface.operstate === 'down' ? 'bg-zinc-600' : 'bg-amber-400'}"></span>
                  <span class="text-zinc-200 w-20">{iface.name}</span>
                  <span class="text-zinc-500 w-16">{iface.type}</span>
                  <span class="text-zinc-400 flex-1">{iface.operstate}{#if iface.speed_mbps} · {iface.speed_mbps}Mb{/if}</span>
                </div>
              {/each}
            </div>
            {#if (io.usb ?? []).length}
              <div class="pt-2 border-t border-ink-800">
                <div class="text-[10px] font-mono uppercase tracking-wider text-zinc-500 mb-1">USB</div>
                {#each io.usb as u}
                  <div class="text-xs text-zinc-400 truncate"><span class="text-zinc-600 font-mono">{u.id}</span> {u.name}</div>
                {/each}
              </div>
            {/if}
            {#if (io.serial ?? []).length}
              <div class="text-[11px] font-mono text-zinc-500">serial: {(io.serial ?? []).join(', ')}</div>
            {/if}
          </section>

          <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
            <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">Buses &amp; camera</h2>
            <div class="space-y-1.5 text-xs font-mono">
              <div class="flex items-center gap-2">
                <span class="w-2 h-2 rounded-full {buses.i2c?.enabled ? 'bg-live-400' : 'bg-zinc-600'}"></span>
                <span class="text-zinc-300 w-12">I2C</span>
                <span class="text-zinc-500">{buses.i2c?.enabled ? (buses.i2c.devices ?? []).join(', ') : 'disabled (enable in v97 — needs reboot)'}</span>
              </div>
              <div class="flex items-center gap-2">
                <span class="w-2 h-2 rounded-full {buses.spi?.enabled ? 'bg-live-400' : 'bg-zinc-600'}"></span>
                <span class="text-zinc-300 w-12">SPI</span>
                <span class="text-zinc-500">{buses.spi?.enabled ? (buses.spi.devices ?? []).join(', ') : 'disabled (enable in v97 — needs reboot)'}</span>
              </div>
              <div class="flex items-center gap-2">
                <span class="w-2 h-2 rounded-full {buses.uart ? 'bg-live-400' : 'bg-zinc-600'}"></span>
                <span class="text-zinc-300 w-12">UART</span>
                <span class="text-zinc-500">{buses.uart ? 'serial console' : 'off'}</span>
              </div>
              <div class="flex items-center gap-2">
                <span class="w-2 h-2 rounded-full {cam.csi?.detected ? 'bg-live-400' : 'bg-zinc-600'}"></span>
                <span class="text-zinc-300 w-12">CSI cam</span>
                <span class="text-zinc-500">{cam.csi?.detected ? 'camera attached' : cam.csi?.supported ? 'supported, none attached' : 'none (add a Pi camera + v97)'}</span>
              </div>
            </div>
            <p class="text-[11px] text-zinc-600 pt-1 border-t border-ink-800">The live feed runs over the USB HDMI-capture device (Cam Link) — tune it on the streamer. CSI is for an optional ribbon camera module.</p>
          </section>
        </div>

        <p class="text-[11px] text-zinc-600 text-center">
          Read-only snapshot, refreshing every 4s. Pin control, PWM, I2C/SPI, remapping, and the CSI camera land with the v97 image + the structured MCP tools.
        </p>
      {/if}
    </div>
  </main>
</div>
