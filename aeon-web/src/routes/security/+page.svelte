<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let m: api.SecurityMetrics | null = null;
  let error = '';
  let poll_iv: ReturnType<typeof setInterval>;

  async function refresh() {
    try {
      m = await api.getSecurityMetrics();
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
    }
  }

  onMount(() => {
    refresh();
    // 5s poll keeps the Pi cool — each request is one /sys file read
    // per iface + one iptables -nvL invocation.
    poll_iv = setInterval(refresh, 5000);
  });
  onDestroy(() => { if (poll_iv) clearInterval(poll_iv); });

  function fmtBps(bps: number): string {
    if (bps < 1_000) return `${bps} bps`;
    if (bps < 1_000_000) return `${(bps / 1_000).toFixed(1)} kbps`;
    if (bps < 1_000_000_000) return `${(bps / 1_000_000).toFixed(2)} Mbps`;
    return `${(bps / 1_000_000_000).toFixed(2)} Gbps`;
  }

  function fmtBytes(b: number): string {
    if (b < 1_000) return `${b} B`;
    if (b < 1_000_000) return `${(b / 1_000).toFixed(1)} kB`;
    if (b < 1_000_000_000) return `${(b / 1_000_000).toFixed(1)} MB`;
    return `${(b / 1_000_000_000).toFixed(2)} GB`;
  }

  // Build SVG path for a sparkline. Keep it ASCII and small — Pi has
  // limited compute, so we just compute Y per sample on the client.
  function sparkline(series: { ts_ms: number; in_bps: number; out_bps: number }[],
                     accessor: (s: typeof series[number]) => number,
                     w = 280, h = 60): string {
    if (series.length < 2) return '';
    const values = series.map(accessor);
    const max = Math.max(...values, 1);
    const min = 0;
    const xStep = w / (series.length - 1);
    const yScale = (v: number) => h - ((v - min) / (max - min)) * h;
    let d = `M0,${yScale(values[0]).toFixed(1)}`;
    for (let i = 1; i < values.length; i++) {
      d += ` L${(i * xStep).toFixed(1)},${yScale(values[i]).toFixed(1)}`;
    }
    return d;
  }

  $: history = m?.throughput_history ?? [];
  $: maxBps = Math.max(
    ...history.map(h => h.in_bps),
    ...history.map(h => h.out_bps),
    1
  );
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">
        ← AEON MAGICK
      </a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">security console</span>
    </div>
  </header>

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-5xl mx-auto w-full space-y-6">
    {#if error}<p class="text-red-400 text-sm">{error}</p>{/if}

    {#if m}
      <!-- Throughput at-a-glance -->
      <section class="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div class="bg-ink-900 border border-ink-700 rounded-xl p-5">
          <div class="flex items-center justify-between mb-2">
            <span class="text-[10px] uppercase tracking-wider text-zinc-500">Inbound (WAN → Pi)</span>
            <span class="text-[10px] font-mono text-cursed-400">live</span>
          </div>
          <div class="text-3xl font-mono text-live-400">{fmtBps(m.throughput_bps.in)}</div>
          <svg viewBox="0 0 280 60" class="w-full h-16 mt-2">
            <defs>
              <linearGradient id="grad-in" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stop-color="rgb(110,231,183)" stop-opacity="0.4"/>
                <stop offset="100%" stop-color="rgb(110,231,183)" stop-opacity="0"/>
              </linearGradient>
            </defs>
            {#if history.length >= 2}
              {@const path = sparkline(history, h => h.in_bps)}
              {@const fill = path + ` L280,60 L0,60 Z`}
              <path d={fill} fill="url(#grad-in)" />
              <path d={path} stroke="rgb(110,231,183)" stroke-width="1.5" fill="none" />
            {/if}
          </svg>
        </div>
        <div class="bg-ink-900 border border-ink-700 rounded-xl p-5">
          <div class="flex items-center justify-between mb-2">
            <span class="text-[10px] uppercase tracking-wider text-zinc-500">Outbound (Pi → WAN)</span>
            <span class="text-[10px] font-mono text-cursed-400">live</span>
          </div>
          <div class="text-3xl font-mono text-cursed-300">{fmtBps(m.throughput_bps.out)}</div>
          <svg viewBox="0 0 280 60" class="w-full h-16 mt-2">
            <defs>
              <linearGradient id="grad-out" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stop-color="rgb(217,70,239)" stop-opacity="0.4"/>
                <stop offset="100%" stop-color="rgb(217,70,239)" stop-opacity="0"/>
              </linearGradient>
            </defs>
            {#if history.length >= 2}
              {@const path = sparkline(history, h => h.out_bps)}
              {@const fill = path + ` L280,60 L0,60 Z`}
              <path d={fill} fill="url(#grad-out)" />
              <path d={path} stroke="rgb(217,70,239)" stroke-width="1.5" fill="none" />
            {/if}
          </svg>
        </div>
      </section>

      <!-- Blocked / threat stats -->
      <section class="grid grid-cols-1 md:grid-cols-3 gap-4">
        <div class="bg-ink-900 border border-red-500/30 rounded-xl p-5">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Packets blocked</div>
          <div class="text-2xl font-mono text-red-400 mt-1">{m.blocked_24h.toLocaleString()}</div>
          <p class="text-[10px] text-zinc-500 mt-1">cumulative — iptables DROP/REJECT counters</p>
        </div>
        <div class="bg-ink-900 border border-amber-500/30 rounded-xl p-5">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Suspicious events</div>
          <div class="text-2xl font-mono text-amber-400 mt-1">{m.suspicious_events.length}</div>
          <p class="text-[10px] text-zinc-500 mt-1">heuristic detector — extends as alert sources land</p>
        </div>
        <div class="bg-ink-900 border border-cursed-500/30 rounded-xl p-5">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Active clients</div>
          <div class="text-2xl font-mono text-cursed-300 mt-1">{m.top_clients.length}</div>
          <p class="text-[10px] text-zinc-500 mt-1">unique source IPs in current conntrack</p>
        </div>
      </section>

      <!-- Top clients -->
      {#if m.top_clients.length}
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <h3 class="font-mono text-xs uppercase tracking-wider text-zinc-400">Top clients by traffic</h3>
          <div class="space-y-1">
            {#each m.top_clients as c}
              {@const pct = m.top_clients.length > 0
                ? (c.bytes / (m.top_clients[0]?.bytes || 1)) * 100
                : 0}
              <div class="space-y-0.5">
                <div class="flex justify-between text-xs font-mono">
                  <span class="text-zinc-300">{c.ip}</span>
                  <span class="text-zinc-500">{fmtBytes(c.bytes)}</span>
                </div>
                <div class="h-1.5 bg-ink-800 rounded overflow-hidden">
                  <div class="h-full bg-gradient-to-r from-cursed-500 to-fuchsia-400"
                       style="width: {pct}%"></div>
                </div>
              </div>
            {/each}
          </div>
        </section>
      {/if}

      <!-- Suspicious events -->
      {#if m.suspicious_events.length}
        <section class="bg-ink-900 border border-amber-500/30 rounded-xl p-5 space-y-3">
          <h3 class="font-mono text-xs uppercase tracking-wider text-amber-400">Suspicious activity</h3>
          <div class="space-y-2">
            {#each m.suspicious_events as ev}
              <div class="p-2 rounded border
                          {ev.severity === 'crit' ? 'bg-red-500/10 border-red-500/40'
                            : ev.severity === 'warn' ? 'bg-amber-500/10 border-amber-500/40'
                            : 'bg-ink-800 border-ink-700'}">
                <div class="text-sm font-medium text-zinc-200">{ev.label}</div>
                <div class="text-xs text-zinc-400 mt-0.5">{ev.detail}</div>
              </div>
            {/each}
          </div>
        </section>
      {:else}
        <p class="text-xs text-zinc-500 italic">
          No suspicious events flagged. The detector watches for anomalies
          like sudden traffic spikes, blacklisted-domain attempts, and
          repeated SYN floods. Quiet here means quiet on the wire.
        </p>
      {/if}
    {/if}

    <section class="text-xs text-zinc-500 leading-relaxed">
      <strong class="text-zinc-300">Compute budget:</strong>
      this page re-polls every 5 seconds and each poll is two cheap file
      reads + one iptables list parse — negligible CPU on a Pi 4. We
      keep ~5 minutes of throughput history in supervisor memory; no
      timeseries DB needed.
    </section>
    </div>
  </main>
</div>
