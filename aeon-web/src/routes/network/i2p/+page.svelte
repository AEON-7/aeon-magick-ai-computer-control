<script lang="ts">
  // I2P config + diagnostics (v57+).
  //
  // The /network page has a small "I2P config →" button that lands
  // here when the user picks the i2p VPN provider. This page shows
  // what the i2pd router is actually doing right now, surfaces the
  // browser-proxy URLs in a copy-pasteable form, and links to the
  // i2pd web console so the user can manage tunnels / address book.
  //
  // The actual outproxy + VPN provider toggle still lives on
  // /network (under the VPN section) — this page is read-mostly.

  import PageHeader from '$lib/components/PageHeader.svelte';
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let status: api.I2pStatus | null = null;
  let loading = true;
  let error = '';
  let poll: ReturnType<typeof setInterval>;

  async function refresh() {
    try {
      status = await api.getI2pStatus();
      loading = false;
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
      loading = false;
    }
  }

  onMount(() => {
    refresh();
    poll = setInterval(refresh, 5000);
  });
  onDestroy(() => { if (poll) clearInterval(poll); });

  function copy(text: string) {
    navigator.clipboard?.writeText(text);
  }
</script>

<div class="h-full flex flex-col">
  <PageHeader title="I2P config" backHref="/network" backLabel="NETWORK" />

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-3xl mx-auto w-full space-y-6">

      {#if loading}<p class="text-zinc-500 text-sm">loading…</p>{/if}
      {#if error}<p class="text-red-400 text-sm">{error}</p>{/if}

      {#if status}
        <!-- ── Why this page exists ── -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-2">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            About I2P access
          </h2>
          <p class="text-xs text-zinc-400 leading-relaxed">
            I2P is fundamentally different from Tor — there's no
            transparent-TCP path for it. Browsers reach
            <code class="text-cursed-300">.i2p</code> sites by talking
            to the I2P daemon's HTTP proxy, which does the address-book
            lookup + tunnel building. So unlike <code class="text-cursed-300">.onion</code>
            (which works in any browser once Tor's DNS is hooked up),
            <code class="text-cursed-300">.i2p</code> requires you to
            point your browser at I2P's proxy explicitly.
          </p>
          <p class="text-xs text-zinc-400 leading-relaxed">
            The proxy URLs below are bound on the USB-connected
            network (when USB ethernet is on) so any device plugged
            into the Pi can use them — set them once in your browser's
            proxy settings and <code class="text-cursed-300">.i2p</code>
            sites Just Work.
          </p>
        </section>

        <!-- ── Status ── -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Daemon status
          </h2>
          <div class="grid grid-cols-2 gap-3">
            <div class="p-3 rounded bg-ink-950/40 border border-ink-800">
              <p class="text-[10px] uppercase tracking-wider text-zinc-500">i2pd installed</p>
              <p class="text-base mt-1 {status.installed ? 'text-live-400' : 'text-red-400'}">
                {status.installed ? '✓ yes' : '✗ no — install via apt'}
              </p>
            </div>
            <div class="p-3 rounded bg-ink-950/40 border border-ink-800">
              <p class="text-[10px] uppercase tracking-wider text-zinc-500">service running</p>
              <p class="text-base mt-1 {status.service_active ? 'text-live-400' : 'text-amber-400'}">
                {status.service_active ? '✓ active' : '⚠ not running — pick I2P as VPN provider'}
              </p>
            </div>
          </div>
          {#if !status.installed}
            <p class="text-xs text-amber-300 leading-relaxed pt-2 border-t border-ink-800">
              The <code>i2pd</code> binary isn't on this system. Install it via:
              <code class="block mt-1 p-2 bg-ink-950 rounded font-mono text-zinc-300">
                sudo apt install i2pd
              </code>
              Then come back here and pick I2P as the VPN provider on
              <a href="/network" class="text-cursed-300 hover:underline">/network</a>.
            </p>
          {/if}
        </section>

        <!-- ── Browser proxy setup ── -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Browser proxy
          </h2>
          <p class="text-xs text-zinc-500">
            Configure your browser to send requests through this HTTP
            proxy. Most browsers: Settings → Network/Proxy → "Manual
            proxy configuration" → HTTP proxy.
          </p>
          <div class="space-y-2">
            <div class="flex items-center gap-2 p-3 rounded bg-ink-950 border border-ink-800">
              <span class="text-[10px] uppercase tracking-wider text-zinc-500 w-24 shrink-0">HTTP proxy</span>
              <code class="text-zinc-200 font-mono text-sm flex-1 truncate">
                {status.http_proxy.addr || '127.0.0.1'}:{status.http_proxy.port}
              </code>
              <button class="text-[10px] px-2 py-1 rounded border border-cursed-500/40
                             text-cursed-300 hover:bg-cursed-500/10"
                      on:click={() => copy(`${status?.http_proxy.addr}:${status?.http_proxy.port}`)}>
                copy
              </button>
            </div>
            <div class="flex items-center gap-2 p-3 rounded bg-ink-950 border border-ink-800">
              <span class="text-[10px] uppercase tracking-wider text-zinc-500 w-24 shrink-0">SOCKS proxy</span>
              <code class="text-zinc-200 font-mono text-sm flex-1 truncate">
                {status.socks_proxy.addr || '127.0.0.1'}:{status.socks_proxy.port}
              </code>
              <button class="text-[10px] px-2 py-1 rounded border border-cursed-500/40
                             text-cursed-300 hover:bg-cursed-500/10"
                      on:click={() => copy(`${status?.socks_proxy.addr}:${status?.socks_proxy.port}`)}>
                copy
              </button>
            </div>
          </div>
          <p class="text-[11px] text-zinc-500 leading-relaxed">
            Test it: with the proxy configured, visit
            <code class="text-cursed-300">http://stats.i2p/</code> or
            <code class="text-cursed-300">http://identiguy.i2p/</code>.
            If they load, you're routed through I2P.
          </p>
          <p class="text-[11px] text-zinc-500 leading-relaxed">
            <strong>Important:</strong> set <code class="text-cursed-300">network.dns.blockDotOnion</code>
            and equivalent flags to <em>false</em> in your browser if
            it refuses to resolve <code>.i2p</code> addresses. Firefox's
            about:config has this; Chromium browsers usually permit
            them by default once a proxy is configured.
          </p>
        </section>

        <!-- ── Router console ── -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Router console
          </h2>
          <p class="text-xs text-zinc-500">
            i2pd's web admin — tunnels, peers, address book, bandwidth.
            Open this from the Pi or from any USB-connected device.
          </p>
          <div class="flex items-center gap-2 p-3 rounded bg-ink-950 border border-ink-800">
            <span class="text-[10px] uppercase tracking-wider text-zinc-500 w-24 shrink-0">URL</span>
            <code class="text-zinc-200 font-mono text-sm flex-1 truncate">
              {status.browser_hint.console_url}
            </code>
            <a class="text-[10px] px-2 py-1 rounded border border-cursed-500/40
                      text-cursed-300 hover:bg-cursed-500/10"
               href={status.browser_hint.console_url} target="_blank" rel="noreferrer">
              open
            </a>
          </div>
        </section>

        <!-- ── Outproxy ── -->
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-300">
            Clearnet outproxy
          </h2>
          <p class="text-xs text-zinc-500">
            By default I2P stays inside the network — only
            <code class="text-cursed-300">.i2p</code> sites work. To
            also tunnel <em>clearnet</em> requests (regular websites
            via an I2P exit), set an outproxy. Configure via the
            VPN section on <a href="/network" class="text-cursed-300 hover:underline">/network</a>;
            shown here for status only.
          </p>
          {#if status.outproxy}
            <div class="flex items-center gap-2 p-3 rounded bg-ink-950 border border-ink-800">
              <span class="text-[10px] uppercase tracking-wider text-zinc-500 w-24 shrink-0">outproxy</span>
              <code class="text-zinc-200 font-mono text-sm flex-1 truncate">{status.outproxy}</code>
            </div>
          {:else}
            <p class="text-xs text-zinc-500 italic">
              No outproxy configured — I2P-only mode. This is the
              recommended default unless you specifically need
              clearnet exit through I2P.
            </p>
          {/if}
        </section>

        <!-- ── Footer note ── -->
        <p class="text-[11px] text-zinc-600 leading-relaxed">
          I2P-vs-Tor: I2P is a peer-to-peer "darknet" — its content
          lives inside the network itself; you're not anonymizing
          clearnet traffic unless you set an outproxy. Tor is more
          of an anonymizing tunnel to the regular internet, with
          <code>.onion</code> hidden services as a bonus. You can run
          both at once; they have different threat models.
        </p>
      {/if}
    </div>
  </main>
</div>
