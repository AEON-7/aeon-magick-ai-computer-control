<script lang="ts">
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let m: api.SecurityMetrics | null = null;
  let blocked: api.BlockedPacket[] = [];
  let blockedSince = '';
  let error = '';
  let poll_iv: ReturnType<typeof setInterval>;

  async function refresh() {
    try {
      m = await api.getSecurityMetrics();
      const b = await api.getBlockedPackets(300);
      blocked = b.entries;
      blockedSince = b.since;
    } catch (e: any) {
      error = e?.message ?? 'failed to load';
    }
  }

  onMount(() => {
    refresh();
    // 5s poll keeps the Pi cool — each request is one /sys file read
    // per iface + one iptables -nvL invocation + one journalctl tail.
    poll_iv = setInterval(refresh, 5000);
  });
  onDestroy(() => { if (poll_iv) clearInterval(poll_iv); });

  function fmtTime(ms: number): string {
    if (!ms) return '—';
    const d = new Date(ms);
    return d.toLocaleTimeString();
  }
  function fmtAgo(ms: number): string {
    const dt = Date.now() - ms;
    if (dt < 60_000) return 'just now';
    if (dt < 3_600_000) return `${Math.floor(dt / 60_000)} min ago`;
    return `${Math.floor(dt / 3_600_000)}h ago`;
  }

  /// Map a blocked-packet cause_tag to a human label + one-line
  /// explanation. Returns null for unknown tags so we don't show a
  /// useless "fw-abc123" pill. severity=expected means "this is
  /// intended behavior, the system did this on purpose"; everything
  /// else is "you may want to inspect this".
  function describeCause(tag: string):
      { label: string; note: string; severity: 'expected' | 'user' } | null {
    if (!tag) return null;
    if (tag.startsWith('fw-')) {
      return {
        label: 'User firewall rule',
        note: `Rule ${tag.slice(3)} — open /network → Advanced to edit or remove.`,
        severity: 'user',
      };
    }
    switch (tag) {
      case 'vpn-udp-forward':
        return {
          label: 'VPN: UDP catchall',
          note: 'UDP (incl. QUIC/HTTP-3 on :443) cannot traverse Tor. The Pi sends ICMP port-unreachable so the browser drops QUIC immediately and falls back to TCP HTTPS — much faster than waiting for the QUIC timeout.',
          severity: 'expected',
        };
      case 'vpn-udp-output':
        return {
          label: 'VPN: local-host UDP',
          note: 'Pi-originating UDP (other than DHCP/NTP/mDNS) when Tor is active — same ICMP-port-unreachable fast-fail.',
          severity: 'expected',
        };
      case 'vpn-killswitch':
        return {
          label: 'VPN: kill-switch',
          note: 'Tunnel is down or disabled — kill-switch is sending ICMP host-unreachable to fail-fast all WAN attempts.',
          severity: 'expected',
        };
      case 'usbnet-iso-rfc1918':
        return {
          label: 'Isolation: LAN destination',
          note: 'USB-mode is "isolation" or "restricted" — clients can\'t reach RFC1918 LAN destinations. ICMP host-unreachable returned so apps fail-fast.',
          severity: 'expected',
        };
      case 'usbnet-iso-pi-rfc1918':
        return {
          label: 'Isolation: Pi via LAN IP',
          note: 'Client tried to reach the Pi via a non-usb0 IP — use the Pi\'s usb0 address instead. ICMP host-unreachable returned for fast fall-back.',
          severity: 'expected',
        };
      case 'usbnet-restricted-input':
        return {
          label: 'Restricted: Pi services',
          note: 'USB-mode is "restricted" — only DHCP + DNS reach the Pi. ICMP port-unreachable returned so the host fails-fast instead of timing out on SSH/HTTPS attempts.',
          severity: 'expected',
        };
      default:
        return {
          label: tag,
          note: '',
          severity: 'user',
        };
    }
  }

  /// Build a /network deep-link that pre-populates the firewall rule
  /// form to ALLOW the observed traffic. Every field the kernel logged
  /// is forwarded so the resulting rule is as narrow as we can make it
  /// from a single observed packet:
  ///   chain   = FORWARD (if the kernel logged OUT=, it was forwarded;
  ///                      otherwise INPUT — local-host-bound)
  ///   iface   = inbound interface (-i)
  ///   out_iface = outbound interface (-o, FORWARD only)
  ///   proto   = exact protocol
  ///   src/dst = single hosts (iptables treats as /32 by default)
  ///   sport/dport = exact ports
  /// The user can broaden any field in the form after they review it.
  function allowLink(p: api.BlockedPacket): string {
    const params = new URLSearchParams();
    params.set('createRule', '1');
    // OUT= empty in the log means the kernel was about to deliver
    // locally (INPUT chain). Anything else was being forwarded.
    const chain = p.out_iface ? 'FORWARD' : 'INPUT';
    params.set('chain', chain);
    params.set('action', 'ACCEPT');
    if (p.proto) params.set('proto', p.proto.toLowerCase());
    if (p.in_iface) params.set('iface', p.in_iface);
    if (p.out_iface && chain === 'FORWARD') params.set('out_iface', p.out_iface);
    if (p.src) params.set('src', p.src);
    if (p.dst) params.set('dst', p.dst);
    if (p.sport) params.set('sport', p.sport);
    if (p.dport) params.set('dport', p.dport);
    const portTag = p.dport ? `:${p.dport}` : '';
    const ifaceTag = p.in_iface && p.out_iface
      ? ` (${p.in_iface}→${p.out_iface})`
      : p.in_iface ? ` (${p.in_iface})` : '';
    params.set(
      'comment',
      `Allow ${p.proto || 'traffic'}${ifaceTag} from ${p.src || 'any'} to ${p.dst || 'any'}${portTag}`,
    );
    return `/network?${params.toString()}#advanced`;
  }

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

<div class="page-void h-full flex flex-col">
  <PageHeader title="security console" subtitle="metrics · blocked traffic" index="09" />

  <main class="flex-1 overflow-auto">
    <div class="p-6 max-w-5xl mx-auto w-full space-y-6">
    {#if error}<p class="text-red-400 text-sm">{error}</p>{/if}

    {#if m}
      <!-- Throughput at-a-glance -->
      <section class="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div class="panel p-5">
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
        <div class="panel p-5">
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
        <div class="bg-ink-900 border border-red-500/30 rounded-sm p-5">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Packets blocked</div>
          <div class="text-2xl font-mono text-red-400 mt-1">{m.blocked_24h.toLocaleString()}</div>
          <p class="text-[10px] text-zinc-500 mt-1">cumulative — iptables DROP/REJECT counters</p>
        </div>
        <div class="bg-ink-900 border border-amber-500/30 rounded-sm p-5">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Suspicious events</div>
          <div class="text-2xl font-mono text-amber-400 mt-1">{m.suspicious_events.length}</div>
          <p class="text-[10px] text-zinc-500 mt-1">heuristic detector — extends as alert sources land</p>
        </div>
        <div class="bg-ink-900 border border-cursed-500/30 rounded-sm p-5">
          <div class="text-[10px] uppercase tracking-wider text-zinc-500">Active clients</div>
          <div class="text-2xl font-mono text-cursed-300 mt-1">{m.top_clients.length}</div>
          <p class="text-[10px] text-zinc-500 mt-1">unique source IPs in current conntrack</p>
        </div>
      </section>

      <!-- Top clients -->
      {#if m.top_clients.length}
        <section class="panel p-5 space-y-3">
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

      <!-- ─── Blocked traffic (last 2h) ─── -->
      <section class="bg-ink-900 border border-red-500/30 rounded-sm p-5 space-y-3">
        <header class="flex items-center justify-between flex-wrap gap-2">
          <div>
            <h3 class="font-mono text-xs uppercase tracking-wider text-red-300">
              Blocked traffic
              <span class="text-zinc-500 ml-2">({blocked.length})</span>
            </h3>
            <p class="text-[11px] text-zinc-500">
              Last 2 hours of packets dropped by the firewall. Use "allow this traffic"
              to open a pre-populated firewall rule. Logging is rate-limited (5/sec)
              so a chatty broadcast storm can't flood this view.
            </p>
          </div>
        </header>
        {#if blocked.length === 0}
          <p class="text-xs text-zinc-500 italic">
            Nothing blocked recently. Either no rules are firing or your traffic
            is all legit — both are good outcomes.
          </p>
        {:else}
          <div class="max-h-[420px] overflow-auto rounded border border-ink-800
                      divide-y divide-ink-800 bg-ink-950">
            {#each blocked as b (b.ts_ms + b.src + b.dst + b.dport + b.proto)}
              {@const cause = describeCause(b.cause_tag)}
              <div class="flex flex-col gap-1 p-2 text-[11px] font-mono
                          hover:bg-ink-900/60">
                <div class="flex items-center gap-3">
                  <span class="text-zinc-600 w-20 shrink-0" title={fmtTime(b.ts_ms)}>
                    {fmtAgo(b.ts_ms)}
                  </span>
                  <span class="text-[10px] px-1.5 py-0.5 rounded border shrink-0
                               {b.proto === 'TCP' ? 'bg-cursed-500/15 text-cursed-300 border-cursed-500/40'
                                 : b.proto === 'UDP' ? 'bg-fuchsia-500/15 text-fuchsia-300 border-fuchsia-500/40'
                                 : 'bg-ink-700 text-zinc-300 border-steel-600'}">
                    {b.proto || '?'}
                  </span>
                  <span class="text-zinc-500 w-12 shrink-0 truncate">
                    {b.in_iface || '?'}
                  </span>
                  <span class="text-zinc-300 flex-1 truncate"
                        title={`${b.src}${b.sport ? ':' + b.sport : ''} → ${b.dst}${b.dport ? ':' + b.dport : ''}`}>
                    {b.src}{b.sport ? ':' + b.sport : ''}
                    <span class="text-zinc-600">→</span>
                    {b.dst}{b.dport ? ':' + b.dport : ''}
                  </span>
                  <a href={allowLink(b)}
                     class="text-[10px] px-2 py-1 rounded shrink-0
                            border border-live-500/40 text-live-300
                            hover:bg-live-500/15 hover:text-live-200
                            transition-colors"
                     title="Open firewall rule editor pre-filled to ACCEPT this traffic">
                    ✓ allow this traffic
                  </a>
                </div>
                {#if cause}
                  <!-- Cause attribution: which rule blocked the packet
                       and a one-liner explaining what to do about it. -->
                  <div class="flex items-start gap-2 pl-[5.75rem] text-[10px]">
                    <span class="px-1.5 py-0.5 rounded border shrink-0 normal-case
                                 {cause.severity === 'expected'
                                   ? 'bg-amber-500/10 text-amber-300 border-amber-500/30'
                                   : 'bg-red-500/10 text-red-300 border-red-500/30'}">
                      {cause.label}
                    </span>
                    {#if cause.note}
                      <span class="text-zinc-500 normal-case">{cause.note}</span>
                    {/if}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <!-- Suspicious events -->
      {#if m.suspicious_events.length}
        <section class="bg-ink-900 border border-amber-500/30 rounded-sm p-5 space-y-3">
          <h3 class="font-mono text-xs uppercase tracking-wider text-amber-400">Suspicious activity</h3>
          <div class="space-y-2">
            {#each m.suspicious_events as ev}
              <div class="p-2 rounded border
                          {ev.severity === 'crit' ? 'bg-red-500/10 border-red-500/40'
                            : ev.severity === 'warn' ? 'bg-amber-500/10 border-amber-500/40'
                            : 'bg-ink-800 border-steel-700'}">
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
