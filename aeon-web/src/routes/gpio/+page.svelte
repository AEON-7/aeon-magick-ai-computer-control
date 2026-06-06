<script lang="ts">
  // GPIO / Hardware dashboard — a live 40-pin J8 header schematic + HAT, power,
  // IO and bus state. When a HAT is detected and known to the bundled library
  // (or picked from the browser), its pin usage is overlaid on the header so you
  // can see exactly which Pi pins it claims and for what (the Pi->HAT mapping).
  // Read-only; pin control / bus enablement / CSI camera arrive with v97.
  import { onMount, onDestroy } from 'svelte';

  type Pin = {
    physical: number;
    kind: string;
    bcm?: number;
    name: string;
    bus: string;
    mode?: string;
    direction?: string;
    pull?: string;
    level?: number;
    function?: string;
    active?: boolean;
    consumer?: string;
  };
  type Hat = {
    id: string;
    name: string;
    manufacturer?: string;
    type?: string[];
    pins?: Record<string, string>;
    i2c?: Record<string, { name?: string; device?: string }>;
    description?: string;
    url?: string;
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

  // HAT library + overlay
  let hatLibrary: Hat[] = [];
  let overlayHat: Hat | null = null;
  let matchApplied = false;
  let hatSearch = '';
  let showBrowser = false;

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
  async function loadHats() {
    try {
      const r = await fetch('/api/hardware/hats', { credentials: 'same-origin' }).then((r) => r.json());
      hatLibrary = r.hats ?? [];
    } catch {
      /* library optional */
    }
  }
  onMount(() => {
    refresh();
    loadHats();
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
      default:
        if (p.active) return 'bg-cursed-500/25 border-cursed-400/70 text-cursed-100 ring-1 ring-cursed-500/40';
        return 'bg-ink-800 border-ink-700 text-zinc-400 hover:border-ink-600';
    }
  }
  function dirGlyph(p: Pin): string {
    if (p.direction === 'output') return '▲';
    if (p.direction === 'alt') return '◆';
    return '▼';
  }
  function hatRole(physical: number): string | null {
    return overlayHat?.pins?.[String(physical)] ?? null;
  }
  function roleAbbr(role: string | null): string {
    if (!role) return '';
    const r = role.toLowerCase();
    if (r === 'power') return 'PWR';
    if (r === 'ground') return 'GND';
    if (r.includes('i2c')) return 'I2C';
    if (r.includes('spi')) return 'SPI';
    if (r.includes('uart')) return 'UART';
    if (r.includes('pwm')) return 'PWM';
    return r.toUpperCase().slice(0, 4);
  }

  $: pins = state?.gpio?.pins ?? [];
  $: leftPins = pins.filter((p) => p.physical % 2 === 1);
  $: rightPins = pins.filter((p) => p.physical % 2 === 0);
  $: pw = state?.power ?? {};
  $: hat = state?.hat ?? {};
  $: io = state?.io ?? {};
  $: buses = state?.buses ?? {};
  $: cam = state?.camera ?? {};
  $: activeCount = pins.filter((p) => p.kind === 'gpio' && p.active).length;
  // Auto-overlay the detected HAT's pin map once, if it's in the library.
  $: if (!matchApplied && hat?.library_match) {
    overlayHat = hat.library_match;
    matchApplied = true;
  }
  $: hatMatches = hatSearch.trim()
    ? hatLibrary
        .filter((h) => `${h.name} ${h.manufacturer ?? ''} ${(h.type ?? []).join(' ')}`.toLowerCase().includes(hatSearch.toLowerCase()))
        .slice(0, 40)
    : [];
  $: overlayChips = overlayHat?.i2c ? Object.entries(overlayHat.i2c) : [];
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
      {#if loading && !state}<p class="text-zinc-500 text-sm">reading hardware…</p>{/if}

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
          </div>
          <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
            <div class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">Active GPIO</div>
            <div class="text-2xl font-mono text-cursed-300">{activeCount}<span class="text-sm text-zinc-500">/26</span></div>
          </div>
        </section>

        <!-- HAT + library -->
        <section class="bg-ink-900 border {hat.present ? 'border-cursed-500/30' : 'border-ink-700'} rounded-xl p-5 space-y-3">
          <div class="flex items-start justify-between gap-3">
            <div>
              <h2 class="font-mono text-sm uppercase tracking-wider text-cursed-300">🎩 HAT</h2>
              {#if hat.present}
                <div class="mt-1.5 text-zinc-200 text-sm">{hat.vendor} — <span class="font-medium">{hat.product}</span></div>
                <div class="mt-0.5 text-[11px] font-mono text-zinc-600">{hat.uuid}</div>
                {#if hat.library_match}
                  <div class="mt-1 text-xs text-live-400">✓ identified as “{hat.library_match.name}” — pin map overlaid on the header ↓</div>
                {:else}
                  <div class="mt-1 text-xs text-zinc-500">Not in the pin library (long-tail board) — the AI can probe it. Or browse the library to overlay any board's map.</div>
                {/if}
              {:else}
                <div class="mt-1.5 text-zinc-500 text-sm">No HAT attached. Browse the library to preview any board's pin footprint.</div>
              {/if}
            </div>
            <button class="btn text-xs whitespace-nowrap" on:click={() => (showBrowser = !showBrowser)}>
              {showBrowser ? 'close' : `browse library (${hatLibrary.length})`}
            </button>
          </div>

          {#if showBrowser}
            <div class="space-y-2 pt-2 border-t border-ink-800">
              <input bind:value={hatSearch} placeholder="search {hatLibrary.length} HATs by name, maker, or type (motor, rtc, adc, lora…)" autocomplete="off"
                     class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
              {#if hatMatches.length}
                <div class="max-h-56 overflow-y-auto space-y-1">
                  {#each hatMatches as h}
                    <button class="w-full text-left flex items-center gap-2 px-2 py-1.5 rounded border border-ink-800 bg-ink-950/40 hover:border-cursed-500/40"
                            on:click={() => { overlayHat = h; showBrowser = false; }}>
                      <span class="text-zinc-200 text-sm flex-1 truncate">{h.name}</span>
                      {#if h.manufacturer}<span class="text-[10px] font-mono text-zinc-500">{h.manufacturer}</span>{/if}
                      {#if h.i2c}<span class="text-[10px] font-mono text-amber-300/80">I2C</span>{/if}
                    </button>
                  {/each}
                </div>
              {:else if hatSearch.trim()}
                <p class="text-xs text-zinc-600 italic">no match in the library</p>
              {/if}
            </div>
          {/if}
        </section>

        <!-- GPIO header schematic -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
          <div class="flex items-center justify-between flex-wrap gap-2">
            <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">40-pin header (J8)</h2>
            <div class="flex items-center gap-3 text-[10px] font-mono text-zinc-500">
              <span><span class="text-red-300">■</span> 5V</span>
              <span><span class="text-amber-300">■</span> 3V3</span>
              <span><span class="text-zinc-400">■</span> GND</span>
              <span><span class="text-cursed-300">■</span> active</span>
              {#if overlayHat}<span><span class="text-amber-400">◯</span> HAT pin</span>{/if}
            </div>
          </div>

          {#if overlayHat}
            <div class="flex items-center justify-between gap-3 bg-amber-500/10 border border-amber-500/30 rounded px-3 py-2">
              <span class="text-xs text-amber-200">
                Overlaying <strong>{overlayHat.name}</strong> — ◯ ringed pins are what this HAT uses{#if overlayChips.length}; chips:
                  {#each overlayChips as [addr, c]}<span class="font-mono text-amber-300">{c.device ?? c.name} @ {addr}</span>{' '}{/each}{/if}
              </span>
              <button class="text-[11px] text-amber-300/80 hover:text-amber-200 whitespace-nowrap" on:click={() => { overlayHat = null; matchApplied = true; }}>clear ✕</button>
            </div>
          {/if}

          <div class="flex justify-center">
            <div class="grid grid-cols-[1fr_auto_1fr] gap-x-2 gap-y-1.5 w-full max-w-2xl">
              {#each Array(20) as _, i}
                {@const lp = leftPins[i]}
                {@const rp = rightPins[i]}
                <button
                  class="flex items-center justify-end gap-2 px-2 py-1 rounded border text-right transition-colors {pinClass(lp)} {hatRole(lp.physical) ? 'ring-2 ring-amber-400/70' : ''} {selected?.physical === lp.physical ? 'outline outline-1 outline-cursed-300' : ''}"
                  on:click={() => (selected = lp)}
                >
                  <span class="font-mono text-xs truncate">{lp.name}</span>
                  {#if hatRole(lp.physical)}
                    <span class="font-mono text-[9px] text-amber-300">{roleAbbr(hatRole(lp.physical))}</span>
                  {:else if lp.bcm !== undefined && lp.active}
                    <span class="font-mono text-[9px] opacity-80">{dirGlyph(lp)}{lp.level === 1 ? 'HI' : 'LO'}</span>
                  {/if}
                </button>
                <div class="flex items-center justify-center gap-1 px-1">
                  <span class="font-mono text-[10px] text-zinc-600 w-4 text-center">{lp.physical}</span>
                  <span class="w-1.5 h-1.5 rounded-full bg-ink-600"></span>
                  <span class="font-mono text-[10px] text-zinc-600 w-4 text-center">{rp.physical}</span>
                </div>
                <button
                  class="flex items-center justify-start gap-2 px-2 py-1 rounded border text-left transition-colors {pinClass(rp)} {hatRole(rp.physical) ? 'ring-2 ring-amber-400/70' : ''} {selected?.physical === rp.physical ? 'outline outline-1 outline-cursed-300' : ''}"
                  on:click={() => (selected = rp)}
                >
                  {#if hatRole(rp.physical)}
                    <span class="font-mono text-[9px] text-amber-300">{roleAbbr(hatRole(rp.physical))}</span>
                  {:else if rp.bcm !== undefined && rp.active}
                    <span class="font-mono text-[9px] opacity-80">{dirGlyph(rp)}{rp.level === 1 ? 'HI' : 'LO'}</span>
                  {/if}
                  <span class="font-mono text-xs truncate">{rp.name}</span>
                </button>
              {/each}
            </div>
          </div>

          {#if selected}
            <div class="border-t border-ink-700 pt-3 flex flex-wrap items-center gap-x-6 gap-y-1 text-xs font-mono">
              <span class="text-cursed-300">pin {selected.physical} · {selected.name}</span>
              {#if selected.bcm !== undefined}
                <span class="text-zinc-400">BCM{selected.bcm}</span>
                <span class="text-zinc-400">dir: <span class="text-zinc-200">{selected.direction ?? '—'}</span></span>
                <span class="text-zinc-400">level: <span class="text-zinc-200">{selected.level === 1 ? 'HIGH' : 'LOW'}</span></span>
                <span class="text-zinc-400">pull: <span class="text-zinc-200">{selected.pull ?? '—'}</span></span>
                {#if selected.consumer}<span class="text-zinc-400">owner: <span class="text-cursed-200">{selected.consumer}</span></span>{/if}
              {/if}
              {#if hatRole(selected.physical)}<span class="text-amber-300">{overlayHat?.name} uses: {hatRole(selected.physical)}</span>{/if}
              {#if selected.bus}<span class="text-zinc-500">{selected.bus}</span>{/if}
            </div>
          {:else}
            <p class="border-t border-ink-700 pt-3 text-[11px] text-zinc-600">Click a pin for its live state + (when a HAT is overlaid) what the HAT uses it for. Pin control + I2C/SPI arrive with v97.</p>
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
          Read-only snapshot, refreshing every 4s. HAT data distilled from <span class="text-zinc-500">pinout.xyz (CC BY-SA 4.0)</span>. Pin control, PWM, I2C/SPI, remapping + the CSI camera land with v97.
        </p>
      {/if}
    </div>
  </main>
</div>
