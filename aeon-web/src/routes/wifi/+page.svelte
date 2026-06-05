<script lang="ts">
  // WiFi management page (v63+).
  //
  // Three operating modes the user can pick between, with one panel
  // per mode underneath:
  //
  //   Client mode   — scan + connect + saved-network management.
  //                   The most common deployment: Pi joins your home
  //                   network and serves the web UI over it.
  //
  //   Access Point  — Pi BECOMES a WiFi network. SSID + password
  //                   set here. THESE SAME CREDENTIALS are used by
  //                   the boot-fallback AP (the captive portal that
  //                   spins up when no known network is reachable),
  //                   so changing them affects both.
  //
  //   Disabled      — radio off. For ethernet-only deployments where
  //                   the operator doesn't want a beacon visible at
  //                   all.
  //
  // The /setup/wifi page (captive-portal first-boot flow) is a
  // separate, simpler version of the client panel — this page is the
  // authenticated full-feature equivalent for ongoing management.

  import { onMount, onDestroy } from 'svelte';

  type Mode = 'client' | 'ap' | 'off';
  let mode: Mode = 'client';

  // ── Live state ──
  let state: {
    connected_ssid: string | null;
    ap_mode: boolean;
    wifi_radio_on: boolean;
  } | null = null;
  let scanned: Array<{
    ssid: string;
    signal: number;
    security: string;
    in_use: boolean;
  }> = [];
  let known: Array<{
    profile: string;
    ssid: string;
    autoconnect: boolean;
    priority: number;
    mode: string;
    active: boolean;
  }> = [];
  let ap: {
    ssid: string;
    has_password: boolean;
    active: boolean;
    default_ssid: string;
    default_password: string;
  } | null = null;

  // ── Form state ──
  // Client mode "connect to new network" form.
  let connectSsid = '';
  let connectPsk = '';
  let connecting = false;

  // AP mode "edit credentials" form.
  let apSsid = '';
  let apPassword = '';
  let apActivateNow = false;
  let apSaving = false;

  let loading = true;
  let error = '';
  let msg = '';
  let scanning = false;
  let poll: ReturnType<typeof setInterval>;

  async function refresh() {
    error = '';
    try {
      const [stateRes, knownRes, apRes] = await Promise.all([
        fetch('/api/wifi/state', { credentials: 'same-origin' }).then(r => r.json()),
        fetch('/api/wifi/known', { credentials: 'same-origin' }).then(r => r.json()),
        fetch('/api/wifi/ap', { credentials: 'same-origin' }).then(r => r.json()),
      ]);
      state = stateRes;
      known = knownRes.networks ?? [];
      ap = apRes;

      // Infer current mode from the live state. The user can override
      // (e.g. they're in client mode but want to switch to AP), but
      // refresh()'s job is to reflect reality.
      if (state) {
        if (!state.wifi_radio_on) mode = 'off';
        else if (state.ap_mode) mode = 'ap';
        else mode = 'client';
      }

      // Pre-populate AP form fields from the saved profile so the user
      // sees what's currently set (password stays as a placeholder
      // marker — we never echo it).
      if (ap && !apSaving) {
        apSsid = ap.ssid;
      }
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
    } finally {
      loading = false;
    }
  }

  let manualSsid = '';

  async function scan() {
    scanning = true;
    try {
      const r = await fetch('/api/wifi/scan', { credentials: 'same-origin' }).then(r => r.json());
      scanned = r.networks ?? [];
    } catch (e: any) {
      error = e?.message ?? 'scan failed';
    } finally {
      scanning = false;
    }
  }

  async function connectTo(ssid: string, security: string) {
    connectSsid = ssid;
    if (security === 'open') {
      // No password required.
      connectPsk = '';
      await doConnect();
    } else {
      // Show the password input by selecting this SSID into the form;
      // user types the PSK and clicks Connect.
      const el = document.getElementById('connect-psk');
      if (el) (el as HTMLInputElement).focus();
    }
  }

  async function doConnect() {
    if (!connectSsid.trim()) {
      error = 'pick a network first';
      return;
    }
    connecting = true;
    error = '';
    msg = `connecting to "${connectSsid}"…`;
    try {
      const r = await fetch('/api/wifi/connect', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({ ssid: connectSsid, password: connectPsk }),
      }).then(r => r.json());
      if (r.ok) {
        msg = `✓ ${r.msg ?? 'connected'}`;
        connectSsid = '';
        connectPsk = '';
        await refresh();
      } else {
        error = `connect failed: ${r.err}`;
        msg = '';
      }
    } catch (e: any) {
      error = e?.message ?? 'connect failed';
      msg = '';
    } finally {
      connecting = false;
    }
  }

  // Curried form so we can pass it directly to on:change without
  // inline arrow + cast (which the Svelte parser chokes on for
  // event.target typing).
  function onAutoconnectToggle(profile: string) {
    return (e: Event) => {
      const target = e.target as HTMLInputElement;
      void setAutoconnect(profile, target.checked);
    };
  }
  async function setAutoconnect(profile: string, enabled: boolean) {
    try {
      const r = await fetch('/api/wifi/autoconnect', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({ profile, enabled }),
      }).then(r => r.json());
      if (r.ok) {
        msg = `✓ ${profile}: autoconnect = ${enabled ? 'on' : 'off'}`;
        await refresh();
      } else {
        error = r.err ?? 'update failed';
      }
    } catch (e: any) {
      error = e?.message ?? 'update failed';
    }
  }

  async function forget(profile: string) {
    if (!confirm(`Forget "${profile}"? Saved password will be deleted.`)) return;
    try {
      const r = await fetch('/api/wifi/known', {
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({ profile }),
      }).then(r => r.json());
      if (r.ok) {
        msg = `✓ forgot ${profile}`;
        await refresh();
      } else {
        error = r.err ?? 'forget failed';
      }
    } catch (e: any) {
      error = e?.message ?? 'forget failed';
    }
  }

  async function saveAp() {
    if (!apSsid.trim()) {
      error = 'AP SSID required';
      return;
    }
    if (apPassword && apPassword.length < 8) {
      error = 'AP password must be at least 8 characters (WPA2 requirement)';
      return;
    }
    if (
      apActivateNow &&
      !confirm(
        'Activating AP mode now will tear down the current client connection. ' +
        'If you are accessing this UI over WiFi you will lose your connection — ' +
        'use ethernet or USB to reach the device until you reconnect to the new AP.',
      )
    ) {
      return;
    }
    apSaving = true;
    error = '';
    try {
      const body: any = { ssid: apSsid, activate: apActivateNow };
      // Empty password = "leave the existing one alone" if we have one,
      // or "open AP" if we don't. UI surfaces this in the form copy.
      if (apPassword) body.password = apPassword;
      const r = await fetch('/api/wifi/ap', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify(body),
      }).then(r => r.json());
      if (r.ok) {
        msg = '✓ AP credentials saved';
        apPassword = '';
        await refresh();
      } else {
        error = r.err ?? 'save failed';
      }
    } catch (e: any) {
      error = e?.message ?? 'save failed';
    } finally {
      apSaving = false;
    }
  }

  async function setRadio(on: boolean) {
    if (
      !on &&
      !confirm(
        'Turn the WiFi radio off entirely?\n\n' +
        'You will lose any active WiFi connection AND the no-internet ' +
        'boot-fallback AP will not work either — reach the Pi via ethernet ' +
        'or USB only.',
      )
    ) {
      return;
    }
    try {
      const r = await fetch('/api/wifi/radio', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({ on }),
      }).then(r => r.json());
      if (r.ok) {
        msg = on ? '✓ WiFi radio on' : '✓ WiFi radio off';
        await refresh();
      } else {
        error = r.err ?? 'radio toggle failed';
      }
    } catch (e: any) {
      error = e?.message ?? 'radio toggle failed';
    }
  }

  onMount(() => {
    refresh();
    scan();
    // Poll every 8s so signal-strength on the scan list stays fresh
    // and "currently connected" indicator updates when the device
    // auto-reconnects after a transient drop.
    poll = setInterval(() => {
      refresh();
      if (mode === 'client') scan();
    }, 8000);
  });
  onDestroy(() => {
    if (poll) clearInterval(poll);
  });

  function signalBars(s: number): string {
    if (s >= 75) return '▰▰▰▰';
    if (s >= 55) return '▰▰▰▱';
    if (s >= 35) return '▰▰▱▱';
    if (s >= 15) return '▰▱▱▱';
    return '▱▱▱▱';
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← AEON MAGICK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">WiFi</span>
    </div>
  </header>

  <div class="flex-1 overflow-y-auto p-6 max-w-4xl w-full mx-auto space-y-6">

    {#if loading}
      <p class="text-zinc-400">loading…</p>
    {:else}

      <!-- Top status pill -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-4 space-y-2">
        <div class="flex items-center gap-3 flex-wrap">
          <span class="font-mono text-[10px] uppercase tracking-wider text-zinc-500">
            Current
          </span>
          {#if !state?.wifi_radio_on}
            <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                         bg-zinc-700/40 border border-zinc-600 text-zinc-300
                         text-[11px] font-mono">
              <span class="h-1.5 w-1.5 rounded-full bg-zinc-500"></span>
              radio off
            </span>
          {:else if state?.ap_mode}
            <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                         bg-cursed-500/20 border border-cursed-500/50 text-cursed-200
                         text-[11px] font-mono">
              <span class="h-1.5 w-1.5 rounded-full bg-cursed-400 animate-pulse"></span>
              access point: {ap?.ssid ?? 'aeon-setup'}
            </span>
          {:else if state?.connected_ssid}
            <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                         bg-live-500/20 border border-live-500/50 text-live-200
                         text-[11px] font-mono">
              <span class="h-1.5 w-1.5 rounded-full bg-live-400 animate-pulse"></span>
              connected: {state.connected_ssid}
            </span>
          {:else}
            <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full
                         bg-amber-500/20 border border-amber-500/50 text-amber-200
                         text-[11px] font-mono">
              <span class="h-1.5 w-1.5 rounded-full bg-amber-400"></span>
              radio on, not connected
            </span>
          {/if}
        </div>
      </section>

      {#if error}
        <div class="p-3 rounded-lg border border-red-500/40 bg-red-500/10 text-red-200 text-sm">
          {error}
        </div>
      {/if}
      {#if msg}
        <div class="p-3 rounded-lg border border-live-500/40 bg-live-500/10 text-live-200 text-sm">
          {msg}
        </div>
      {/if}

      <!-- Mode selector -->
      <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
        <h3 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
          Operating mode
        </h3>
        <div class="grid grid-cols-1 sm:grid-cols-3 gap-2">
          <label class="flex items-start gap-3 cursor-pointer p-3 rounded border
                        transition-colors
                        {mode === 'client'
                          ? 'bg-cursed-500/10 border-cursed-500/50'
                          : 'bg-ink-950/40 border-ink-800 hover:border-ink-700'}">
            <input type="radio" bind:group={mode} value="client"
                   on:change={() => setRadio(true)}
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1 min-w-0">
              <div class="text-zinc-200 text-sm font-medium">WiFi client</div>
              <p class="text-[11px] text-zinc-500 leading-relaxed">
                Pi joins an existing network. Default deployment.
              </p>
            </div>
          </label>

          <label class="flex items-start gap-3 cursor-pointer p-3 rounded border
                        transition-colors
                        {mode === 'ap'
                          ? 'bg-cursed-500/10 border-cursed-500/50'
                          : 'bg-ink-950/40 border-ink-800 hover:border-ink-700'}">
            <input type="radio" bind:group={mode} value="ap"
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1 min-w-0">
              <div class="text-zinc-200 text-sm font-medium">Access point</div>
              <p class="text-[11px] text-zinc-500 leading-relaxed">
                Pi broadcasts its own SSID — phones/laptops connect to it.
              </p>
            </div>
          </label>

          <label class="flex items-start gap-3 cursor-pointer p-3 rounded border
                        transition-colors
                        {mode === 'off'
                          ? 'bg-cursed-500/10 border-cursed-500/50'
                          : 'bg-ink-950/40 border-ink-800 hover:border-ink-700'}">
            <input type="radio" bind:group={mode} value="off"
                   on:change={() => setRadio(false)}
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1 min-w-0">
              <div class="text-zinc-200 text-sm font-medium">Disable radio</div>
              <p class="text-[11px] text-zinc-500 leading-relaxed">
                No WiFi beacon at all — ethernet/USB only.
              </p>
            </div>
          </label>
        </div>
      </section>

      <!-- Client mode panel -->
      {#if mode === 'client'}
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
          <header class="flex items-center justify-between">
            <h3 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              Connect to a network
            </h3>
            <button class="btn text-xs" on:click={scan} disabled={scanning}>
              {scanning ? 'scanning…' : '⟳ rescan'}
            </button>
          </header>

          <div class="space-y-1.5 max-h-72 overflow-y-auto pr-1">
            {#each scanned as n}
              <button type="button" on:click={() => connectTo(n.ssid, n.security)}
                      class="w-full text-left flex items-center gap-3 p-2 rounded
                             border {n.in_use
                               ? 'bg-live-500/10 border-live-500/30'
                               : 'bg-ink-950/40 border-ink-800 hover:border-ink-700'}">
                <span class="font-mono text-zinc-500 text-[11px] w-8 tabular-nums">{n.signal}</span>
                <span class="font-mono text-cursed-300 text-xs w-12">{signalBars(n.signal)}</span>
                <span class="text-zinc-200 text-sm flex-1 truncate">{n.ssid}</span>
                <span class="text-[10px] font-mono text-zinc-500 uppercase">{n.security}</span>
                {#if n.in_use}
                  <span class="text-[10px] font-mono text-live-300">● connected</span>
                {/if}
              </button>
            {/each}
            {#if scanned.length === 0}
              <p class="text-zinc-500 text-xs italic">no networks scanned yet — click "rescan"</p>
            {/if}
          </div>

          <!-- Manual entry: join a network the scan can't see — e.g. while the Orb
               is hosting its own setup AP, where one radio can't scan AND host. -->
          <div class="space-y-1.5 pt-2 border-t border-ink-800">
            <div class="flex gap-2">
              <input type="text" bind:value={manualSsid} autocomplete="off"
                     placeholder="…or type a network name (SSID)"
                     on:keydown={(e) => { if (e.key === 'Enter' && manualSsid.trim()) connectSsid = manualSsid.trim(); }}
                     class="flex-1 bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
              <button class="btn text-xs"
                      on:click={() => { if (manualSsid.trim()) connectSsid = manualSsid.trim(); }}
                      disabled={!manualSsid.trim()}>
                enter →
              </button>
            </div>
            <p class="text-[11px] text-zinc-500 leading-relaxed">
              The scan is empty while the Orb broadcasts its own <strong>setup AP</strong>
              (one radio can't scan + host at once). Type the nearby network's name to join
              it — <strong>joining drops this page</strong> as the Orb switches off the AP;
              reconnect your device to that network and find the Orb at
              <code>aeon-magick.local</code> (or its new IP from your router).
            </p>
          </div>

          <!-- Connect-with-password form (becomes visible once a network is picked) -->
          {#if connectSsid}
            <div class="space-y-2 p-3 rounded border border-cursed-500/40 bg-cursed-500/5">
              <p class="text-xs uppercase tracking-wider text-cursed-300">
                Connect to "{connectSsid}"
              </p>
              <input id="connect-psk" type="password" bind:value={connectPsk}
                     placeholder="Password (leave blank for open networks)"
                     autocomplete="off"
                     class="w-full bg-ink-800 border border-ink-700 rounded
                            px-3 py-1.5 text-sm text-zinc-200 font-mono" />
              <div class="flex gap-2">
                <button class="btn-primary text-xs" on:click={doConnect} disabled={connecting}>
                  {connecting ? 'connecting…' : 'Connect'}
                </button>
                <button class="btn text-xs"
                        on:click={() => { connectSsid = ''; connectPsk = ''; }}
                        disabled={connecting}>
                  cancel
                </button>
              </div>
            </div>
          {/if}
        </section>

        <!-- Known / saved networks -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <h3 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Saved networks ({known.length})
          </h3>
          <p class="text-[11px] text-zinc-500 leading-relaxed">
            Auto-connect controls whether NetworkManager re-joins this network
            on its own when it's in range. Turn it off if you only want to
            connect manually. Forget removes the saved password entirely.
          </p>
          <div class="space-y-1.5">
            {#each known as n}
              <div class="flex items-center gap-3 p-2 rounded
                          {n.active
                            ? 'bg-live-500/10 border border-live-500/30'
                            : 'bg-ink-950/40 border border-ink-800'}">
                <span class="text-zinc-200 text-sm flex-1 truncate">{n.ssid}</span>
                {#if n.active}
                  <span class="text-[10px] font-mono text-live-300 px-1.5">● active</span>
                {/if}
                <label class="flex items-center gap-1.5 cursor-pointer text-[11px] text-zinc-400">
                  <input type="checkbox" checked={n.autoconnect}
                         on:change={onAutoconnectToggle(n.profile)}
                         class="w-3 h-3 accent-cursed-500" />
                  auto-connect
                </label>
                <button class="text-[11px] px-2 py-0.5 rounded border
                               border-red-500/40 text-red-300
                               hover:bg-red-500/10 hover:text-red-200"
                        on:click={() => forget(n.profile)}>
                  forget
                </button>
              </div>
            {/each}
            {#if known.length === 0}
              <p class="text-zinc-500 text-xs italic">no saved networks yet</p>
            {/if}
          </div>
        </section>
      {/if}

      <!-- AP mode panel -->
      {#if mode === 'ap'}
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-4">
          <header class="space-y-1">
            <h3 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
              Access point credentials
            </h3>
            <p class="text-xs text-zinc-400 leading-relaxed">
              These SSID and password are used BOTH for:
            </p>
            <ul class="ml-4 list-disc text-xs text-zinc-400 leading-relaxed space-y-0.5">
              <li>The AP you broadcast when "Access point" is the active mode.</li>
              <li>
                The no-internet boot-fallback AP that aeon-netwatch spins up
                if no known client network is reachable within 90 seconds
                of boot.
              </li>
            </ul>
            {#if ap}
              <p class="text-[11px] text-zinc-500 leading-relaxed mt-2">
                Shipped defaults are <code class="text-cursed-300">{ap.default_ssid}</code>
                / <code class="text-cursed-300">{ap.default_password}</code> —
                change them before exposing the device on a network you
                don't physically control.
              </p>
            {/if}
          </header>

          <div class="space-y-2">
            <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="ap-ssid">
              AP SSID
            </label>
            <input id="ap-ssid" type="text" bind:value={apSsid}
                   placeholder="aeon-setup"
                   class="w-full bg-ink-800 border border-ink-700 rounded
                          px-3 py-1.5 text-sm text-zinc-200 font-mono" />
          </div>

          <div class="space-y-2">
            <label class="text-xs uppercase tracking-wider text-zinc-500 block" for="ap-pw">
              AP password
              {#if ap?.has_password}
                <span class="text-cursed-400 normal-case ml-1 text-[10px]">
                  (one saved — leave blank to keep it)
                </span>
              {/if}
            </label>
            <input id="ap-pw" type="password" bind:value={apPassword}
                   placeholder="≥ 8 characters"
                   autocomplete="off"
                   class="w-full bg-ink-800 border border-ink-700 rounded
                          px-3 py-1.5 text-sm text-zinc-200 font-mono" />
          </div>

          <label class="flex items-start gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={apActivateNow}
                   class="mt-1 w-4 h-4 accent-cursed-500" />
            <div class="space-y-1">
              <div class="text-zinc-200 text-sm font-medium">
                Switch to AP mode now
              </div>
              <p class="text-xs text-zinc-500 leading-relaxed">
                Activate the AP profile immediately. Tears down any active
                client WiFi connection — only safe if you're managing the Pi
                over ethernet/USB right now.
              </p>
            </div>
          </label>

          <button class="btn-primary text-sm" on:click={saveAp} disabled={apSaving}>
            {apSaving ? 'saving…' : 'Save AP settings'}
          </button>
        </section>
      {/if}

      <!-- Off mode panel -->
      {#if mode === 'off'}
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-2">
          <h3 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Radio disabled
          </h3>
          <p class="text-sm text-zinc-400 leading-relaxed">
            WiFi is currently turned off entirely. The Pi is reachable only
            via ethernet, the USB ethernet gadget, or Tailscale.
          </p>
          <p class="text-xs text-zinc-500 leading-relaxed">
            Re-enable by picking <em>WiFi client</em> or <em>Access point</em>
            above. Saved networks and the AP profile are preserved while the
            radio is off.
          </p>
        </section>
      {/if}

    {/if}
  </div>
</div>
