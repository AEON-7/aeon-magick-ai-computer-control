<script lang="ts">
  // GPIO / Hardware dashboard — a live 40-pin J8 header + HAT identification, a
  // stack planner (overlay multiple HATs, detect pin/address collisions), power,
  // IO and bus state. Read-only; pin control / buses / CSI camera arrive with v97.
  import { onMount, onDestroy } from 'svelte';

  type Pin = {
    physical: number; kind: string; bcm?: number; name: string; bus: string;
    mode?: string; direction?: string; pull?: string; level?: number;
    function?: string; active?: boolean; consumer?: string;
  };
  type Hat = {
    id: string; name: string; manufacturer?: string; type?: string[];
    pins?: Record<string, string>; i2c?: Record<string, { name?: string; device?: string }>;
    description?: string; url?: string;
  };
  type HwState = {
    ok: boolean; model: string; hat: any; power: any;
    gpio: { pins: Pin[] }; io: any; buses: any; camera: any;
  };
  type StackCheck = {
    verdict: string; // compatible | i2c_address_conflict | pin_conflict
    conflicts: Array<{ type: string; physical?: number; address?: string; users: any[] }>;
    free_gpio_pins: number[];
  };

  let state: HwState | null = null;
  let error = '';
  let loading = true;
  let selected: Pin | null = null;
  let poll: ReturnType<typeof setInterval>;

  // HAT library + stack planner
  let hatLibrary: Hat[] = [];
  let stack: Hat[] = [];
  let stackCheck: StackCheck | null = null;
  let detailHat: Hat | null = null;
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
    } catch { /* optional */ }
  }
  async function runCheck() {
    if (stack.length < 1) { stackCheck = null; return; }
    const ids = stack.map((h) => h.id).join(',');
    try {
      stackCheck = await fetch(`/api/hardware/hats/check?ids=${encodeURIComponent(ids)}`, { credentials: 'same-origin' }).then((r) => r.json());
    } catch { stackCheck = null; }
  }
  function addToStack(h: Hat) {
    if (!stack.find((x) => x.id === h.id)) {
      stack = [...stack, h];
      runCheck();
    }
  }
  function removeFromStack(id: string) {
    stack = stack.filter((h) => h.id !== id);
    runCheck();
  }
  onMount(() => {
    refresh();
    loadHats();
    poll = setInterval(refresh, 4000);
  });
  onDestroy(() => clearInterval(poll));

  function pinClass(p: Pin): string {
    switch (p.kind) {
      case '5v': return 'bg-red-500/15 border-red-500/40 text-red-300';
      case '3v3': return 'bg-amber-500/15 border-amber-500/40 text-amber-300';
      case 'gnd': return 'bg-zinc-600/20 border-zinc-600/50 text-zinc-400';
      case 'id_eeprom': return 'bg-sky-500/15 border-sky-500/40 text-sky-300';
      default:
        if (p.active) return 'bg-cursed-500/25 border-cursed-400/70 text-cursed-100 ring-1 ring-cursed-500/40';
        return 'bg-ink-800 border-ink-700 text-zinc-400 hover:border-ink-600';
    }
  }
  function dirGlyph(p: Pin): string {
    return p.direction === 'output' ? '▲' : p.direction === 'alt' ? '◆' : '▼';
  }
  function pinUsers(physical: number): Array<{ name: string; role: string }> {
    return stack.filter((h) => h.pins?.[String(physical)]).map((h) => ({ name: h.name, role: h.pins![String(physical)] }));
  }
  function pinRole(physical: number): string | null {
    const u = pinUsers(physical);
    return u.length ? u[0].role : null;
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
  function connDesc(role: string, physical: number): string {
    const r = (role || '').toLowerCase();
    if (r === 'power') return physical === 1 || physical === 17 ? '3.3 V supply rail' : '5 V supply rail';
    if (r === 'ground') return 'Ground return';
    if (r.includes('i2c')) return physical === 3 ? 'I2C data (SDA) — commands + data to the on-board chips' : physical === 5 ? 'I2C clock (SCL)' : 'I2C bus line';
    if (r.includes('spi')) return 'SPI bus (data / clock / chip-select)';
    if (r.includes('uart')) return 'Serial line (TX / RX)';
    if (r.includes('pwm')) return 'PWM output';
    return "GPIO — the HAT's control / interrupt line";
  }
  function hatConnections(h: Hat): Array<{ physical: number; name: string; role: string; desc: string }> {
    if (!h?.pins) return [];
    return Object.entries(h.pins)
      .map(([phys, role]) => {
        const p = pins.find((pp) => pp.physical === Number(phys));
        const name = p?.name ?? (role === 'power' ? 'PWR' : role === 'ground' ? 'GND' : `pin ${phys}`);
        return { physical: Number(phys), name, role: String(role), desc: connDesc(String(role), Number(phys)) };
      })
      .sort((a, b) => a.physical - b.physical);
  }
  // overlay ring: red on a conflict, amber when a stacked HAT uses the pin
  function pinRing(physical: number): string {
    if (conflictPins.has(physical)) return 'ring-2 ring-red-500';
    if (pinRole(physical)) return 'ring-2 ring-amber-400/70';
    return '';
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
  $: conflictPins = new Set((stackCheck?.conflicts ?? []).filter((c) => c.type === 'pin').map((c) => c.physical));
  $: addrConflicts = (stackCheck?.conflicts ?? []).filter((c) => c.type === 'i2c_address');
  // auto-add a detected+identified HAT to the stack once
  $: if (!matchApplied && hat?.library_match) { addToStack(hat.library_match); matchApplied = true; }
  // Browse-all when the search is empty (scroll container caps the view), filter when typing.
  $: hatMatches = hatSearch.trim()
    ? hatLibrary.filter((h) => `${h.name} ${h.manufacturer ?? ''} ${(h.type ?? []).join(' ')}`.toLowerCase().includes(hatSearch.toLowerCase()))
    : hatLibrary;
  // Drop the connection-detail focus if its HAT leaves the stack.
  $: if (detailHat && !stack.find((h) => h.id === detailHat.id)) detailHat = null;
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
      {#if error}<div class="bg-red-900/20 border border-red-500/40 rounded-xl p-4 text-red-300 text-sm font-mono">{error}</div>{/if}
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
            {#if pw.healthy}<div class="text-lg font-mono text-live-400">● healthy</div>
            {:else}<div class="text-lg font-mono text-red-400">⚠ {pw.undervolt_now ? 'undervolt' : pw.throttled_now ? 'throttled' : 'fault'}</div>{/if}
          </div>
          <div class="bg-ink-900 border border-ink-700 rounded-xl p-4">
            <div class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">Active GPIO</div>
            <div class="text-2xl font-mono text-cursed-300">{activeCount}<span class="text-sm text-zinc-500">/26</span></div>
          </div>
        </section>

        <!-- HAT detection + stack planner -->
        <section class="bg-ink-900 border {hat.present ? 'border-cursed-500/30' : 'border-ink-700'} rounded-xl p-5 space-y-3">
          <div class="flex items-start justify-between gap-3">
            <div>
              <h2 class="font-mono text-sm uppercase tracking-wider text-cursed-300">🎩 HAT &amp; stack planner</h2>
              {#if hat.present}
                <div class="mt-1.5 text-zinc-200 text-sm">{hat.vendor} — <span class="font-medium">{hat.product}</span></div>
                {#if hat.library_match}<div class="mt-1 text-xs text-live-400">✓ identified as “{hat.library_match.name}”</div>
                {:else}<div class="mt-1 text-xs text-zinc-500">Not in the pin library (long-tail) — the AI can probe it. Add boards below to plan a stack.</div>{/if}
              {:else}
                <div class="mt-1.5 text-zinc-500 text-sm">No HAT attached. Add boards below to plan a stack + check compatibility before you plug anything in.</div>
              {/if}
            </div>
            <button class="btn text-xs whitespace-nowrap" on:click={() => (showBrowser = !showBrowser)}>
              {showBrowser ? 'close' : `+ add HAT (${hatLibrary.length})`}
            </button>
          </div>

          {#if showBrowser}
            <div class="space-y-2 pt-2 border-t border-ink-800">
              <input bind:value={hatSearch} placeholder="search {hatLibrary.length} HATs by name, maker, or type (motor, rtc, adc, lora…)" autocomplete="off"
                     class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
              {#if hatMatches.length}
                <div class="max-h-56 overflow-y-auto space-y-1">
                  {#each hatMatches as h}
                    <button class="w-full text-left flex items-center gap-2 px-2 py-1.5 rounded border border-ink-800 bg-ink-950/40 hover:border-cursed-500/40 disabled:opacity-40"
                            disabled={!!stack.find((x) => x.id === h.id)} on:click={() => addToStack(h)}>
                      <span class="text-zinc-200 text-sm flex-1 truncate">{h.name}</span>
                      {#if h.manufacturer}<span class="text-[10px] font-mono text-zinc-500">{h.manufacturer}</span>{/if}
                      {#if h.i2c}<span class="text-[10px] font-mono text-amber-300/80">I2C</span>{/if}
                    </button>
                  {/each}
                </div>
              {:else if hatSearch.trim()}<p class="text-xs text-zinc-600 italic">no match in the library</p>{/if}
            </div>
          {/if}

          {#if stack.length}
            <div class="pt-2 border-t border-ink-800 space-y-2">
              <div class="flex flex-wrap items-center gap-2">
                <span class="text-[10px] font-mono uppercase tracking-wider text-zinc-500">stack:</span>
                {#each stack as h}
                  <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded border {detailHat?.id === h.id ? 'border-amber-300 bg-amber-500/20' : 'border-amber-500/40 bg-amber-500/10'} text-amber-200 text-xs">
                    <button class="hover:text-amber-100" on:click={() => (detailHat = detailHat?.id === h.id ? null : h)} title="show pin connections">{h.name}</button>
                    <button class="text-amber-400/70 hover:text-amber-200" on:click={() => removeFromStack(h.id)}>✕</button>
                  </span>
                {/each}
                {#if stack.length && !detailHat}<span class="text-[10px] text-zinc-600">← tap a board for its pin connections</span>{/if}
              </div>

              {#if detailHat}
                <div class="pt-2 border-t border-ink-800 space-y-1">
                  <div class="flex items-center justify-between gap-2 flex-wrap">
                    <span class="text-[11px] font-mono text-amber-200">{detailHat.name} — pin connections</span>
                    {#if detailHat.i2c}<span class="text-[10px] font-mono text-amber-300/70">chips: {Object.entries(detailHat.i2c).map(([a, c]) => `${c.device ?? c.name} @ ${a}`).join(', ')}</span>{/if}
                  </div>
                  <div class="space-y-0.5 max-h-44 overflow-y-auto pr-1">
                    {#each hatConnections(detailHat) as c}
                      <div class="flex items-baseline gap-2 text-[11px] font-mono">
                        <span class="text-zinc-600 w-9">#{c.physical}</span>
                        <span class="text-zinc-300 w-16 truncate">{c.name}</span>
                        <span class="text-amber-300 w-9">{roleAbbr(c.role)}</span>
                        <span class="text-zinc-400 flex-1">{c.desc}</span>
                      </div>
                    {/each}
                  </div>
                </div>
              {/if}

              {#if stack.length >= 2 && stackCheck}
                {#if stackCheck.verdict === 'compatible'}
                  <div class="text-sm text-live-400">✓ Compatible — shared I2C/SPI buses + rails, no pin or address conflicts.</div>
                {:else}
                  <div class="bg-red-900/20 border border-red-500/40 rounded p-3 space-y-1.5">
                    <div class="text-sm text-red-300 font-medium">⚠ {stackCheck.verdict === 'i2c_address_conflict' ? 'I2C address conflict' : 'Pin conflict'} — these boards can't stack as-is.</div>
                    {#each stackCheck.conflicts as c}
                      <div class="text-xs font-mono text-red-200/90">
                        {#if c.type === 'i2c_address'}
                          I2C {c.address}: {(c.users ?? []).join(' + ')} — both answer to the same address (change a jumper/strap).
                        {:else}
                          pin {c.physical}: {(c.users ?? []).map((u) => `${u.hat} (${u.role})`).join(' vs ')} — reroute one.
                        {/if}
                      </div>
                    {/each}
                    {#if stackCheck.free_gpio_pins?.length && stackCheck.verdict === 'pin_conflict'}
                      <div class="text-[11px] text-amber-300/80">free GPIO pins to reroute onto: {stackCheck.free_gpio_pins.slice(0, 12).join(', ')}…</div>
                    {/if}
                  </div>
                {/if}
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
              {#if stack.length}<span><span class="text-amber-400">◯</span> HAT</span>{/if}
              {#if conflictPins.size}<span><span class="text-red-500">◯</span> conflict</span>{/if}
            </div>
          </div>

          <div class="flex justify-center">
            <div class="grid grid-cols-[1fr_auto_1fr] gap-x-2 gap-y-1.5 w-full max-w-2xl">
              {#each Array(20) as _, i}
                {@const lp = leftPins[i]}
                {@const rp = rightPins[i]}
                <button class="flex items-center justify-end gap-2 px-2 py-1 rounded border text-right transition-colors {pinClass(lp)} {pinRing(lp.physical)} {selected?.physical === lp.physical ? 'outline outline-1 outline-cursed-300' : ''}"
                        on:click={() => (selected = lp)}>
                  <span class="font-mono text-xs truncate">{lp.name}</span>
                  {#if conflictPins.has(lp.physical)}<span class="font-mono text-[9px] text-red-400">⚠</span>
                  {:else if pinRole(lp.physical)}<span class="font-mono text-[9px] text-amber-300">{roleAbbr(pinRole(lp.physical))}</span>
                  {:else if lp.bcm !== undefined && lp.active}<span class="font-mono text-[9px] opacity-80">{dirGlyph(lp)}{lp.level === 1 ? 'HI' : 'LO'}</span>{/if}
                </button>
                <div class="flex items-center justify-center gap-1 px-1">
                  <span class="font-mono text-[10px] text-zinc-600 w-4 text-center">{lp.physical}</span>
                  <span class="w-1.5 h-1.5 rounded-full bg-ink-600"></span>
                  <span class="font-mono text-[10px] text-zinc-600 w-4 text-center">{rp.physical}</span>
                </div>
                <button class="flex items-center justify-start gap-2 px-2 py-1 rounded border text-left transition-colors {pinClass(rp)} {pinRing(rp.physical)} {selected?.physical === rp.physical ? 'outline outline-1 outline-cursed-300' : ''}"
                        on:click={() => (selected = rp)}>
                  {#if conflictPins.has(rp.physical)}<span class="font-mono text-[9px] text-red-400">⚠</span>
                  {:else if pinRole(rp.physical)}<span class="font-mono text-[9px] text-amber-300">{roleAbbr(pinRole(rp.physical))}</span>
                  {:else if rp.bcm !== undefined && rp.active}<span class="font-mono text-[9px] opacity-80">{dirGlyph(rp)}{rp.level === 1 ? 'HI' : 'LO'}</span>{/if}
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
                {#if selected.consumer}<span class="text-zinc-400">owner: <span class="text-cursed-200">{selected.consumer}</span></span>{/if}
              {/if}
              {#each pinUsers(selected.physical) as u}<span class="{conflictPins.has(selected.physical) ? 'text-red-300' : 'text-amber-300'}">{u.name}: {u.role}</span>{/each}
              {#if selected.bus}<span class="text-zinc-500">{selected.bus}</span>{/if}
            </div>
          {:else}
            <p class="border-t border-ink-700 pt-3 text-[11px] text-zinc-600">Click a pin for its live state + how each stacked HAT uses it. ⚠ = a collision. Pin control + I2C/SPI arrive with v97.</p>
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
                {#each io.usb as u}<div class="text-xs text-zinc-400 truncate"><span class="text-zinc-600 font-mono">{u.id}</span> {u.name}</div>{/each}
              </div>
            {/if}
          </section>

          <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
            <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">Buses &amp; camera</h2>
            <div class="space-y-1.5 text-xs font-mono">
              <div class="flex items-center gap-2"><span class="w-2 h-2 rounded-full {buses.i2c?.enabled ? 'bg-live-400' : 'bg-zinc-600'}"></span><span class="text-zinc-300 w-12">I2C</span><span class="text-zinc-500">{buses.i2c?.enabled ? (buses.i2c.devices ?? []).join(', ') : 'disabled (v97 — needs reboot)'}</span></div>
              <div class="flex items-center gap-2"><span class="w-2 h-2 rounded-full {buses.spi?.enabled ? 'bg-live-400' : 'bg-zinc-600'}"></span><span class="text-zinc-300 w-12">SPI</span><span class="text-zinc-500">{buses.spi?.enabled ? (buses.spi.devices ?? []).join(', ') : 'disabled (v97 — needs reboot)'}</span></div>
              <div class="flex items-center gap-2"><span class="w-2 h-2 rounded-full {buses.uart ? 'bg-live-400' : 'bg-zinc-600'}"></span><span class="text-zinc-300 w-12">UART</span><span class="text-zinc-500">{buses.uart ? 'serial console' : 'off'}</span></div>
              <div class="flex items-center gap-2"><span class="w-2 h-2 rounded-full {cam.csi?.detected ? 'bg-live-400' : 'bg-zinc-600'}"></span><span class="text-zinc-300 w-12">CSI cam</span><span class="text-zinc-500">{cam.csi?.detected ? 'attached' : cam.csi?.supported ? 'supported, none attached' : 'none (add a Pi camera + v97)'}</span></div>
            </div>
            <p class="text-[11px] text-zinc-600 pt-1 border-t border-ink-800">Live feed runs over the USB HDMI-capture device — tune it on the streamer. CSI is for an optional ribbon camera.</p>
          </section>
        </div>

        <p class="text-[11px] text-zinc-600 text-center">Read-only, refreshing every 4s. HAT data distilled from <span class="text-zinc-500">pinout.xyz (CC BY-SA 4.0)</span>. Control + I2C/SPI + CSI camera land with v97.</p>
      {/if}
    </div>
  </main>
</div>
