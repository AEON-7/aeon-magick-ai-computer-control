<script lang="ts">
  // WiFi setup wizard — first-boot AND ongoing management.
  //
  // First-boot flow (captive portal):
  //   - Pi has fallen back to its own `aeon-setup` AP because no known
  //     network is reachable
  //   - Client (phone or laptop) connects to aeon-setup
  //   - OS pops captive portal — directed here
  //   - User picks a nearby SSID + enters password, clicks Connect
  //   - Pi tries to associate; on success the AP transitions off and
  //     the Pi rejoins as a regular WiFi client
  //
  // Ongoing flow (already-configured device):
  //   - Authenticated user navigates here from the network page
  //   - Same UI, same backend; just changes the active WiFi

  import { onMount, onDestroy } from 'svelte';

  type ScanResult = {
    ssid: string;
    signal: number;       // 0-100
    security: string;     // "WPA2", "open", etc.
    in_use: boolean;
  };

  type WifiState = {
    connected_ssid: string | null;
    ap_mode: boolean;
    wifi_radio_on: boolean;
  };

  let networks: ScanResult[] = [];
  let state: WifiState | null = null;
  let loadingScan = false;
  let lastScan = 0;

  let selectedSsid = '';
  let manualSsid = '';
  let password = '';
  let busy = false;
  let result = '';
  let resultIsError = false;

  let pollTimer: ReturnType<typeof setInterval> | null = null;

  async function loadScan() {
    loadingScan = true;
    try {
      const r = await fetch('/api/wifi/scan', { credentials: 'same-origin' });
      if (r.ok) {
        const data = await r.json();
        networks = data.networks ?? [];
        lastScan = Date.now();
      }
    } catch (e) {
      console.warn('scan failed', e);
    } finally {
      loadingScan = false;
    }
  }

  async function loadState() {
    try {
      const r = await fetch('/api/wifi/state', { credentials: 'same-origin' });
      if (r.ok) state = await r.json();
    } catch (e) {
      console.warn('state failed', e);
    }
  }

  async function submit() {
    const ssid = (selectedSsid || manualSsid).trim();
    if (!ssid) {
      result = 'pick a network or enter an SSID';
      resultIsError = true;
      return;
    }
    busy = true;
    result = `connecting to "${ssid}"…`;
    resultIsError = false;
    try {
      const r = await fetch('/api/wifi/connect', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ssid, password }),
      });
      const data = await r.json().catch(() => ({}));
      if (r.ok && data.ok) {
        result = `✓ ${data.msg || 'connected'}. The Pi will close the setup AP momentarily; your device may need to rejoin your home network manually.`;
        resultIsError = false;
        password = '';
        // Refresh state after a beat to show the new association
        setTimeout(loadState, 2000);
      } else {
        throw new Error(data.err || `HTTP ${r.status}`);
      }
    } catch (e: any) {
      result = `✗ ${e?.message ?? 'connect failed'}`;
      resultIsError = true;
    } finally {
      busy = false;
    }
  }

  function signalIcon(n: number): string {
    if (n >= 70) return '▰▰▰▰';
    if (n >= 50) return '▰▰▰▱';
    if (n >= 30) return '▰▰▱▱';
    if (n >= 10) return '▰▱▱▱';
    return '▱▱▱▱';
  }

  onMount(() => {
    loadState();
    loadScan();
    // Refresh scan every 8 seconds so live signal changes are visible
    pollTimer = setInterval(loadScan, 8000);
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
  });
</script>

<div class="h-full overflow-auto p-6">
  <div class="max-w-2xl mx-auto space-y-6">
    <header class="space-y-1">
      <h1 class="text-cursed-400 font-mono text-lg tracking-widest">
        AEON MAGICK — WIFI SETUP
      </h1>
      <p class="text-zinc-400 text-sm">
        {#if state?.ap_mode}
          You're connected to this device's <code class="text-cursed-300">aeon-setup</code> AP.
          Pick a nearby WiFi network and provide its password to bring this
          device online. Once it associates, the AP will close.
        {:else if state?.connected_ssid}
          Currently connected to <code class="text-cursed-300">{state.connected_ssid}</code>.
          Pick a different network below to switch.
        {:else}
          WiFi radio is {state?.wifi_radio_on ? 'on' : 'off'} but no
          network is associated.
        {/if}
      </p>
    </header>

    <!-- Network list -->
    <section class="panel">
      <header class="flex items-center justify-between px-5 py-3 border-b border-steel-700">
        <h2 class="font-mono text-xs uppercase tracking-wider text-zinc-400">
          Nearby networks
          {#if loadingScan}
            <span class="text-zinc-500 normal-case ml-2">scanning…</span>
          {:else if lastScan > 0}
            <span class="text-zinc-500 normal-case ml-2">
              {networks.length} found
            </span>
          {/if}
        </h2>
        <button type="button" class="btn text-xs" on:click={loadScan} disabled={loadingScan}>
          {loadingScan ? '…' : '↻ rescan'}
        </button>
      </header>
      <div class="divide-y divide-ink-800 max-h-72 overflow-y-auto">
        {#each networks as n (n.ssid)}
          <label class="flex items-center gap-3 px-5 py-2.5 cursor-pointer
                        hover:bg-ink-800/60 transition-colors
                        {selectedSsid === n.ssid ? 'bg-cursed-900/30' : ''}">
            <input
              type="radio"
              bind:group={selectedSsid}
              value={n.ssid}
              class="w-4 h-4 accent-cursed-500"
            />
            <div class="flex-1 min-w-0">
              <div class="text-zinc-200 text-sm font-medium truncate">
                {n.ssid}
                {#if n.in_use}
                  <span class="text-live-400 text-[10px] ml-1">● connected</span>
                {/if}
              </div>
              <div class="text-[10px] font-mono text-zinc-500">
                {n.security}
              </div>
            </div>
            <span class="font-mono text-xs text-cursed-300">
              {signalIcon(n.signal)}
            </span>
            <span class="text-[10px] font-mono text-zinc-500 w-8 text-right">
              {n.signal}%
            </span>
          </label>
        {:else}
          {#if !loadingScan}
            <p class="px-5 py-6 text-center text-zinc-500 text-sm">
              No networks visible yet. Wait a few seconds and try
              <button type="button" class="text-cursed-400 underline"
                      on:click={loadScan}>rescan</button>.
            </p>
          {/if}
        {/each}
      </div>
    </section>

    <!-- Connect form -->
    <form
      on:submit|preventDefault={submit}
      class="panel p-5 space-y-4"
    >
      <div class="space-y-1">
        <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="manual-ssid">
          SSID
          {#if selectedSsid}
            <span class="normal-case text-zinc-400 ml-1">
              (using <code class="text-cursed-300">{selectedSsid}</code> from list above)
            </span>
          {/if}
        </label>
        <input
          id="manual-ssid"
          type="text"
          bind:value={manualSsid}
          placeholder="or type an SSID manually (e.g. hidden network)"
          disabled={!!selectedSsid}
          class="w-full px-3 py-2 rounded-md bg-ink-800 border border-steel-700
                 disabled:opacity-50 disabled:cursor-not-allowed
                 focus:outline-none focus:ring-1 focus:ring-cursed-500/50"
        />
      </div>

      <div class="space-y-1">
        <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="wifi-password">
          password
          <span class="normal-case text-zinc-500 ml-1">
            (leave blank for open networks)
          </span>
        </label>
        <input
          id="wifi-password"
          type="password"
          bind:value={password}
          autocomplete="new-password"
          class="w-full px-3 py-2 rounded-md bg-ink-800 border border-steel-700
                 focus:outline-none focus:ring-1 focus:ring-cursed-500/50"
        />
      </div>

      <button
        type="submit"
        disabled={busy || (!selectedSsid && !manualSsid.trim())}
        class="btn-primary w-full disabled:opacity-50 disabled:cursor-not-allowed"
      >
        {busy ? 'connecting…' : 'connect'}
      </button>

      {#if result}
        <p class="text-sm {resultIsError ? 'text-red-400' : 'text-live-400'} break-words">
          {result}
        </p>
      {/if}
    </form>

    <p class="text-xs text-zinc-500 text-center">
      After a successful connection, the setup AP will close and the
      device's web UI will become reachable at
      <code>https://aeon-magick.local/</code> (or the IP it got from your
      router's DHCP).
    </p>
  </div>
</div>
