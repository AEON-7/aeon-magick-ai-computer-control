<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { browser } from '$app/environment';
  import * as api from '$lib/api';

  let tab: 'overview' | 'systems' | 'containers' | 'terminal' = 'overview';
  let pubkey = '';
  let systems: api.ConnectedSystem[] = [];
  let metrics: Record<string, api.SystemMetrics> = {};
  let agents: Record<string, api.AgentRoster> = {};
  let usage: Record<string, api.UsageHistory> = {};
  let range: 'all' | '30d' | '90d' | '1y' | 'month' | 'year' = 'all';
  let selMonth = '';
  let selYear = '';
  let loading = true;

  // agent detail modal
  let detailSys = '';
  let detailAgent: api.AgentInfo | null = null;
  let detail: api.AgentDetail | null = null;
  let detailLoading = false;
  let provisioning = false;
  let newToken = '';
  let configChange = '';
  let dropMsg = '';
  // ssh provisioning (admin only)
  let sshBusy = false;
  let newPrivKey = '';
  let sshCmd = '';
  let sshDropMsg = '';
  let grantSudo = false;
  // E1: Matrix avatar
  let avatar: api.AgentAvatar | null = null;
  let avatarBusy = false;
  let avatarMsg = '';
  // E1: corpus browser
  let corpus: api.CorpusList | null = null;
  let corpusLoading = false;
  let corpusFile: api.CorpusFile | null = null;
  let corpusFilePath = '';
  let corpusFileLoading = false;
  let corpusFilter = '';
  // E1: voice
  let voice: api.AgentVoice | null = null;
  // E1: add-skill
  let skillBusy = '';                 // skill name currently being added ('' = idle)
  let skillResult: api.AddSkillResult | null = null;
  let customSkillName = '';
  let customSkillFile: File | null = null;
  // F7a: persona files (Soul + Identity) editor
  let personaFiles: Record<api.PersonaWhich, api.PersonaFile | null> = { soul: null, identity: null };
  let personaDraft: Record<api.PersonaWhich, string> = { soul: '', identity: '' };
  let personaLoading: Record<api.PersonaWhich, boolean> = { soul: false, identity: false };
  let personaSaving: Record<api.PersonaWhich, boolean> = { soul: false, identity: false };
  let personaMsg: Record<api.PersonaWhich, string> = { soul: '', identity: '' };
  // F7a: the two persona files to render (typed here so the template needs no casts).
  const PERSONA_FILES: { which: api.PersonaWhich; label: string; file: string; hint: string }[] = [
    { which: 'soul', label: 'Soul', file: 'SOUL.md', hint: 'the essence: voice, values, manner (the system prompt)' },
    { which: 'identity', label: 'Identity', file: 'IDENTITY.md', hint: 'the facts: name, era, domain, emoji' },
  ];
  // F7b: deploy a new persona
  let showNewPersona = false;
  let npSysId = '';
  let np: api.NewPersonaInput = { id: '', name: '', emoji: '', identity: '', soul: '', voice: '', corpus_seed: '', model: '' };
  let npBusy = false;
  let npResult: api.NewPersonaResult | null = null;
  // per-system power controls
  let lastMac: Record<string, string> = {};
  let powerBusy = '';
  let metricsLoading = false;
  let err = '';
  let timer: ReturnType<typeof setInterval> | undefined;

  // add-system form
  let label = '';
  let address = '';
  let sshUser = 'root';
  let port = 22;
  let roleOpenclaw = false;
  let roleHermes = false;
  let roleDgx = false;
  let adding = false;
  let busyId = '';
  let fallback: Record<string, string> = {};

  // ── E4: Container Management ──
  let cSys = '';                                   // selected system id
  let cData: api.ContainerList | null = null;
  let cLoading = false;
  let cErr = '';
  let cBusy = '';                                  // container name being acted on
  let cTimer: ReturnType<typeof setInterval> | undefined;
  // compose editor
  let composeEdit: { path: string; content: string } | null = null;
  let composeBusy = '';                            // compose path being acted on
  let composeSaving = false;
  let composeMsg = '';
  // ── E5: Easy Deploy — model+container picker, template flags, progress ──
  let deployCat: api.DeployCatalog | null = null;
  let deployEntries: api.DeployCatalogEntry[] = [];
  let deploySel = -1;                               // index into deployEntries (-1 = none)
  let deployName = '';
  // the highlighted flags (pre-filled from the picked entry's template)
  let dfModelLen: number | null = 131072;           // default 128k context
  let dfGpu = '1';                                  // which/how many devices ("all" | "1" | "0,1")
  let dfGpuPct = 70;                                // % of total GPU VRAM (slider) → --gpu-memory-utilization. NOT 100 (OOMs).
  let dfMaxBatchTok: number | null = 8192;
  let dfMaxSeqs: number | null = 8;
  let dfExtra = '';                                 // advanced: raw extra args
  let depAdvOpen = false;                           // advanced flags collapsed
  let deployBusy = false;
  let deployResult: api.DeployResult | null = null;
  let deployErr = '';
  let deployCatLoading = false;
  // install progress (polled after a deploy kicks off)
  let depStatus: api.DeployStatus | null = null;
  let depPolling = false;
  let depPollTimer: ReturnType<typeof setTimeout> | null = null;

  // ── E2: multi-pane web SSH terminal ──
  // xterm.js + addon-fit are loaded lazily in the browser (they touch the DOM
  // on import, so a static import would break the SSR/prerender build).
  type XtermMod = typeof import('@xterm/xterm');
  type FitMod = typeof import('@xterm/addon-fit');
  let XTerm: XtermMod['Terminal'] | null = null;
  let FitAddonCtor: FitMod['FitAddon'] | null = null;
  let xtermLoading = false;
  type Pane = {
    key: number; // unique pane id (allows duplicates of one system)
    sysId: string;
    label: string;
    el: HTMLDivElement | null; // bound container
    term: any | null; // xterm Terminal
    fit: any | null; // FitAddon
    ws: WebSocket | null;
    ro: ResizeObserver | null;
    status: 'connecting' | 'open' | 'closed';
  };
  let panes: Pane[] = [];
  let paneSeq = 1;

  /** Lazy-load xterm + the fit addon + its CSS (browser only, once). */
  async function ensureXterm(): Promise<boolean> {
    if (!browser) return false;
    if (XTerm && FitAddonCtor) return true;
    if (xtermLoading) {
      // Wait for the in-flight load to settle.
      while (xtermLoading) await new Promise((r) => setTimeout(r, 30));
      return !!(XTerm && FitAddonCtor);
    }
    xtermLoading = true;
    try {
      const [x, f] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
        import('@xterm/xterm/css/xterm.css'),
      ]);
      XTerm = x.Terminal;
      FitAddonCtor = f.FitAddon;
      return true;
    } catch (e) {
      console.error('xterm load failed', e);
      return false;
    } finally {
      xtermLoading = false;
    }
  }

  /** Open a new terminal pane for a system and switch to the Terminal tab. */
  async function openTerminal(sysId: string) {
    const sys = systems.find((s) => s.id === sysId);
    if (!sys) return;
    tab = 'terminal';
    const pane: Pane = {
      key: paneSeq++,
      sysId,
      label: sys.label,
      el: null,
      term: null,
      fit: null,
      ws: null,
      ro: null,
      status: 'connecting',
    };
    panes = [...panes, pane];
    // Wait for the {#each} to render the pane's container, then mount xterm.
    await tick();
    await mountPane(pane);
  }

  async function mountPane(pane: Pane) {
    const ok = await ensureXterm();
    if (!ok || !pane.el || !XTerm || !FitAddonCtor) {
      pane.status = 'closed';
      panes = panes;
      return;
    }
    const term = new XTerm({
      cursorBlink: true,
      fontFamily:
        'ui-monospace, SFMono-Regular, Menlo, Monaco, "Cascadia Code", "Source Code Pro", monospace',
      fontSize: 13,
      scrollback: 5000,
      allowProposedApi: true,
      theme: {
        background: '#0b0b12',
        foreground: '#d4d4d8',
        cursor: '#a78bfa',
        cursorAccent: '#0b0b12',
        selectionBackground: 'rgba(167,139,250,0.35)',
        black: '#18181b',
        red: '#f87171',
        green: '#34d399',
        yellow: '#fbbf24',
        blue: '#60a5fa',
        magenta: '#c4b5fd',
        cyan: '#22d3ee',
        white: '#e4e4e7',
        brightBlack: '#52525b',
        brightRed: '#fca5a5',
        brightGreen: '#6ee7b7',
        brightYellow: '#fde68a',
        brightBlue: '#93c5fd',
        brightMagenta: '#ddd6fe',
        brightCyan: '#67e8f9',
        brightWhite: '#fafafa',
      },
    });
    const fit = new FitAddonCtor();
    term.loadAddon(fit);
    term.open(pane.el);
    pane.term = term;
    pane.fit = fit;
    try {
      fit.fit();
    } catch {
      /* element not laid out yet — the ResizeObserver below will refit */
    }

    // Open the WebSocket to the PTY bridge (same-origin → admin cookie auth).
    const ws = new WebSocket(api.terminalWsURL(pane.sysId));
    ws.binaryType = 'arraybuffer';
    pane.ws = ws;

    ws.onopen = () => {
      pane.status = 'open';
      panes = panes;
      sendResize(pane);
      term.focus();
    };
    ws.onmessage = (ev: MessageEvent) => {
      const d = ev.data;
      if (typeof d === 'string') {
        term.write(d);
      } else if (d instanceof ArrayBuffer) {
        term.write(new Uint8Array(d));
      } else if (d instanceof Blob) {
        d.arrayBuffer().then((b) => term.write(new Uint8Array(b)));
      }
    };
    ws.onclose = () => {
      pane.status = 'closed';
      panes = panes;
      try {
        term.write('\r\n\x1b[2m[connection closed]\x1b[0m\r\n');
      } catch {
        /* term may already be disposed */
      }
    };
    ws.onerror = () => {
      pane.status = 'closed';
      panes = panes;
    };

    // Keystrokes / paste → PTY stdin.
    term.onData((data: string) => {
      if (ws.readyState === WebSocket.OPEN) ws.send(data);
    });

    // Refit + tell the PTY whenever the pane resizes.
    const ro = new ResizeObserver(() => {
      try {
        fit.fit();
      } catch {
        /* ignore transient layout errors */
      }
      sendResize(pane);
    });
    ro.observe(pane.el);
    pane.ro = ro;
    panes = panes;
  }

  /** Send the current xterm geometry to the PTY as a resize control frame. */
  function sendResize(pane: Pane) {
    if (!pane.term || !pane.ws || pane.ws.readyState !== WebSocket.OPEN) return;
    const cols = pane.term.cols;
    const rows = pane.term.rows;
    if (cols > 0 && rows > 0) {
      pane.ws.send(JSON.stringify({ type: 'resize', cols, rows }));
    }
  }

  function teardownPane(pane: Pane) {
    try {
      pane.ro?.disconnect();
    } catch {
      /* noop */
    }
    try {
      pane.ws?.close();
    } catch {
      /* noop */
    }
    try {
      pane.term?.dispose();
    } catch {
      /* noop */
    }
    pane.ro = null;
    pane.ws = null;
    pane.term = null;
    pane.fit = null;
  }

  function closePane(key: number) {
    const pane = panes.find((p) => p.key === key);
    if (pane) teardownPane(pane);
    panes = panes.filter((p) => p.key !== key);
  }

  function closeAllPanes() {
    for (const p of panes) teardownPane(p);
    panes = [];
  }

  /** Re-fit every pane (e.g. after the grid column count changes). */
  async function refitAll() {
    await tick();
    for (const p of panes) {
      try {
        p.fit?.fit();
      } catch {
        /* noop */
      }
      sendResize(p);
    }
  }
  // When the pane count changes the grid template changes → refit all panes.
  $: if (browser && tab === 'terminal') {
    void panes.length;
    refitAll();
  }
  /** Tailwind-ish grid column count for the pane grid (1/2/3). */
  function paneGridCols(n: number): number {
    if (n <= 1) return 1;
    if (n === 2) return 2;
    return 3;
  }

  $: openclawSystems = systems.filter((s) => s.roles?.includes('openclaw'));
  $: cSysObj = systems.find((s) => s.id === cSys) ?? null;

  const inputCls =
    'bg-ink-900 border border-ink-700 rounded px-2 py-1 text-xs text-zinc-200 placeholder-zinc-600 focus:outline-none focus:border-cursed-500';

  async function refresh() {
    try {
      const [pk, sys] = await Promise.all([api.getAgentPubkey(), api.listSystems()]);
      pubkey = pk.pubkey ?? '';
      systems = sys.systems ?? [];
    } catch (e) {
      err = (e as any)?.message ?? String(e);
    } finally {
      loading = false;
    }
  }
  async function loadMetrics() {
    if (!systems.length) return;
    metricsLoading = true;
    await Promise.all([
      ...systems.map(async (s) => {
        try {
          const r = await api.getSystemMetrics(s.id);
          metrics[s.id] = r.metrics;
          if (r.metrics?.mac) lastMac[s.id] = r.metrics.mac; // remember for WoL when offline
        } catch {
          metrics[s.id] = { reachable: false };
        }
      }),
      ...openclawSystems.map(async (s) => {
        try {
          agents[s.id] = await api.getSystemAgents(s.id);
        } catch {
          agents[s.id] = { ok: false, reachable: false };
        }
      }),
      ...openclawSystems.map(async (s) => {
        try {
          usage[s.id] = await api.getSystemUsage(s.id);
        } catch {
          /* keep prior history on a transient failure */
        }
      }),
    ]);
    metrics = metrics;
    agents = agents;
    usage = usage;
    metricsLoading = false;
  }
  onMount(async () => {
    await refresh();
    await loadMetrics();
    timer = setInterval(() => {
      if (tab === 'overview' && !document.hidden) loadMetrics();
    }, 15000);
    // E4: refresh the container view (incl. live stats) every 5s while the
    // Containers tab is open + a system is selected + the page is visible.
    cTimer = setInterval(() => {
      if (tab === 'containers' && cSys && !cLoading && !document.hidden) loadContainers(true);
    }, 5000);
  });
  onDestroy(() => {
    clearInterval(timer);
    clearInterval(cTimer);
    stopDeployPoll();
    closeAllPanes();
  });

  // ── formatting helpers ──────────────────────────────────────────────
  function fmtTok(n?: number): string {
    n = n ?? 0;
    if (n >= 1e9) return (n / 1e9).toFixed(2) + 'B';
    if (n >= 1e6) return (n / 1e6).toFixed(2) + 'M';
    if (n >= 1e3) return (n / 1e3).toFixed(1) + 'k';
    return String(n);
  }
  function fmtAgo(s?: number | null): string {
    if (s == null) return '—';
    if (s < 60) return Math.round(s) + 's';
    if (s < 3600) return Math.round(s / 60) + 'm';
    if (s < 86400) return Math.round(s / 3600) + 'h';
    return Math.round(s / 86400) + 'd';
  }
  function num(x: unknown): number {
    const v = typeof x === 'string' ? parseFloat(x) : (x as number);
    return isNaN(v as number) ? NaN : (v as number);
  }
  function clampPct(x: unknown): number {
    const v = num(x);
    return isNaN(v) ? 0 : Math.min(100, Math.max(0, v));
  }
  function utilColor(u: unknown): string {
    const v = num(u);
    if (isNaN(v)) return '#3f3f46';
    if (v >= 85) return '#f87171';
    if (v >= 60) return '#fbbf24';
    if (v >= 30) return '#34d399';
    return '#22d3ee';
  }
  function parseMem(mem?: string): { used: number; total: number; pct: number } | null {
    if (!mem) return null;
    const [u, t] = mem.split('/').map((x) => parseInt(x.trim(), 10));
    if (isNaN(u) || isNaN(t) || t === 0) return null;
    return { used: u, total: t, pct: Math.round((u / t) * 100) };
  }
  const gb = (mb: number) => (mb / 1024).toFixed(1);
  const isDgx = (s: api.ConnectedSystem) => s.roles?.includes('dgx');
  /** For a DGX, system memory IS the GPU VRAM (unified GB10). */
  function unifiedVram(s: api.ConnectedSystem, m?: api.SystemMetrics) {
    return isDgx(s) ? parseMem(m?.mem) : null;
  }
  function vramPct(g: { mem_used: string; mem_total: string }): number {
    const u = num(g.mem_used),
      t = num(g.mem_total);
    return isNaN(u) || isNaN(t) || t === 0 ? 0 : Math.min(100, (u / t) * 100);
  }
  const hasNvVram = (g: { mem_total: string }) => !isNaN(num(g.mem_total));
  function memTile(s: api.ConnectedSystem, m?: api.SystemMetrics): string {
    const pm = parseMem(m?.mem);
    if (!pm) return '—';
    return gb(pm.used) + '/' + gb(pm.total) + 'G';
  }
  function roleIcon(roles: string[]): string {
    if (roles?.includes('dgx')) return '⚡';
    if (roles?.includes('openclaw')) return '🧠';
    if (roles?.includes('hermes')) return '📡';
    return '🖥️';
  }

  // ── agent roster helpers ────────────────────────────────────────────
  function agentRank(a: api.AgentInfo): number {
    return (
      (a.working ? 8 : 0) +
      (a.on_call ? 4 : 0) +
      (a.active || (a.active_sessions ?? 0) > 0 ? 2 : 0) +
      (a.is_default ? 1 : 0)
    );
  }
  function sortedAgents(r?: api.AgentRoster): api.AgentInfo[] {
    return [...(r?.agents ?? [])].sort(
      (a, b) =>
        agentRank(b) - agentRank(a) ||
        (b.total_tokens ?? 0) - (a.total_tokens ?? 0) ||
        a.name.localeCompare(b.name),
    );
  }
  function rosterSummary(r?: api.AgentRoster) {
    const ags = r?.agents ?? [];
    return {
      count: ags.length,
      active: ags.filter((a) => a.working || a.on_call || a.active || (a.active_sessions ?? 0) > 0)
        .length,
      tokens: ags.reduce((s, a) => s + (a.total_tokens ?? 0), 0),
    };
  }
  function tokMax(r?: api.AgentRoster): number {
    return Math.max(1, ...(r?.agents ?? []).map((a) => a.total_tokens ?? 0));
  }
  function agentStatus(a: api.AgentInfo): { label: string; color: string; pulse: boolean } {
    if (a.working) return { label: 'working', color: '#34d399', pulse: true };
    if (a.on_call) return { label: 'on call', color: '#fbbf24', pulse: true };
    if (a.active || (a.active_sessions ?? 0) > 0)
      return { label: 'active', color: '#22d3ee', pulse: false };
    if (a.last_seen_s_ago != null && a.last_seen_s_ago < 3600)
      return { label: 'idle', color: '#4ade80', pulse: false };
    if (a.last_seen_s_ago != null)
      return { label: 'seen ' + fmtAgo(a.last_seen_s_ago), color: '#71717a', pulse: false };
    return { label: 'cold', color: '#3f3f46', pulse: false };
  }

  // ── token-usage history (banner) ────────────────────────────────────
  const MONTHS = ['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'];
  function fmtLocalDate(d: Date): string {
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }
  function daysBack(todayStr: string, n: number): string[] {
    const [y, m, d] = (todayStr || '').split('-').map(Number);
    if (!y) return [];
    const base = new Date(y, m - 1, d);
    const out: string[] = [];
    for (let i = n - 1; i >= 0; i--) {
      const x = new Date(base);
      x.setDate(base.getDate() - i);
      out.push(fmtLocalDate(x));
    }
    return out;
  }
  function lastMonths(todayStr: string, n: number): string[] {
    const [y, m] = (todayStr || '').split('-').map(Number);
    if (!y) return [];
    const out: string[] = [];
    for (let i = n - 1; i >= 0; i--) {
      const d = new Date(y, m - 1 - i, 1);
      out.push(`${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`);
    }
    return out;
  }
  function monthDays(ym: string): string[] {
    if (!ym) return [];
    const [y, m] = ym.split('-').map(Number);
    const dim = new Date(y, m, 0).getDate();
    return Array.from({ length: dim }, (_, i) => `${ym}-${String(i + 1).padStart(2, '0')}`);
  }
  const monthLabel = (ym: string): string => {
    if (!ym) return '';
    const [y, m] = ym.split('-').map(Number);
    return `${MONTHS[m - 1]} ${y}`;
  };
  function monthsBetween(first: string, today: string): string[] {
    if (!first || !today) return [];
    let [y, m] = first.split('-').map(Number);
    const [ty, tm] = today.split('-').map(Number);
    const out: string[] = [];
    while (y < ty || (y === ty && m <= tm)) {
      out.push(`${y}-${String(m).padStart(2, '0')}`);
      m++;
      if (m > 12) { m = 1; y++; }
    }
    return out.reverse();
  }
  function yearsBetween(first: string, today: string): string[] {
    const fy = Number((first || '').slice(0, 4));
    const ty = Number((today || '').slice(0, 4));
    if (!fy || !ty) return [];
    const out: string[] = [];
    for (let y = ty; y >= fy; y--) out.push(String(y));
    return out;
  }
  function trackedDays(u: api.UsageHistory): number {
    if (!u?.first_day || !u?.today) return 0;
    const [fy, fm, fd] = u.first_day.split('-').map(Number);
    const [ty, tm, td] = u.today.split('-').map(Number);
    return Math.max(1, Math.round((+new Date(ty, tm - 1, td) - +new Date(fy, fm - 1, fd)) / 86400000) + 1);
  }
  const tokAt = (u: api.UsageHistory, date: string): number => u?.daily?.[date]?.total ?? 0;
  function sumDates(u: api.UsageHistory, dates: string[]): number {
    return dates.reduce((s, d) => s + tokAt(u, d), 0);
  }
  function perAgentForDates(u: api.UsageHistory, dates: string[]): Record<string, number> {
    const acc: Record<string, number> = {};
    for (const d of dates) {
      const ag = u?.daily?.[d]?.agents ?? {};
      for (const id in ag) acc[id] = (acc[id] ?? 0) + ag[id];
    }
    return acc;
  }
  function rangeShort(): string {
    if (range === 'all') return 'all-time';
    if (range === '30d') return '30d';
    if (range === '90d') return '90d';
    if (range === '1y') return '1y';
    if (range === 'month') return monthLabel(selMonth);
    if (range === 'year') return selYear;
    return '';
  }
  type Bucket = { label: string; tokens: number };
  type RangeView = { label: string; total: number; perAgent: Record<string, number>; buckets: Bucket[] };
  function view(u: api.UsageHistory, r?: api.AgentRoster): RangeView {
    if (range === 'all') {
      // All-time = the live gateway snapshot (always available): total is the
      // gateway's running total, the chart is a per-agent breakdown.
      const ags = [...(r?.agents ?? [])].sort((a, b) => (b.total_tokens ?? 0) - (a.total_tokens ?? 0));
      return {
        label: 'all-time',
        total: u?.gateway_total ?? ags.reduce((s, a) => s + (a.total_tokens ?? 0), 0),
        perAgent: Object.fromEntries(ags.map((a) => [a.id, a.total_tokens ?? 0])),
        buckets: ags.slice(0, 16).map((a) => ({ label: a.emoji || (a.name ?? a.id).slice(0, 4), tokens: a.total_tokens ?? 0 })),
      };
    }
    const today = u?.today || '';
    let dates: string[] = [];
    let groups: { label: string; dates: string[] }[] = [];
    let label = '';
    if (range === '30d') {
      dates = daysBack(today, 30);
      groups = dates.map((d) => ({ label: d.slice(5), dates: [d] }));
      label = 'last 30 days';
    } else if (range === '90d') {
      dates = daysBack(today, 90);
      for (let i = 0; i < dates.length; i += 7) groups.push({ label: dates[i].slice(5), dates: dates.slice(i, i + 7) });
      label = 'last 90 days';
    } else if (range === '1y') {
      for (const ym of lastMonths(today, 12)) {
        const ds = monthDays(ym);
        dates.push(...ds);
        groups.push({ label: ym.slice(2), dates: ds });
      }
      label = 'last 12 months';
    } else if (range === 'month') {
      dates = monthDays(selMonth);
      groups = dates.map((d) => ({ label: d.slice(8), dates: [d] }));
      label = monthLabel(selMonth);
    } else if (range === 'year') {
      for (let m = 1; m <= 12; m++) {
        const ym = `${selYear}-${String(m).padStart(2, '0')}`;
        const ds = monthDays(ym);
        dates.push(...ds);
        groups.push({ label: MONTHS[m - 1], dates: ds });
      }
      label = selYear;
    }
    return {
      label,
      total: sumDates(u, dates),
      perAgent: perAgentForDates(u, dates),
      buckets: groups.map((g) => ({ label: g.label, tokens: sumDates(u, g.dates) })),
    };
  }
  function showBucketLabel(n: number, i: number): boolean {
    if (range === 'all') return true; // per-agent emoji bars — label every one
    if (n <= 14) return true;
    return i % Math.ceil(n / 8) === 0;
  }
  function agentPrimaryMax(r: api.AgentRoster | undefined, v: RangeView | null): number {
    return Math.max(
      1,
      ...(r?.agents ?? []).map((a) => {
        const ru = v?.perAgent?.[a.id] ?? 0;
        return ru > 0 ? ru : a.total_tokens ?? 0;
      }),
    );
  }
  function pickMonth(e: Event) {
    selMonth = (e.currentTarget as HTMLSelectElement).value;
    range = 'month';
  }
  function pickYear(e: Event) {
    selYear = (e.currentTarget as HTMLSelectElement).value;
    range = 'year';
  }

  // ── connected-systems actions ───────────────────────────────────────
  // Per-system one-time SSH password (several systems can be unconnected at
  // once). NEVER stored server-side; cleared on a successful authenticate.
  let authPw: Record<string, string> = {};
  function rolesArr(): string[] {
    const r: string[] = [];
    if (roleOpenclaw) r.push('openclaw');
    if (roleHermes) r.push('hermes');
    if (roleDgx) r.push('dgx');
    return r;
  }
  async function onAdd() {
    if (!address.trim()) return;
    adding = true;
    try {
      const res = await api.addSystem({ label, address, ssh_user: sshUser, port, roles: rolesArr() });
      if (!res.ok) {
        alert(res.err ?? 'add failed');
        return;
      }
      label = address = '';
      sshUser = 'root';
      port = 22;
      roleOpenclaw = roleHermes = roleDgx = false;
      await refresh();
      // The new card shows its always-visible masked SSH-password input itself
      // (it's not 'connected'); no open-toggle needed.
    } finally {
      adding = false;
    }
  }
  /** Authenticate: push the Pi's key using the one-time SSH password for this
   *  system. On success the card flips to connected (input disappears); on
   *  failure we surface the manual authorize command and keep the auth section. */
  async function doRegister(s: api.ConnectedSystem) {
    busyId = s.id;
    try {
      const res = await api.registerSystem(s.id, authPw[s.id] ?? '');
      if (res.ok) {
        // Connected — clear the one-time password + any stale fallback.
        delete authPw[s.id];
        authPw = authPw;
        delete fallback[s.id];
        fallback = fallback;
      } else if (res.authorize_command) {
        // Show the SSH instructions; keep the auth section visible to retry.
        fallback[s.id] = res.authorize_command;
        fallback = fallback;
      }
      await refresh();
    } finally {
      busyId = '';
    }
  }
  async function onTest(s: api.ConnectedSystem) {
    busyId = s.id;
    try {
      await api.testSystem(s.id);
      await refresh();
    } finally {
      busyId = '';
    }
  }
  async function onRemove(s: api.ConnectedSystem) {
    if (!confirm(`Remove ${s.label}?`)) return;
    await api.removeSystem(s.id);
    await refresh();
  }
  const copy = (t: string) => navigator.clipboard?.writeText(t);

  async function doPower(s: api.ConnectedSystem, action: 'shutdown' | 'reboot' | 'wake') {
    if (action !== 'wake') {
      const verb = action === 'reboot' ? 'Reboot' : 'Shut down';
      if (!confirm(`${verb} ${s.label}? This SSHes in and runs systemctl ${action === 'reboot' ? 'reboot' : 'poweroff'}.`)) return;
    }
    powerBusy = s.id;
    try {
      const r = await api.powerSystem(s.id, action, action === 'wake' ? lastMac[s.id] ?? '' : '');
      if (!r.ok) alert(r.err ?? 'power action failed');
      setTimeout(loadMetrics, 3500);
    } finally {
      powerBusy = '';
    }
  }

  // ── E4: Container Management ────────────────────────────────────────────
  /** Jump to the Containers tab focused on a system (from the sys-card icon). */
  function openContainers(sysId: string) {
    tab = 'containers';
    selectSystem(sysId);
  }
  function selectSystem(sysId: string) {
    if (cSys === sysId && cData) return;
    cSys = sysId;
    cData = null;
    cErr = '';
    composeEdit = null;
    composeMsg = '';
    // reset Easy Deploy picker + stop any in-flight progress poll
    stopDeployPoll();
    deployResult = null;
    deployErr = '';
    deployCat = null;
    deployEntries = [];
    deploySel = -1;
    depStatus = null;
    loadContainers();
    // Catalog handler decides eligibility (dgx OR docker+GPU) and returns a
    // friendly message otherwise — so probe every system.
    loadDeployCatalog();
  }
  async function loadContainers(silent = false) {
    if (!cSys) return;
    if (!silent) cLoading = true;
    cErr = '';
    try {
      const r = await api.getContainers(cSys);
      if (r.ok) cData = r;
      else cErr = r.err ?? 'failed to list containers';
    } catch (e) {
      cErr = (e as any)?.message ?? String(e);
    } finally {
      cLoading = false;
    }
  }
  async function doContainerAction(name: string, action: 'start' | 'stop' | 'restart') {
    if (action === 'stop' && !confirm(`Stop container "${name}"?`)) return;
    cBusy = name;
    try {
      const r = await api.containerAction(cSys, name, action);
      if (!r.ok) alert(r.err ?? `${action} failed`);
      await loadContainers(true);
    } finally {
      cBusy = '';
    }
  }
  async function doComposeAction(path: string, action: 'up' | 'down') {
    if (action === 'down' && !confirm(`Bring DOWN the compose project at\n${path}?`)) return;
    composeBusy = path;
    composeMsg = '';
    try {
      const r = await api.composeAction(cSys, path, action);
      if (!r.ok) alert(r.err ?? `compose ${action} failed`);
      else composeMsg = (r.out || `${action} ok`).slice(0, 400);
      await loadContainers(true);
    } finally {
      composeBusy = '';
    }
  }
  async function openComposeEditor(path: string) {
    composeBusy = path;
    composeMsg = '';
    try {
      const r = await api.getComposeFile(cSys, path);
      if (r.ok) composeEdit = { path, content: r.content ?? '' };
      else alert(r.err ?? 'could not read compose file');
    } finally {
      composeBusy = '';
    }
  }
  async function saveComposeFile() {
    if (!composeEdit) return;
    composeSaving = true;
    composeMsg = '';
    try {
      const r = await api.putComposeFile(cSys, composeEdit.path, composeEdit.content);
      if (r.ok) composeMsg = 'saved';
      else alert(r.err ?? 'save failed');
    } finally {
      composeSaving = false;
    }
  }
  /** Badge class from the robust `running` flag (State is unreliable on some boxes). */
  function cStateCls(c: api.ContainerInfo): string {
    if (c.running) return 'cst-run';
    if (c.state === 'exited' || c.state === 'dead' || c.status.trim().startsWith('Exited'))
      return 'cst-exit';
    return 'cst-other';
  }
  /** Badge label: trust `running` first, fall back to the raw state when stopped. */
  function cStateLabel(c: api.ContainerInfo): string {
    return c.running ? 'running' : c.state || 'exited';
  }
  /** "12.3%" → clamped 0-100 number for a bar width. */
  function pctNum(s?: string): number {
    if (!s) return 0;
    const n = parseFloat(s.replace('%', ''));
    return isNaN(n) ? 0 : Math.max(0, Math.min(100, n));
  }

  // ── E5: Easy Deploy ─────────────────────────────────────────────────────
  /** Short error-message coercion (kept here, not in the template — no casts). */
  function errMsg(e: unknown): string {
    return (e as { message?: string })?.message ?? String(e);
  }
  async function loadDeployCatalog() {
    deployErr = '';
    deployCatLoading = true;
    try {
      const r = await api.getDeployCatalog(cSys);
      deployCat = r; // carries err (e.g. no GPU) when !ok
      if (r.ok) {
        deployEntries = r.catalog ?? [];
        if (deploySel < 0 && deployEntries.length) pickEntry(0);
      }
    } catch (e) {
      deployErr = errMsg(e);
    } finally {
      deployCatLoading = false;
    }
  }
  /** Suggest a docker-safe deploy name from a catalog entry's id. */
  function suggestName(c: api.DeployCatalogEntry): string {
    const base = (c?.id || 'aeon-deploy').toLowerCase().replace(/[^a-z0-9._-]+/g, '-');
    return base.replace(/^-+|-+$/g, '') || 'aeon-deploy';
  }
  /** Pick a catalog entry → pre-fill the name + the highlighted flags. */
  function pickEntry(i: number) {
    deploySel = i;
    const e = deployEntries[i];
    if (!e) return;
    deployName = suggestName(e);
    const t = e.template_flags;
    dfModelLen = t.max_model_len || null;
    dfGpu = t.gpu || '1';
    // VRAM% slider ← template's gpu_mem_util (0.0–1.0). Default 70% (never 100).
    dfGpuPct = Math.round(((t.gpu_mem_util && t.gpu_mem_util > 0 ? t.gpu_mem_util : 0.7)) * 100);
    if (!(dfGpuPct > 0 && dfGpuPct <= 100)) dfGpuPct = 70;
    dfMaxBatchTok = t.max_num_batched_tokens || null;
    dfMaxSeqs = t.max_num_seqs || null;
    dfExtra = t.extra || '';
    deployResult = null;
    depStatus = null;
    deployErr = '';
  }
  /** Is this catalog entry already deployed/available on the box? */
  function entryInstalled(e: api.DeployCatalogEntry): boolean {
    const models = deployCat?.installed_models ?? [];
    if (e.kind === 'imagegen') {
      // ComfyUI: match by the image name appearing among installed containers.
      return (deployCat?.installed_containers ?? []).some((c) =>
        (c.image || '').includes('comfyui'),
      );
    }
    return models.some((m) => m === e.model || m.toLowerCase() === e.model.toLowerCase());
  }
  function kindLabel(k: string): string {
    return (
      { llm: 'LLM', 'llm-gguf': 'LLM (GGUF)', imagegen: 'image-gen', tts: 'TTS', embedding: 'embedding' }[k] ?? k
    );
  }
  /** Container image → short "name:tag" chip text. */
  function imageChip(img: string): string {
    const parts = img.split('/');
    return parts[parts.length - 1] || img;
  }
  function stopDeployPoll() {
    depPolling = false;
    if (depPollTimer) {
      clearTimeout(depPollTimer);
      depPollTimer = null;
    }
  }
  /** Poll /deploy/status every ~1.5s until running/failed (bounded by ~5min). */
  async function pollDeployStatus(name: string, startedMs = Date.now()) {
    if (!depPolling) return;
    try {
      const s = await api.getDeployStatus(cSys, name);
      if (s.ok) depStatus = s;
      if (s.ok && s.done) {
        stopDeployPoll();
        await loadContainers(true);
        return;
      }
    } catch {
      /* transient — keep polling */
    }
    // Safety bound: give up after 5 minutes of polling.
    if (Date.now() - startedMs > 5 * 60_000) {
      stopDeployPoll();
      return;
    }
    depPollTimer = setTimeout(() => pollDeployStatus(name, startedMs), 1500);
  }
  async function doDeploy() {
    const entry = deployEntries[deploySel];
    if (!entry || !deployName.trim()) return;
    if (
      !confirm(
        `Deploy "${entry.model}"?\n\nThis writes a compose to ~/aeon-deploy/${deployName}/ on ` +
          `${cSysObj?.label} and PULLS ${entry.container_image} (can be multi-GB), then starts it.\n\nContinue?`,
      )
    )
      return;
    deployBusy = true;
    deployErr = '';
    deployResult = null;
    depStatus = null;
    stopDeployPoll();
    try {
      const r = await api.deployImage(cSys, {
        model_id: entry.id,
        name: deployName,
        kind: entry.kind,
        flags: {
          max_model_len: dfModelLen,
          gpu: dfGpu,
          gpu_mem_util: Math.min(100, Math.max(1, dfGpuPct)) / 100, // % VRAM slider → 0.0–1.0
          max_num_batched_tokens: dfMaxBatchTok,
          max_num_seqs: dfMaxSeqs,
          extra: dfExtra,
        },
      });
      deployResult = r;
      if (!r.ok) {
        deployErr = r.err ?? 'deploy failed';
      } else {
        // Deploy kicked off in the background — start the progress poll.
        depStatus = { ok: true, name: r.name, phase: r.phase ?? 'writing', percent: 10, done: false };
        depPolling = true;
        pollDeployStatus(r.name ?? deployName);
      }
    } catch (e) {
      deployErr = errMsg(e);
    } finally {
      deployBusy = false;
    }
  }
  /** Label + accent colour for a progress phase. */
  function phaseLabel(p?: string): string {
    return (
      {
        writing: 'writing compose…',
        pulling: 'pulling image…',
        starting: 'starting container…',
        running: 'running ✓',
        failed: 'failed ✗',
      }[p ?? ''] ?? p ?? ''
    );
  }
  function phaseColor(p?: string): string {
    if (p === 'failed') return '#f87171';
    if (p === 'running') return '#34d399';
    return '#a78bfa';
  }

  // ── agent detail + provisioning ─────────────────────────────────────
  async function openDetail(sysId: string, a: api.AgentInfo) {
    detailSys = sysId;
    detailAgent = a;
    detail = null;
    newToken = configChange = dropMsg = newPrivKey = sshCmd = sshDropMsg = '';
    grantSudo = false;
    avatar = null;
    avatarMsg = '';
    corpus = null;
    corpusFile = null;
    corpusFilePath = '';
    corpusFilter = '';
    voice = null;
    skillBusy = '';
    skillResult = null;
    customSkillName = '';
    customSkillFile = null;
    personaFiles = { soul: null, identity: null };
    personaDraft = { soul: '', identity: '' };
    personaMsg = { soul: '', identity: '' };
    detailLoading = true;
    try {
      detail = await api.getAgentDetail(sysId, a.id);
    } catch (e) {
      detail = { ok: false, err: (e as any)?.message ?? String(e) };
    } finally {
      detailLoading = false;
    }
    // Matrix avatar + voice load independently — a missing creds file or a
    // slow extra SSH shouldn't block the rest of the detail panel.
    api
      .getAgentAvatar(sysId, a.id)
      .then((r) => (avatar = r))
      .catch((e) => (avatar = { ok: false, err: (e as any)?.message ?? String(e) }));
    api
      .getAgentVoice(sysId, a.id)
      .then((r) => (voice = r))
      .catch((e) => (voice = { ok: false, err: (e as any)?.message ?? String(e) }));
  }
  function closeDetail() {
    detailAgent = null;
    detail = null;
    newToken = '';
    avatar = null;
    corpus = null;
    corpusFile = null;
    voice = null;
  }
  // ── E1: corpus browse/view ──────────────────────────────────────────
  async function loadCorpus() {
    if (!detailAgent || corpusLoading) return;
    corpusLoading = true;
    corpusFile = null;
    corpusFilePath = '';
    try {
      corpus = await api.getAgentCorpus(detailSys, detailAgent.id);
    } catch (e) {
      corpus = { ok: false, err: (e as any)?.message ?? String(e) };
    } finally {
      corpusLoading = false;
    }
  }
  async function viewCorpusFile(path: string) {
    if (!detailAgent) return;
    corpusFilePath = path;
    corpusFile = null;
    corpusFileLoading = true;
    try {
      corpusFile = await api.getAgentCorpusFile(detailSys, detailAgent.id, path);
    } catch (e) {
      corpusFile = { ok: false, err: (e as any)?.message ?? String(e) };
    } finally {
      corpusFileLoading = false;
    }
  }
  function closeCorpusFile() {
    corpusFile = null;
    corpusFilePath = '';
  }
  function fmtBytes(n?: number): string {
    n = n ?? 0;
    if (n >= 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + 'M';
    if (n >= 1024) return (n / 1024).toFixed(1) + 'k';
    return n + 'B';
  }
  $: corpusFiles = (corpus?.files ?? []).filter(
    (f) => !corpusFilter.trim() || f.path.toLowerCase().includes(corpusFilter.toLowerCase()),
  );
  // ── F7a: persona files (Soul + Identity) view/edit ──────────────────
  async function loadPersonaFile(which: api.PersonaWhich) {
    if (!detailAgent || personaLoading[which]) return;
    personaLoading = { ...personaLoading, [which]: true };
    personaMsg = { ...personaMsg, [which]: '' };
    try {
      const r = await api.getAgentPersonaFile(detailSys, detailAgent.id, which);
      personaFiles = { ...personaFiles, [which]: r };
      personaDraft = { ...personaDraft, [which]: r.ok ? (r.content ?? '') : personaDraft[which] };
      if (!r.ok) personaMsg = { ...personaMsg, [which]: r.err ?? 'load failed' };
    } catch (e) {
      const msg = (e as any)?.message ?? String(e);
      personaFiles = { ...personaFiles, [which]: { ok: false, err: msg } };
      personaMsg = { ...personaMsg, [which]: msg };
    } finally {
      personaLoading = { ...personaLoading, [which]: false };
    }
  }
  async function savePersonaFile(which: api.PersonaWhich) {
    if (!detailAgent || personaSaving[which]) return;
    personaSaving = { ...personaSaving, [which]: true };
    personaMsg = { ...personaMsg, [which]: '' };
    try {
      const r = await api.putAgentPersonaFile(detailSys, detailAgent.id, which, personaDraft[which]);
      if (r.ok) {
        personaMsg = { ...personaMsg, [which]: `saved (${r.bytes_written ?? 0} bytes)` };
        // refresh the on-disk view so exists/size reflect the write
        personaFiles = {
          ...personaFiles,
          [which]: { ...(personaFiles[which] ?? { ok: true }), ok: true, exists: true, content: personaDraft[which] },
        };
      } else {
        personaMsg = { ...personaMsg, [which]: r.err ?? 'save failed' };
      }
    } catch (e) {
      personaMsg = { ...personaMsg, [which]: (e as any)?.message ?? String(e) };
    } finally {
      personaSaving = { ...personaSaving, [which]: false };
    }
  }
  // ── F7b: deploy a new persona ───────────────────────────────────────
  function openNewPersona(sysId: string) {
    npSysId = sysId;
    np = { id: '', name: '', emoji: '', identity: '', soul: '', voice: '', corpus_seed: '', model: '' };
    npResult = null;
    showNewPersona = true;
  }
  function closeNewPersona() {
    showNewPersona = false;
    npResult = null;
  }
  async function submitNewPersona() {
    if (npBusy) return;
    const id = (np.id || '').trim().toLowerCase();
    if (!/^[a-z][a-z0-9_-]*$/.test(id)) {
      npResult = { ok: false, err: 'id must be lowercase, start with a letter, and use only [a-z0-9_-]' };
      return;
    }
    npBusy = true;
    npResult = null;
    try {
      npResult = await api.createPersona(npSysId, { ...np, id });
      // on success, refresh that system's roster so the new persona shows up
      if (npResult.ok) {
        api
          .getSystemAgents(npSysId)
          .then((r) => (agents = { ...agents, [npSysId]: r }))
          .catch(() => {});
      }
    } catch (e) {
      npResult = { ok: false, err: (e as any)?.message ?? String(e) };
    } finally {
      npBusy = false;
    }
  }
  // ── E1: add-skill ───────────────────────────────────────────────────
  // Skills already on the agent (so quick-add chips can hide ones it has).
  $: agentSkillSet = new Set(detail?.skills ?? []);
  $: quickAddSkills = (detail?.available_skills ?? []).filter((s) => !agentSkillSet.has(s));
  async function quickAddSkill(name: string) {
    if (!detailAgent || skillBusy) return;
    skillBusy = name;
    skillResult = null;
    try {
      const r = await api.addAgentSkill(detailSys, detailAgent.id, name, 'existing');
      skillResult = r;
      if (!r.ok) alert(r.err ?? 'add skill failed');
    } catch (e) {
      skillResult = { ok: false, err: (e as any)?.message ?? String(e) };
    } finally {
      skillBusy = '';
    }
  }
  function onSkillFile(e: Event) {
    const f = (e.currentTarget as HTMLInputElement).files?.[0] ?? null;
    customSkillFile = f;
    // Default the skill name from the filename (strip extension) if blank.
    if (f && !customSkillName.trim()) {
      customSkillName = f.name.replace(/\.(md|tar\.gz|tgz|tar)$/i, '');
    }
  }
  /** Classify the chosen file as a SKILL.md vs a tar archive. */
  function skillKindFor(f: File): 'md' | 'tar' {
    return /\.(tar\.gz|tgz|tar)$/i.test(f.name) ? 'tar' : 'md';
  }
  async function uploadCustomSkill() {
    if (!detailAgent || !customSkillFile || !customSkillName.trim() || skillBusy) return;
    const name = customSkillName.trim();
    skillBusy = name;
    skillResult = null;
    try {
      const kind = skillKindFor(customSkillFile);
      const b64 = await fileToB64(customSkillFile);
      const r = await api.addAgentSkill(detailSys, detailAgent.id, name, kind, b64);
      skillResult = r;
      if (r.ok) {
        customSkillFile = null;
        // refresh the detail so available_skills picks up the new dir
        detail = await api.getAgentDetail(detailSys, detailAgent.id);
      } else {
        alert(r.err ?? 'upload failed');
      }
    } catch (e) {
      skillResult = { ok: false, err: (e as any)?.message ?? String(e) };
    } finally {
      skillBusy = '';
    }
  }
  // ── E1: profile photo → Matrix avatar ───────────────────────────────
  async function onAvatarPick(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file || !detailAgent) return;
    if (file.size > 8 * 1024 * 1024) {
      avatarMsg = 'image too large (max 8 MB)';
      input.value = '';
      return;
    }
    avatarBusy = true;
    avatarMsg = '';
    try {
      const b64 = await fileToB64(file);
      const r = await api.setAgentAvatar(detailSys, detailAgent.id, b64, file.type || 'image/png');
      if (r.ok) {
        avatar = r;
        avatarMsg = 'avatar updated on Matrix';
      } else {
        avatarMsg = r.err ?? 'avatar update failed';
      }
    } catch (e) {
      avatarMsg = (e as any)?.message ?? String(e);
    } finally {
      avatarBusy = false;
      input.value = '';
    }
  }
  /** Read a File into bare base64 (no data: prefix). */
  function fileToB64(file: File): Promise<string> {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => {
        const s = String(r.result);
        const comma = s.indexOf(',');
        resolve(comma >= 0 ? s.slice(comma + 1) : s);
      };
      r.onerror = () => reject(new Error('read failed'));
      r.readAsDataURL(file);
    });
  }
  async function doProvision() {
    if (!detailAgent) return;
    provisioning = true;
    try {
      const r = await api.provisionAgent(detailSys, detailAgent.id, window.location.origin + '/api');
      if (r.ok) {
        newToken = r.token ?? '';
        configChange = r.config_change ?? '';
        dropMsg = r.dropped
          ? `access file dropped → ${r.dropped}`
          : r.drop_err
            ? `skill-drop failed: ${r.drop_err}`
            : '';
        detail = await api.getAgentDetail(detailSys, detailAgent.id);
      } else {
        alert(r.err ?? 'provision failed');
      }
    } finally {
      provisioning = false;
    }
  }
  async function doRevoke() {
    if (!detailAgent || !confirm("Revoke this agent's API key + remove its access file?")) return;
    provisioning = true;
    try {
      await api.deprovisionAgent(detailSys, detailAgent.id);
      newToken = '';
      detail = await api.getAgentDetail(detailSys, detailAgent.id);
    } finally {
      provisioning = false;
    }
  }
  const SUDO_WARN =
    'Enabling sudo grants this agent FULL ADMIN (passwordless root) on the Pi. Only do this if absolutely necessary. Continue?';
  async function doGrantSsh() {
    if (!detailAgent) return;
    if (grantSudo && !confirm(SUDO_WARN)) return;
    sshBusy = true;
    try {
      const r = await api.grantSsh(detailSys, detailAgent.id, grantSudo);
      if (r.ok) {
        newPrivKey = r.private_key ?? '';
        sshCmd = r.ssh_command ?? '';
        sshDropMsg = r.dropped
          ? `key dropped → ${r.dropped}`
          : r.drop_err
            ? `key-drop failed: ${r.drop_err}`
            : '';
        detail = await api.getAgentDetail(detailSys, detailAgent.id);
      } else {
        alert(r.err ?? 'grant failed');
      }
    } finally {
      sshBusy = false;
    }
  }
  function onSudoToggle(e: Event) {
    const on = (e.currentTarget as HTMLInputElement).checked;
    doToggleSudo(on);
  }
  async function doToggleSudo(on: boolean) {
    if (!detailAgent) return;
    if (on && !confirm(SUDO_WARN)) {
      detail = await api.getAgentDetail(detailSys, detailAgent.id); // revert the checkbox
      return;
    }
    sshBusy = true;
    try {
      await api.toggleSshAdmin(detailSys, detailAgent.id, on);
      detail = await api.getAgentDetail(detailSys, detailAgent.id);
    } finally {
      sshBusy = false;
    }
  }
  async function doRevokeSsh() {
    if (!detailAgent || !confirm("Revoke this agent's SSH access (delete the aeon-agent user + key)?")) return;
    sshBusy = true;
    try {
      await api.revokeSsh(detailSys, detailAgent.id);
      newPrivKey = '';
      detail = await api.getAgentDetail(detailSys, detailAgent.id);
    } finally {
      sshBusy = false;
    }
  }
  function badgeCls(status: string): string {
    if (status === 'connected' || status === 'online')
      return 'bg-live-900/40 text-live-300 border border-live-500/40';
    if (status === 'unreachable')
      return 'bg-red-900/30 text-red-300 border border-red-500/40';
    return 'bg-ink-800 text-zinc-400 border border-ink-700';
  }
  function tabCls(t: string): string {
    return tab === t
      ? 'bg-ink-900 text-cursed-300 border-x border-t border-ink-700'
      : 'text-zinc-500 hover:text-zinc-300 border border-transparent';
  }
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">Agent Dash</span>
    </div>
  </header>

  <div class="flex gap-1 px-5 pt-2 border-b border-ink-800 bg-ink-900/40">
    <button class="px-3 py-1.5 text-xs font-mono rounded-t {tabCls('overview')}"
            on:click={() => { tab = 'overview'; loadMetrics(); }}>Overview</button>
    <button class="px-3 py-1.5 text-xs font-mono rounded-t {tabCls('containers')}"
            on:click={() => { tab = 'containers'; if (!cSys && systems.length) selectSystem(systems[0].id); else if (cSys) loadContainers(); }}>Containers</button>
    <button class="px-3 py-1.5 text-xs font-mono rounded-t {tabCls('terminal')}"
            on:click={() => { tab = 'terminal'; refitAll(); }}>
      Terminal{#if panes.length}<span class="tab-count">{panes.length}</span>{/if}</button>
    <button class="px-3 py-1.5 text-xs font-mono rounded-t {tabCls('systems')}"
            on:click={() => (tab = 'systems')}>Connected Systems</button>
  </div>

  <main class="flex-1 overflow-auto">
    {#if tab === 'overview'}
      <div class="p-5 max-w-5xl mx-auto w-full space-y-6">
        {#if loading}<p class="text-zinc-500 text-sm">loading…</p>{/if}
        {#if err}<p class="text-red-400 text-sm">{err}</p>{/if}

        <!-- ── SYSTEMS ───────────────────────────────────────────── -->
        <div class="flex items-center justify-between">
          <h2 class="section-title">Systems</h2>
          <button class="refresh-btn" on:click={loadMetrics} disabled={metricsLoading}>
            <span class:spin={metricsLoading}>↻</span> {metricsLoading ? 'refreshing' : 'refresh'}
          </button>
        </div>
        {#if !systems.length}
          <p class="text-zinc-500 text-xs">No systems yet — add them in the
            <button class="underline text-cursed-300" on:click={() => (tab = 'systems')}>Connected Systems</button> tab.</p>
        {/if}

        <div class="sys-grid">
          {#each systems as s (s.id)}
            {@const m = metrics[s.id]}
            <div class="sys-card">
              <div class="sys-head">
                <span class="sys-icon">{roleIcon(s.roles)}</span>
                <div class="sys-id">
                  <div class="sys-label">{s.label}</div>
                  <div class="sys-host">{m?.host || s.address} · {s.roles.join(' / ') || 'system'}</div>
                </div>
                <span class="pill" class:on={m?.reachable} class:off={m && !m.reachable}>
                  {m ? (m.reachable ? 'online' : 'offline') : '…'}
                </span>
                <button class="term-btn" title="Open an SSH terminal to this system"
                        on:click|stopPropagation={() => openTerminal(s.id)}>&gt;_</button>
                <button class="docker-btn" title="Manage containers on this system"
                        on:click|stopPropagation={() => openContainers(s.id)}>🐳</button>
              </div>

              {#if m?.reachable}
                {#if m.gpus?.length}
                  {#each m.gpus as g}
                    {@const uv = unifiedVram(s, m)}
                    <div class="gpu">
                      <div class="gpu-top">
                        <span class="gpu-name">{g.name}</span>
                        <span class="gpu-meta" style="color:{utilColor(g.util)}">{g.util}% · {g.temp}°C</span>
                      </div>
                      <div class="gauge">
                        <div class="gauge-fill" style="width:{clampPct(g.util)}%; background:{utilColor(g.util)}; box-shadow:0 0 10px {utilColor(g.util)}88"></div>
                      </div>
                      {#if uv}
                        <div class="vram-row"><span>VRAM <span class="unified">unified</span></span><span>{gb(uv.used)} / {gb(uv.total)} GB</span></div>
                        <div class="gauge sm"><div class="gauge-fill" style="width:{uv.pct}%; background:#a78bfa; box-shadow:0 0 8px #a78bfa66"></div></div>
                      {:else if hasNvVram(g)}
                        <div class="vram-row"><span>VRAM</span><span>{g.mem_used} / {g.mem_total} MB</span></div>
                        <div class="gauge sm"><div class="gauge-fill" style="width:{vramPct(g)}%; background:#a78bfa; box-shadow:0 0 8px #a78bfa66"></div></div>
                      {/if}
                    </div>
                  {/each}
                {/if}

                <!-- CPU + RAM bars (non-DGX; on a DGX the unified VRAM bar above
                     already covers memory, since GB10 system RAM *is* the VRAM). -->
                {#if !isDgx(s)}
                  {@const pm = parseMem(m.mem)}
                  <div class="sys-bars">
                    {#if m.cpu}
                      <div class="sbar">
                        <div class="sbar-top"><span>CPU</span><span style="color:{utilColor(m.cpu)}">{m.cpu}%</span></div>
                        <div class="gauge sm"><div class="gauge-fill" style="width:{clampPct(m.cpu)}%; background:{utilColor(m.cpu)}; box-shadow:0 0 8px {utilColor(m.cpu)}88"></div></div>
                      </div>
                    {/if}
                    {#if pm}
                      <div class="sbar">
                        <div class="sbar-top"><span>RAM</span><span style="color:{utilColor(pm.pct)}">{gb(pm.used)} / {gb(pm.total)} GB · {pm.pct}%</span></div>
                        <div class="gauge sm"><div class="gauge-fill" style="width:{clampPct(pm.pct)}%; background:{utilColor(pm.pct)}; box-shadow:0 0 8px {utilColor(pm.pct)}88"></div></div>
                      </div>
                    {/if}
                  </div>
                {/if}

                <div class="tiles">
                  <div class="tile"><div class="tile-num">{m.load?.split(' ')[0] || '—'}</div><div class="tile-lbl">load</div></div>
                  <div class="tile"><div class="tile-num">{memTile(s, m)}</div><div class="tile-lbl">{isDgx(s) ? 'unified' : 'mem'}</div></div>
                  <div class="tile"><div class="tile-num">{m.containers?.length ?? 0}</div><div class="tile-lbl">containers</div></div>
                </div>
                {#if m.containers?.length}
                  <div class="chips">{#each m.containers as c}<span class="chip">{c}</span>{/each}</div>
                {/if}
              {:else if m}
                <div class="errline">{m.err || 'unreachable'}</div>
              {:else}
                <div class="errline dim">gathering…</div>
              {/if}
              <div class="sys-power">
                <button class="pw-btn" on:click={() => doPower(s, 'reboot')} disabled={powerBusy === s.id} title="Reboot (ssh systemctl reboot)">⟳ reboot</button>
                <button class="pw-btn" on:click={() => doPower(s, 'shutdown')} disabled={powerBusy === s.id} title="Shut down (ssh systemctl poweroff)">⏻ shutdown</button>
                <button class="pw-btn wake" on:click={() => doPower(s, 'wake')} disabled={powerBusy === s.id || !lastMac[s.id]}
                        title={lastMac[s.id] ? `Wake-on-LAN → ${lastMac[s.id]}` : 'WoL needs a MAC (captured while the system is online)'}>⏾ wake</button>
              </div>
            </div>
          {/each}
        </div>

        <!-- ── USAGE BANNER + PANTHEON (per OpenClaw system) ──────── -->
        {#if openclawSystems.length}
          {#each openclawSystems as s (s.id)}
            {@const u = usage[s.id]}
            {@const r = agents[s.id]}
            {@const v = u ? view(u, r) : null}
            {@const sum = rosterSummary(r)}

            {#if u}
              <div class="usage-banner">
                <div class="ub-head">
                  <div class="min-w-0">
                    <div class="ub-title">Token Usage <span class="ub-sub">· {s.label}</span></div>
                    <div class="ub-meta">
                      {#if u.first_day}tracked since {u.first_day} · {trackedDays(u)}d{:else}building history…{/if}
                      <span class="dot-sep">·</span> gateway all-time <b>{fmtTok(u.gateway_total)}</b>
                    </div>
                  </div>
                  <div class="ub-total">
                    <div class="ub-total-num">{fmtTok(v?.total ?? 0)}</div>
                    <div class="ub-total-lbl">{v?.label ?? ''}</div>
                  </div>
                </div>

                <div class="ub-controls">
                  <button class="ub-btn" class:active={range === 'all'} on:click={() => (range = 'all')}>All-time</button>
                  <button class="ub-btn" class:active={range === '30d'} on:click={() => (range = '30d')}>30 days</button>
                  <button class="ub-btn" class:active={range === '90d'} on:click={() => (range = '90d')}>90 days</button>
                  <button class="ub-btn" class:active={range === '1y'} on:click={() => (range = '1y')}>1 year</button>
                  <select class="ub-sel" class:active={range === 'month'} value={range === 'month' ? selMonth : ''}
                          on:change={pickMonth}>
                    <option value="" disabled>Month…</option>
                    {#each monthsBetween(u.first_day, u.today) as ym}<option value={ym}>{monthLabel(ym)}</option>{/each}
                  </select>
                  <select class="ub-sel" class:active={range === 'year'} value={range === 'year' ? selYear : ''}
                          on:change={pickYear}>
                    <option value="" disabled>Year…</option>
                    {#each yearsBetween(u.first_day, u.today) as y}<option value={y}>{y}</option>{/each}
                  </select>
                </div>

                <div class="ub-chart">
                  {#if v && v.buckets.some((b) => b.tokens > 0)}
                    {@const mx = Math.max(1, ...v.buckets.map((x) => x.tokens))}
                    {#each v.buckets as b, i}
                      <div class="ub-bar-wrap" title="{b.label}: {fmtTok(b.tokens)} tokens">
                        <div class="ub-bar" style="height:{Math.max(2, (b.tokens / mx) * 100)}%; opacity:{b.tokens > 0 ? 1 : 0.3}"></div>
                        <div class="ub-bar-lbl">{showBucketLabel(v.buckets.length, i) ? b.label : ''}</div>
                      </div>
                    {/each}
                  {:else if range === 'all'}
                    <div class="ub-empty">{r ? 'No per-agent usage to chart yet.' : 'loading roster…'}</div>
                  {:else}
                    <div class="ub-empty">No tracked daily history in this range yet — switch to <button class="ub-link" on:click={() => (range = 'all')}>All-time</button> for current totals. The Pi samples every 10&nbsp;min, so 30d / 90d / 1y fill in as days pass.</div>
                  {/if}
                </div>
              </div>
            {/if}

            <div class="space-y-3">
              <div class="flex items-baseline justify-between flex-wrap gap-2">
                <h2 class="section-title">Pantheon <span class="text-zinc-600 font-normal">· {s.label}</span></h2>
                <div class="flex items-baseline gap-2">
                  {#if r?.reachable}
                    <div class="roster-stats">
                      <span><b>{sum.count}</b> agents</span>
                      <span class="dot-sep">·</span>
                      <span class="text-live-300"><b>{sum.active}</b> active</span>
                    </div>
                    <!-- F7b: deploy a new persona on this gateway. -->
                    <button class="new-persona-btn" title="Deploy a new agent persona" on:click={() => openNewPersona(s.id)}>
                      <span class="np-plus">+</span> New persona
                    </button>
                  {:else if r}
                    <span class="text-amber-300 text-[11px] font-mono">{r.err || 'roster unavailable'}</span>
                  {:else}
                    <span class="text-zinc-600 text-[11px] font-mono">loading roster…</span>
                  {/if}
                </div>
              </div>

              {#if r?.reachable}
                <div class="agent-grid">
                  {#each sortedAgents(r) as a (a.id)}
                    {@const st = agentStatus(a)}
                    {@const ru = v?.perAgent?.[a.id] ?? 0}
                    {@const pv = ru > 0 ? ru : a.total_tokens ?? 0}
                    <div class="agent agent-click" class:is-default={a.is_default} class:is-working={a.working || a.on_call}
                         role="button" tabindex="0" title="Open agent detail"
                         on:click={() => openDetail(s.id, a)}
                         on:keydown={(e) => (e.key === 'Enter' || e.key === ' ') && openDetail(s.id, a)}>
                      <div class="agent-top">
                        <span class="agent-emoji">{a.emoji || '🤖'}</span>
                        <div class="agent-id">
                          <div class="agent-name">{a.name}{#if a.is_default}<span class="def-star" title="default agent">★</span>{/if}</div>
                          <div class="agent-model">{a.model || '—'}</div>
                        </div>
                        <span class="dot" class:pulse={st.pulse} style="background:{st.color}; box-shadow:0 0 7px {st.color}"></span>
                      </div>
                      <div class="agent-status" style="color:{st.color}">{st.label}</div>
                      <div class="agent-stats">
                        <div><span class="as-num">{fmtTok(pv)}</span><span class="as-lbl" title={ru > 0 ? 'locally-tracked usage in the selected range' : 'gateway running total'}>{ru > 0 ? rangeShort() : 'gateway'}</span></div>
                        <div><span class="as-num">{a.sessions ?? 0}</span><span class="as-lbl">sessions</span></div>
                        <div><span class="as-num">{a.working ? (a.tok_s ?? 0).toFixed(0) : fmtAgo(a.last_seen_s_ago)}</span><span class="as-lbl">{a.working ? 'tok/s' : 'seen'}</span></div>
                      </div>
                      <div class="tok-bar"><div style="width:{(pv / agentPrimaryMax(r, v)) * 100}%; background:{st.color}"></div></div>
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      </div>
    {/if}

    <!-- ── CONTAINERS (E4) + EASY DEPLOY (E5) ──────────────────────────── -->
    {#if tab === 'containers'}
      <div class="p-5 max-w-5xl mx-auto w-full space-y-5">
        {#if !systems.length}
          <p class="text-zinc-500 text-xs">No systems yet — add them in the
            <button class="underline text-cursed-300" on:click={() => (tab = 'systems')}>Connected Systems</button> tab.</p>
        {:else}
          <!-- system selector -->
          <div class="flex items-center justify-between flex-wrap gap-2">
            <div class="c-sel">
              {#each systems as s (s.id)}
                <button class="c-sel-btn" class:active={cSys === s.id} on:click={() => selectSystem(s.id)}>
                  <span>{roleIcon(s.roles)}</span> {s.label}
                </button>
              {/each}
            </div>
            <button class="refresh-btn" on:click={() => loadContainers()} disabled={cLoading || !cSys}>
              <span class:spin={cLoading}>↻</span> {cLoading ? 'loading' : 'refresh'}
            </button>
          </div>

          {#if cErr}<p class="text-red-400 text-xs font-mono">{cErr}</p>{/if}

          {#if cData}
            <div class="text-[11px] font-mono text-zinc-500">
              {cData.running ?? 0} running · {cData.total ?? 0} total
              <span class="dot-sep">·</span> live stats refresh every 5s
            </div>

            <!-- container list -->
            {#if cData.containers?.length}
              <div class="ctr-list">
                {#each cData.containers as c (c.name)}
                  <div class="ctr">
                    <div class="ctr-main">
                      <span class="ctr-state {cStateCls(c)}">{cStateLabel(c)}</span>
                      <div class="ctr-id">
                        <div class="ctr-name">{c.name}</div>
                        <div class="ctr-img" title={c.image}>{c.image}</div>
                      </div>
                      <div class="ctr-actions">
                        {#if c.running}
                          <button class="ctr-btn" disabled={cBusy === c.name}
                                  on:click={() => doContainerAction(c.name, 'restart')} title="Restart">⟳</button>
                          <button class="ctr-btn stop" disabled={cBusy === c.name}
                                  on:click={() => doContainerAction(c.name, 'stop')} title="Stop">■</button>
                        {:else}
                          <button class="ctr-btn start" disabled={cBusy === c.name}
                                  on:click={() => doContainerAction(c.name, 'start')} title="Start">▶</button>
                        {/if}
                      </div>
                    </div>
                    <div class="ctr-status">{c.status}{#if c.compose_project} · <span class="ctr-proj">{c.compose_project}</span>{/if}</div>
                    {#if c.ports}<div class="ctr-ports" title={c.ports}>{c.ports}</div>{/if}
                    {#if c.stats}
                      <div class="ctr-meters">
                        <div class="meter">
                          <div class="meter-top"><span>CPU</span><span>{c.stats.cpu}</span></div>
                          <div class="gauge sm"><div class="gauge-fill" style="width:{pctNum(c.stats.cpu)}%; background:#60a5fa; box-shadow:0 0 8px #60a5fa66"></div></div>
                        </div>
                        <div class="meter">
                          <div class="meter-top"><span>MEM</span><span title={c.stats.mem_full}>{c.stats.mem} · {c.stats.mem_pct}</span></div>
                          <div class="gauge sm"><div class="gauge-fill" style="width:{pctNum(c.stats.mem_pct)}%; background:#a78bfa; box-shadow:0 0 8px #a78bfa66"></div></div>
                        </div>
                      </div>
                    {/if}
                  </div>
                {/each}
              </div>
            {:else if !cLoading}
              <p class="text-zinc-600 text-xs">No containers on this system.</p>
            {/if}

            <!-- compose files -->
            {#if cData.compose?.length}
              <div class="space-y-2">
                <h2 class="section-title">Compose projects</h2>
                <div class="cmp-list">
                  {#each cData.compose as f (f.path)}
                    <div class="cmp">
                      <span class="cmp-dot" class:up={f.up}></span>
                      <span class="cmp-path" title={f.path}>{f.path}</span>
                      <span class="cmp-state">{f.up ? 'up' : 'down'}</span>
                      <div class="cmp-actions">
                        <button class="ctr-btn start" disabled={composeBusy === f.path}
                                on:click={() => doComposeAction(f.path, 'up')} title="docker compose up -d">up</button>
                        <button class="ctr-btn stop" disabled={composeBusy === f.path}
                                on:click={() => doComposeAction(f.path, 'down')} title="docker compose down">down</button>
                        <button class="ctr-btn" disabled={composeBusy === f.path}
                                on:click={() => openComposeEditor(f.path)} title="Edit compose file">edit</button>
                      </div>
                    </div>
                  {/each}
                </div>
                {#if composeMsg}<pre class="cmp-out">{composeMsg}</pre>{/if}
              </div>
            {/if}

            <!-- compose editor -->
            {#if composeEdit}
              <div class="cmp-editor">
                <div class="flex items-center justify-between gap-2">
                  <code class="text-[11px] font-mono text-cursed-300 truncate">{composeEdit.path}</code>
                  <div class="flex gap-2">
                    <button class="btn-primary text-xs" on:click={saveComposeFile} disabled={composeSaving}>{composeSaving ? 'saving…' : 'save'}</button>
                    <button class="btn text-xs" on:click={() => (composeEdit = null)}>close</button>
                  </div>
                </div>
                <textarea class="cmp-ta" spellcheck="false" bind:value={composeEdit.content}></textarea>
                {#if composeMsg}<div class="text-[11px] font-mono text-live-300">{composeMsg}</div>{/if}
              </div>
            {/if}
          {:else if cLoading}
            <p class="text-zinc-500 text-sm">loading containers…</p>
          {/if}

          <!-- ── E5: Easy Deploy — model + container picker ── -->
          {#if cSys}
            <section class="deploy">
              <div class="flex items-baseline justify-between flex-wrap gap-2">
                <h2 class="section-title">Easy Deploy
                  <span class="text-zinc-600 font-normal">· model + container → {cSysObj?.label}</span>
                </h2>
                {#if deployCat?.ok}
                  <span class="dep-via">{deployCat.allowed_via === 'dgx' ? 'dgx' : 'docker + gpu'}</span>
                {/if}
              </div>

              {#if deployCatLoading && !deployCat}
                <p class="text-zinc-500 text-xs">probing system for docker + GPU…</p>
              {:else if deployCat && !deployCat.ok}
                <!-- not eligible (no GPU / no docker) -->
                <p class="dep-noteligible">{deployCat.err}</p>
              {:else if deployCat?.ok}
                <p class="agd-dim">
                  The easy button: pick a model (paired with its serving container), tune the highlighted
                  flags, and hit <b>Deploy</b>. The image pull + start runs in the background with a progress bar.
                </p>
                {#if deployErr}<p class="text-red-400 text-xs font-mono">{deployErr}</p>{/if}

                <!-- already deployed / available on the box -->
                {#if (deployCat.installed_models?.length ?? 0) > 0 || (deployCat.installed_containers?.length ?? 0) > 0}
                  <div class="dep-installed">
                    <div class="dep-sub">Already on this box</div>
                    <div class="dep-inst-chips">
                      {#each (deployCat.installed_models ?? []).slice(0, 24) as m (m)}
                        <span class="dep-chip model" title="detected model">{m}</span>
                      {/each}
                      {#each (deployCat.installed_containers ?? []) as c (c.name)}
                        <span class="dep-chip ctr" class:run={c.running} title={c.image}>
                          <span class="dep-cdot" class:run={c.running}></span>{c.name}
                        </span>
                      {/each}
                    </div>
                  </div>
                {/if}

                <!-- catalog cards: model + paired container + kind -->
                <div class="dep-sub">Deploy a model</div>
                <div class="dep-cards">
                  {#each deployEntries as e, i (e.id)}
                    <button
                      class="dep-card"
                      class:active={deploySel === i}
                      on:click={() => pickEntry(i)}
                    >
                      <div class="dep-card-top">
                        <span class="dep-kind-badge {e.kind}">{kindLabel(e.kind)}</span>
                        {#if entryInstalled(e)}<span class="dep-inst-tag">on box</span>{/if}
                      </div>
                      <div class="dep-card-model">{e.model}</div>
                      <div class="dep-card-img" title={e.container_image}>
                        <span class="dep-img-chip">{imageChip(e.container_image)}</span>
                      </div>
                      <div class="dep-card-desc">{e.description}</div>
                    </button>
                  {/each}
                </div>

                <!-- selected-entry editor: name + highlighted flags + advanced -->
                {#if deploySel >= 0 && deployEntries[deploySel]}
                  <div class="dep-editor">
                    <div class="dep-ed-head">
                      <span class="dep-ed-title">{deployEntries[deploySel].model}</span>
                      <span class="dep-ed-img">→ {deployEntries[deploySel].container_image}</span>
                    </div>
                    {#if deployEntries[deploySel].description}
                      <p class="dep-ed-desc">{deployEntries[deploySel].description}</p>
                    {/if}

                    <label class="dep-f wide"><span>deploy name</span>
                      <input class="dep-in" bind:value={deployName} placeholder="deploy name" /></label>

                    <!-- headline control: % of total GPU VRAM (the slider) -->
                    <div class="dep-vram">
                      <div class="dep-vram-head">
                        <span class="dep-hl-lbl">GPU VRAM allocation</span>
                        <span class="dep-vram-pct">{dfGpuPct}%</span>
                        <span class="dep-hl-flag">--gpu-memory-utilization {(dfGpuPct / 100).toFixed(2)}</span>
                      </div>
                      <input class="dep-slider" type="range" min="10" max="100" step="1" bind:value={dfGpuPct} />
                      <p class="dep-vram-note">% of each GPU's total VRAM vLLM may use. Default 70% — going to 100% commonly OOMs the box.</p>
                    </div>

                    <!-- the other flags people tune, labeled -->
                    <div class="dep-hl-grid">
                      <label class="dep-hl"><span class="dep-hl-lbl">max model length</span>
                        <span class="dep-hl-flag">--max-model-len</span>
                        <input class="dep-in" type="number" min="0" bind:value={dfModelLen} /></label>
                      <label class="dep-hl"><span class="dep-hl-lbl">GPU devices</span>
                        <span class="dep-hl-flag">count / list (tensor-parallel)</span>
                        <input class="dep-in" bind:value={dfGpu} placeholder="all / 1 / 0,1" /></label>
                      <label class="dep-hl"><span class="dep-hl-lbl">max batch</span>
                        <span class="dep-hl-flag">--max-num-batched-tokens</span>
                        <input class="dep-in" type="number" min="0" bind:value={dfMaxBatchTok} /></label>
                      <label class="dep-hl"><span class="dep-hl-lbl">max concurrent sessions</span>
                        <span class="dep-hl-flag">--max-num-seqs</span>
                        <input class="dep-in" type="number" min="0" bind:value={dfMaxSeqs} /></label>
                    </div>

                    <!-- advanced: raw extra args -->
                    <button class="dep-adv-toggle" on:click={() => (depAdvOpen = !depAdvOpen)}>
                      {depAdvOpen ? '▾' : '▸'} advanced
                    </button>
                    {#if depAdvOpen}
                      <label class="dep-f wide"><span>extra server args</span>
                        <input class="dep-in" bind:value={dfExtra} placeholder="--dtype auto --gpu-memory-utilization 0.9 …" /></label>
                      <p class="dep-adv-note">
                        Appended verbatim to the server command (vLLM kinds) or surfaced as
                        <code>EXTRA_ARGS</code> env (others). Filtered to flag-like tokens.
                      </p>
                    {/if}

                    <div class="dep-go">
                      <button class="btn-primary text-xs" on:click={doDeploy}
                              disabled={deployBusy || depPolling || !deployName.trim()}>
                        {deployBusy ? 'starting…' : '🚀 Deploy'}
                      </button>
                      <span class="dep-go-hint">writes ~/aeon-deploy/{deployName || '<name>'}/ then pulls + starts</span>
                    </div>
                  </div>
                {/if}

                <!-- install progress bar -->
                {#if depStatus}
                  <div class="dep-progress">
                    <div class="dep-prog-top">
                      <span class="dep-prog-phase" style="color:{phaseColor(depStatus.phase)}">
                        {phaseLabel(depStatus.phase)}
                      </span>
                      <span class="dep-prog-pct">{depStatus.percent ?? 0}%</span>
                    </div>
                    <div class="gauge">
                      <div class="gauge-fill"
                           style="width:{depStatus.percent ?? 0}%; background:{phaseColor(depStatus.phase)}; box-shadow:0 0 8px {phaseColor(depStatus.phase)}66"></div>
                    </div>
                    {#if depStatus.log}<pre class="dep-prog-log">{depStatus.log}</pre>{/if}
                    {#if depStatus.phase === 'running'}
                      <div class="text-[11px] font-mono text-live-300">
                        deployed → {deployResult?.path}
                      </div>
                    {/if}
                  </div>
                {/if}

                {#if deployResult?.ok && deployResult.compose}
                  <details class="dep-compose-d">
                    <summary>generated docker-compose.yml</summary>
                    <pre class="cmp-ta-pre">{deployResult.compose}</pre>
                  </details>
                {/if}

                <p class="dep-catnote">{deployCat.live_catalog_todo}</p>
              {/if}
            </section>
          {/if}
        {/if}
      </div>
    {/if}

    <!-- ── TERMINAL (E2): multi-pane web SSH ───────────────────────────── -->
    {#if tab === 'terminal'}
      <div class="term-wrap">
        <!-- toolbar: open a pane for any registered system -->
        <div class="term-toolbar">
          <span class="term-tb-label">Open shell:</span>
          {#if systems.length}
            {#each systems as s (s.id)}
              <button class="term-open-btn" on:click={() => openTerminal(s.id)}
                      title={`Add a terminal — SSH to ${s.ssh_user}@${s.address}:${s.port}`}>
                <span class="term-plus">+</span><span>{roleIcon(s.roles)}</span> {s.label}
              </button>
            {/each}
          {:else}
            <span class="text-zinc-500 text-xs">No systems yet — add them in the
              <button class="underline text-cursed-300" on:click={() => (tab = 'systems')}>Connected Systems</button> tab.</span>
          {/if}
          {#if panes.length}
            <button class="term-closeall" on:click={closeAllPanes} title="Close all terminals">close all ✕</button>
          {/if}
        </div>

        {#if !panes.length}
          <div class="term-empty">
            <div class="term-empty-glyph">&gt;_</div>
            <p>No terminals open.</p>
            <p class="term-empty-sub">Pick a system above (or hit the <code>&gt;_</code> button on a system card) to open an interactive SSH shell. Open as many as you like — they tile into a grid.</p>
          </div>
        {:else}
          <div class="term-grid" style="--cols:{paneGridCols(panes.length)}">
            {#each panes as p (p.key)}
              <div class="term-pane">
                <div class="term-pane-head">
                  <span class="term-dot"
                        class:on={p.status === 'open'}
                        class:connecting={p.status === 'connecting'}
                        class:off={p.status === 'closed'}></span>
                  <span class="term-pane-label" title={p.label}>{p.label}</span>
                  <span class="term-pane-state">{p.status}</span>
                  {#if p.status === 'closed'}
                    <button class="term-reconnect" title="Reconnect" on:click={() => { teardownPane(p); mountPane(p); }}>↻</button>
                  {/if}
                  <button class="term-pane-close" title="Close terminal" on:click={() => closePane(p.key)}>✕</button>
                </div>
                <div class="term-pane-body" bind:this={p.el}></div>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/if}

    {#if tab === 'systems'}
      <div class="p-6 max-w-3xl mx-auto w-full space-y-5">
        {#if err}<p class="text-red-400 text-sm">{err}</p>{/if}
        <section class="space-y-2 p-4 rounded-lg border border-ink-800 bg-ink-950/40">
          <h2 class="font-mono text-xs uppercase tracking-wider text-cursed-300">This device's agent-connect key</h2>
          <p class="text-xs text-zinc-400">The Pi installs <em>this</em> public key on each system so it has SSH key-auth for metrics + provisioning.</p>
          <code class="block text-[10px] font-mono text-zinc-300 bg-ink-900 rounded p-2 break-all">{pubkey || '—'}</code>
          <button class="btn text-xs" on:click={() => copy(pubkey)}>copy public key</button>
        </section>

        <section class="space-y-2 p-4 rounded-lg border border-ink-800 bg-ink-950/40">
          <h2 class="font-mono text-xs uppercase tracking-wider text-cursed-300">Add a connected system</h2>
          <div class="grid grid-cols-2 gap-2">
            <input class="{inputCls} col-span-2" placeholder="Label (e.g. OpenClaw)" bind:value={label} />
            <input class={inputCls} placeholder="Address (e.g. 192.168.1.155)" bind:value={address} />
            <div class="flex gap-2">
              <input class="{inputCls} flex-1" placeholder="ssh user" bind:value={sshUser} />
              <input class="{inputCls} w-20" type="number" placeholder="port" bind:value={port} />
            </div>
            <div class="col-span-2 flex items-center gap-4 text-xs text-zinc-300">
              <label class="flex items-center gap-1"><input type="checkbox" bind:checked={roleOpenclaw} /> OpenClaw</label>
              <label class="flex items-center gap-1"><input type="checkbox" bind:checked={roleHermes} /> Hermes</label>
              <label class="flex items-center gap-1"><input type="checkbox" bind:checked={roleDgx} /> DGX Spark</label>
            </div>
          </div>
          <button class="btn-primary text-xs" on:click={onAdd} disabled={adding || !address.trim()}>add system</button>
        </section>

        <section class="space-y-2">
          <h2 class="font-mono text-xs uppercase tracking-wider text-cursed-300">Connected systems</h2>
          {#if !systems.length}<p class="text-zinc-500 text-xs">No systems yet.</p>{/if}
          {#each systems as s (s.id)}
            <div class="p-3 rounded-lg border border-ink-800 bg-ink-950/40 space-y-2">
              <div class="flex items-center justify-between gap-2">
                <div class="min-w-0">
                  <p class="text-sm text-zinc-200 truncate">{s.label}</p>
                  <p class="text-[10px] font-mono text-zinc-500">{s.ssh_user}@{s.address}:{s.port} · {s.roles.join(', ') || 'no role'}</p>
                </div>
                <span class="text-[10px] font-mono uppercase px-2 py-0.5 rounded-full {badgeCls(s.status)}">{s.status || 'pending'}</span>
              </div>
              {#if s.status === 'connected'}
                <!-- Connected: a green state + test/remove. No auth input. -->
                <div class="flex gap-2 items-center">
                  <span class="flex items-center gap-1.5 text-xs text-live-300">
                    <span class="h-2 w-2 rounded-full bg-live-400 shadow-[0_0_6px_#34d399]"></span>connected
                  </span>
                  <button class="btn text-xs" on:click={() => onTest(s)} disabled={busyId === s.id}>
                    {busyId === s.id ? 'testing…' : 'test connection'}
                  </button>
                  <button class="btn text-xs ml-auto text-red-300" on:click={() => onRemove(s)}>remove</button>
                </div>
              {:else}
                <!-- Not connected: always-visible masked password + single
                     authenticate button (+ test to re-validate, + remove). -->
                <form class="flex flex-wrap gap-2 items-center" on:submit|preventDefault={() => doRegister(s)}>
                  <input class="{inputCls} flex-1 min-w-[200px]" type="password" autocomplete="off"
                         placeholder="SSH password for {s.ssh_user}@{s.address} — used once, never stored"
                         bind:value={authPw[s.id]} disabled={busyId === s.id} />
                  <button class="btn-primary text-xs" type="submit" disabled={busyId === s.id}>
                    {busyId === s.id ? 'authenticating…' : 'authenticate'}
                  </button>
                  <button class="btn text-xs" type="button" on:click={() => onTest(s)} disabled={busyId === s.id}>test</button>
                  <button class="btn text-xs text-red-300" type="button" on:click={() => onRemove(s)}>remove</button>
                </form>
                {#if fallback[s.id]}
                  <div class="text-[10px] font-mono text-amber-300 space-y-1">
                    <p>Password auth unavailable — run this on {s.address} as {s.ssh_user}, then hit <b>test</b>:</p>
                    <code class="block bg-ink-900 rounded p-2 break-all text-zinc-200">{fallback[s.id]}</code>
                    <button class="btn text-xs" type="button" on:click={() => copy(fallback[s.id])}>copy command</button>
                  </div>
                {/if}
              {/if}
            </div>
          {/each}
        </section>
      </div>
    {/if}
  </main>

  {#if detailAgent}
    <div class="agd-overlay" role="button" tabindex="-1"
         on:click={closeDetail} on:keydown={(e) => e.key === 'Escape' && closeDetail()}>
      <div class="agd" role="dialog" tabindex="-1"
           on:click|stopPropagation on:keydown|stopPropagation>
        <header class="agd-head">
          <span class="agd-emoji">{detailAgent.emoji || '🤖'}</span>
          <div class="min-w-0 flex-1">
            <div class="agd-name">{detailAgent.name}{#if detailAgent.is_default}<span class="def-star">★</span>{/if}</div>
            <div class="agd-model">{detail?.model || detailAgent.model || '—'}</div>
          </div>
          <button class="agd-close" on:click={closeDetail} title="close">✕</button>
        </header>

        {#if detailLoading}<p class="agd-dim">loading agent…</p>{/if}
        {#if detail?.err}<p class="text-amber-300 text-xs font-mono">{detail.err}</p>{/if}

        <section class="agd-sec">
          <h3 class="agd-h3">Profile Photo <span class="agd-adminonly">Matrix avatar</span></h3>
          <div class="agd-avatar">
            <div class="agd-avatar-pic">
              {#if avatar?.download_url}
                <img src={avatar.download_url} alt="avatar" />
              {:else}
                <span class="agd-avatar-emoji">{detailAgent.emoji || '🤖'}</span>
              {/if}
            </div>
            <div class="agd-avatar-body">
              {#if avatar && !avatar.ok}
                <p class="agd-warn">{avatar.err}</p>
              {:else if avatar?.user_id}
                <p class="agd-mono agd-dim agd-clip">{avatar.user_id}{#if !avatar.avatar_url} · no avatar set{/if}</p>
              {:else}
                <p class="agd-dim">resolving Matrix account…</p>
              {/if}
              <label class="btn-primary text-xs agd-filebtn" class:agd-disabled={avatarBusy}>
                {avatarBusy ? 'uploading…' : avatar?.avatar_url ? 'Replace photo' : 'Upload photo'}
                <input type="file" accept="image/*" on:change={onAvatarPick} disabled={avatarBusy} hidden />
              </label>
              {#if avatarMsg}<div class="agd-mono agd-dim">{avatarMsg}</div>{/if}
            </div>
          </div>
        </section>

        <section class="agd-sec">
          <h3 class="agd-h3">Aeon Magick Access</h3>
          {#if detail?.provisioned}
            <div class="agd-prov">
              <span class="agd-badge on">provisioned</span>
              <span class="agd-mono agd-dim">token {detail.provisioned.token_id}</span>
              <button class="btn text-xs ml-auto text-red-300" on:click={doRevoke} disabled={provisioning}>revoke API key</button>
            </div>
          {:else}
            <p class="agd-dim">No token yet. Provisioning mints a scoped Aeon Magick API key and drops an access file into this agent's gateway workspace.</p>
            <button class="btn-primary text-xs" on:click={doProvision} disabled={provisioning}>
              {provisioning ? 'provisioning…' : 'Provision API Key'}
            </button>
          {/if}
          {#if newToken}
            <div class="agd-token">
              <div class="agd-token-row"><span class="agd-dim">API token — shown once</span><button class="agd-copy" on:click={() => copy(newToken)}>copy</button></div>
              <code class="agd-code">{newToken}</code>
              {#if dropMsg}<div class="agd-mono agd-dim">{dropMsg}</div>{/if}
              {#if configChange}<div class="agd-note">Enable: {configChange}</div>{/if}
            </div>
          {/if}
        </section>

        <section class="agd-sec">
          <h3 class="agd-h3">SSH Access to the Pi <span class="agd-adminonly">human-admin only</span></h3>
          {#if detail?.ssh}
            <div class="agd-prov">
              <span class="agd-badge on">SSH provisioned</span>
              <span class="agd-mono agd-dim">{detail.ssh.user}@{detail.ssh.pi_address || 'pi'}</span>
              <button class="btn text-xs ml-auto text-red-300" on:click={doRevokeSsh} disabled={sshBusy}>revoke SSH</button>
            </div>
            <label class="agd-sudo">
              <input type="checkbox" checked={detail.ssh.admin} on:change={onSudoToggle} disabled={sshBusy} />
              <span>sudo (admin)</span>
              {#if detail.ssh.admin}<span class="agd-warn">⚠ FULL ADMIN granted</span>{/if}
            </label>
          {:else}
            <p class="agd-dim">Create an <code class="agd-inline">aeon-agent-{detailAgent?.id}</code> login on the Pi so this agent can SSH in for system config. Human-admin action — never exposed to agents via API or MCP.</p>
            <label class="agd-sudo">
              <input type="checkbox" bind:checked={grantSudo} disabled={sshBusy} />
              <span>grant sudo (admin)</span>
            </label>
            {#if grantSudo}<p class="agd-warn">⚠ Enabling sudo grants this agent FULL passwordless root on the Pi. Only do this if absolutely necessary.</p>{/if}
            <button class="btn-primary text-xs" on:click={doGrantSsh} disabled={sshBusy}>{sshBusy ? 'granting…' : 'Grant SSH key'}</button>
          {/if}
          {#if newPrivKey}
            <div class="agd-token">
              <div class="agd-token-row"><span class="agd-dim">Private key — shown once</span><button class="agd-copy" on:click={() => copy(newPrivKey)}>copy</button></div>
              <code class="agd-code agd-key">{newPrivKey}</code>
              {#if sshCmd}<div class="agd-mono agd-dim">{sshCmd}</div>{/if}
              {#if sshDropMsg}<div class="agd-mono agd-dim">{sshDropMsg}</div>{/if}
            </div>
          {/if}
        </section>

        <section class="agd-sec">
          <h3 class="agd-h3">Skills</h3>
          {#if detail?.skills?.length}
            <div class="agd-chips">{#each detail.skills as sk}<span class="agd-chip" class:am={sk === 'aeon-magick'}>{sk}</span>{/each}</div>
          {:else if detail}<p class="agd-dim">none assigned</p>{/if}

          {#if quickAddSkills.length}
            <p class="agd-dim agd-avail">Quick-add a gateway skill (shows the config-change to apply):</p>
            <div class="agd-chips">
              {#each quickAddSkills as sk}
                <button class="agd-chip agd-chip-add" disabled={!!skillBusy} on:click={() => quickAddSkill(sk)}>
                  {skillBusy === sk ? '…' : '+ ' + sk}
                </button>
              {/each}
            </div>
          {/if}

          <div class="agd-skill-upload">
            <p class="agd-dim">Upload a custom skill — a <code class="agd-inline">SKILL.md</code> file or a <code class="agd-inline">.tar</code>/<code class="agd-inline">.tgz</code> of the skill folder. Drops into <code class="agd-inline">~/.openclaw/workspace/skills/&lt;name&gt;/</code> on the gateway.</p>
            <input class={inputCls + ' w-full'} placeholder="skill name (dir under workspace/skills/)" bind:value={customSkillName} />
            <input type="file" accept=".md,.tar,.tgz,.gz,application/x-tar,application/gzip,text/markdown" on:change={onSkillFile} />
            <button
              class="btn-primary text-xs agd-filebtn"
              class:agd-disabled={!customSkillFile || !customSkillName.trim() || !!skillBusy}
              on:click={uploadCustomSkill}
            >
              {skillBusy && customSkillFile ? 'uploading…' : 'Upload skill'}
            </button>
          </div>

          {#if skillResult}
            {#if skillResult.ok}
              <div class="agd-token">
                <div class="agd-mono agd-dim">added “{skillResult.skill}”{#if skillResult.dropped} → {skillResult.dropped}{/if}</div>
                {#if skillResult.config_change}<div class="agd-note">Enable: {skillResult.config_change}</div>{/if}
              </div>
            {:else}
              <p class="agd-warn">{skillResult.err}</p>
            {/if}
          {/if}
          <p class="agd-dim agd-avail">AEON-7 GitHub skill marketplace: TODO (needs <code class="agd-inline">gh</code>, not installed on the Pi).</p>
        </section>

        <!-- F7a: Soul + Identity — the persona's markdown in its workspace. -->
        {#each PERSONA_FILES as pf}
          {@const w = pf.which}
          <section class="agd-sec">
            <h3 class="agd-h3">{pf.label} <span class="agd-adminonly">{pf.file}</span></h3>
            <p class="agd-dim">The persona's {pf.hint}.</p>
            {#if !personaFiles[w] && !personaLoading[w]}
              <button class="btn-primary text-xs agd-filebtn" on:click={() => loadPersonaFile(w)}>View / edit {pf.label}</button>
            {:else if personaLoading[w]}
              <p class="agd-dim">loading {pf.file}…</p>
            {:else if personaFiles[w] && !personaFiles[w].ok}
              <p class="agd-warn">{personaFiles[w].err}</p>
              <button class="btn-primary text-xs agd-filebtn" on:click={() => loadPersonaFile(w)}>Retry</button>
            {:else if personaFiles[w]}
              <p class="agd-mono agd-dim agd-clip">{personaFiles[w].path || pf.file}{#if !personaFiles[w].exists} · (new — not on disk yet){/if}</p>
              <textarea
                class="agd-persona-edit"
                spellcheck="false"
                bind:value={personaDraft[w]}
                placeholder={`Write ${pf.label} in markdown…`}
              ></textarea>
              <div class="agd-persona-actions">
                <button
                  class="btn-primary text-xs agd-filebtn"
                  class:agd-disabled={personaSaving[w]}
                  on:click={() => savePersonaFile(w)}
                >
                  {personaSaving[w] ? 'saving…' : 'Save ' + pf.label}
                </button>
                <button class="agd-chip" on:click={() => loadPersonaFile(w)} disabled={personaSaving[w]}>Reload</button>
                {#if personaMsg[w]}<span class="agd-mono agd-dim">{personaMsg[w]}</span>{/if}
              </div>
            {/if}
          </section>
        {/each}

        <div class="agd-grid2">
          <section class="agd-sec">
            <h3 class="agd-h3">Voice
              {#if voice?.ok && voice.kind && voice.kind !== 'none'}<span class="agd-chip">{voice.kind === 'clone' ? 'named clone' : 'designer'}</span>{/if}
            </h3>
            {#if voice == null}
              <p class="agd-dim">resolving…</p>
            {:else if !voice.ok}
              <p class="agd-warn">{voice.err}</p>
            {:else}
              <p class="agd-mono agd-clip" title={voice.voice ?? ''}>{voice.voice || '—'}</p>
              <p class="agd-dim agd-avail">
                {voice.is_override ? 'per-agent override' : 'inherits gateway default'}
                {#if voice.provider} · {voice.provider}{/if}
              </p>
            {/if}
          </section>
          <section class="agd-sec"><h3 class="agd-h3">Corpus mode</h3><p class="agd-mono">{detail?.corpus || '—'}</p></section>
        </div>
        {#if voice?.ok}
          <p class="agd-dim agd-avail">Voice source: <span class="agd-mono">{voice.source}</span>. Edit designer-description / upload a clone sample: TODO.</p>
        {/if}

        <section class="agd-sec">
          <h3 class="agd-h3">Corpus Files <span class="agd-adminonly">read-only</span></h3>
          {#if !corpus && !corpusLoading}
            <p class="agd-dim">The agent's knowledge vault (markdown notes) on the gateway.</p>
            <button class="btn-primary text-xs agd-filebtn" on:click={loadCorpus}>Browse corpus</button>
          {:else if corpusLoading}
            <p class="agd-dim">loading corpus…</p>
          {:else if corpus && !corpus.ok}
            <p class="agd-warn">{corpus.err}</p>
          {:else if corpus}
            <p class="agd-mono agd-dim agd-clip">{corpus.root} · {corpus.count} files</p>
            {#if !corpus.exists}
              <p class="agd-dim">No corpus vault found for this agent yet.</p>
            {:else if corpus.count}
              <input
                class={inputCls + ' w-full'}
                placeholder="filter files…"
                bind:value={corpusFilter}
              />
              <div class="agd-corpus-list">
                {#each corpusFiles.slice(0, 400) as f}
                  <button
                    class="agd-corpus-row"
                    class:sel={corpusFilePath === f.path}
                    on:click={() => viewCorpusFile(f.path)}
                  >
                    <span class="agd-corpus-path">{f.path}</span>
                    <span class="agd-corpus-size">{fmtBytes(f.size)}</span>
                  </button>
                {/each}
                {#if corpusFiles.length > 400}
                  <p class="agd-dim">…{corpusFiles.length - 400} more (refine the filter)</p>
                {/if}
                {#if corpusFiles.length === 0}
                  <p class="agd-dim">no files match “{corpusFilter}”</p>
                {/if}
              </div>
              <button class="agd-copy" on:click={loadCorpus}>refresh list</button>
            {/if}
            {#if corpusFilePath}
              <div class="agd-corpus-view">
                <div class="agd-token-row">
                  <span class="agd-mono agd-dim agd-clip">{corpusFilePath}{#if corpusFile?.size != null} · {fmtBytes(corpusFile.size)}{/if}</span>
                  <span>
                    {#if corpusFile?.content}<button class="agd-copy" on:click={() => copy(corpusFile?.content ?? '')}>copy</button>{/if}
                    <button class="agd-copy" on:click={closeCorpusFile}>close</button>
                  </span>
                </div>
                {#if corpusFileLoading}
                  <p class="agd-dim">loading file…</p>
                {:else if corpusFile && !corpusFile.ok}
                  <p class="agd-warn">{corpusFile.err}</p>
                {:else if corpusFile}
                  <pre class="agd-corpus-pre">{corpusFile.content}</pre>
                {/if}
              </div>
            {/if}
          {/if}
          <p class="agd-dim agd-avail">Upload / edit: TODO — read-only browse for v1.</p>
        </section>

        <p class="agd-soon">Stretch TODOs: corpus upload/edit · voice designer-edit / clone upload · AEON-7 GitHub skill marketplace.</p>
      </div>
    </div>
  {/if}

  <!-- F7b: deploy a new persona modal. -->
  {#if showNewPersona}
    <div class="agd-overlay" role="button" tabindex="-1"
         on:click={closeNewPersona} on:keydown={(e) => e.key === 'Escape' && closeNewPersona()}>
      <div class="agd np-modal" role="dialog" tabindex="-1"
           on:click|stopPropagation on:keydown|stopPropagation>
        <header class="agd-head">
          <span class="agd-emoji">{np.emoji || '✨'}</span>
          <div class="min-w-0 flex-1">
            <div class="agd-name">New persona</div>
            <div class="agd-model">on {npSysId}</div>
          </div>
          <button class="agd-close" on:click={closeNewPersona} title="close">✕</button>
        </header>
        <div class="agd-body">
          <p class="agd-dim">
            Scaffolds the workspace + <code class="agd-inline">SOUL.md</code>/<code class="agd-inline">IDENTITY.md</code>,
            registers the OpenClaw agent, sets its identity, and reloads the gateway. The Matrix account + voice
            (which need secrets) are returned as the remaining commands.
          </p>

          <section class="agd-sec">
            <div class="np-row2">
              <label class="np-field">
                <span class="np-lbl">Handle / id <span class="agd-dim">(lowercase)</span></span>
                <input class={inputCls + ' w-full'} placeholder="ada" bind:value={np.id} disabled={npBusy} />
              </label>
              <label class="np-field np-emoji-field">
                <span class="np-lbl">Emoji</span>
                <input class={inputCls + ' w-full'} placeholder="✨" bind:value={np.emoji} disabled={npBusy} />
              </label>
            </div>
            <div class="np-row2">
              <label class="np-field">
                <span class="np-lbl">Display name</span>
                <input class={inputCls + ' w-full'} placeholder="Ada Lovelace" bind:value={np.name} disabled={npBusy} />
              </label>
              <label class="np-field">
                <span class="np-lbl">Model</span>
                <input class={inputCls + ' w-full'} placeholder="vllm/qwen36-deep" bind:value={np.model} disabled={npBusy} />
              </label>
            </div>
            <label class="np-field">
              <span class="np-lbl">Voice <span class="agd-dim">— clone name or designer description</span></span>
              <input class={inputCls + ' w-full'} placeholder="warm, measured English mathematician" bind:value={np.voice} disabled={npBusy} />
            </label>
          </section>

          <section class="agd-sec">
            <h3 class="agd-h3">Identity <span class="agd-adminonly">IDENTITY.md</span></h3>
            <p class="agd-dim">Facts: era, domain, one-line self-description. Leave blank to seed a template.</p>
            <textarea class="agd-persona-edit" spellcheck="false" bind:value={np.identity} disabled={npBusy}
              placeholder="The unambiguous facts about the persona…"></textarea>
          </section>

          <section class="agd-sec">
            <h3 class="agd-h3">Soul <span class="agd-adminonly">SOUL.md</span></h3>
            <p class="agd-dim">The essence (2nd person — it becomes the system prompt). Leave blank to seed a template.</p>
            <textarea class="agd-persona-edit" spellcheck="false" bind:value={np.soul} disabled={npBusy}
              placeholder="You are…  voice, cadence, values, manner."></textarea>
          </section>

          <section class="agd-sec">
            <h3 class="agd-h3">Corpus seed <span class="agd-adminonly">optional</span></h3>
            <p class="agd-dim">A single markdown note to ground retrieval from day one (dropped into the corpus vault).</p>
            <textarea class="agd-persona-edit agd-persona-short" spellcheck="false" bind:value={np.corpus_seed} disabled={npBusy}
              placeholder="Optional starter knowledge…"></textarea>
          </section>

          <div class="np-submit">
            <button class="btn-primary text-xs agd-filebtn" class:agd-disabled={npBusy || !np.id?.trim()} on:click={submitNewPersona}>
              {npBusy ? 'provisioning…' : 'Deploy persona'}
            </button>
            <span class="agd-dim agd-mono">creates workspace + registers agent + reloads gateway</span>
          </div>

          {#if npResult}
            {#if npResult.ok}
              <section class="agd-sec np-result-ok">
                <h3 class="agd-h3">Provisioned “{npResult.display}” {npResult.emoji || ''} <span class="agd-chip" class:am={npResult.registered}>{npResult.registered ? 'registered' : 'files only'}</span></h3>
                {#if npResult.report?.steps?.length}
                  <ul class="np-steps">
                    {#each npResult.report.steps as st}
                      <li class:np-ok={st.ok} class:np-bad={!st.ok}>
                        <span class="np-step-mark">{st.ok ? '✓' : '✕'}</span>
                        <span class="np-step-name">{st.step}</span>
                        {#if st.detail}<span class="agd-mono agd-dim np-step-detail">{st.detail}</span>{/if}
                      </li>
                    {/each}
                  </ul>
                {/if}
                {#if npResult.manual_steps?.length}
                  <p class="agd-dim agd-avail">Remaining manual steps (need secrets / homeserver admin):</p>
                  {#each npResult.manual_steps as ms}
                    <div class="agd-token np-manual">
                      <div class="agd-mono">{ms.title}</div>
                      <div class="agd-dim np-why">{ms.why}</div>
                      <pre class="agd-corpus-pre np-cmd">{ms.cmd}</pre>
                    </div>
                  {/each}
                {/if}
                {#if npResult.todo}<p class="agd-soon">{npResult.todo}</p>{/if}
              </section>
            {:else}
              <p class="agd-warn">{npResult.err}</p>
              {#if npResult.report?.steps?.length}
                <ul class="np-steps">
                  {#each npResult.report.steps as st}
                    <li class:np-ok={st.ok} class:np-bad={!st.ok}><span class="np-step-mark">{st.ok ? '✓' : '✕'}</span> <span class="np-step-name">{st.step}</span></li>
                  {/each}
                </ul>
              {/if}
            {/if}
          {/if}
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .section-title {
    font-family: ui-monospace, monospace;
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.12em;
    color: #c4b5fd;
  }
  .refresh-btn {
    font-family: ui-monospace, monospace;
    font-size: 0.65rem;
    color: #a1a1aa;
    padding: 0.2rem 0.6rem;
    border: 1px solid #2a2a38;
    border-radius: 0.35rem;
    background: #12121a;
  }
  .refresh-btn:hover { color: #e4e4e7; border-color: #3f3f5a; }
  .spin { display: inline-block; animation: spin 0.9s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }

  /* ── system cards ── */
  .sys-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 0.85rem;
  }
  .sys-card {
    border: 1px solid #23232f;
    border-radius: 0.75rem;
    padding: 0.9rem 1rem;
    background: linear-gradient(160deg, rgba(30, 27, 50, 0.5), rgba(12, 12, 20, 0.6));
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
  }
  .sys-head { display: flex; align-items: center; gap: 0.6rem; }
  .sys-icon { font-size: 1.35rem; line-height: 1; filter: drop-shadow(0 0 6px rgba(167, 139, 250, 0.4)); }
  .sys-id { min-width: 0; flex: 1; }
  .sys-label { font-size: 0.9rem; color: #f4f4f5; font-weight: 600; line-height: 1.1; }
  .sys-host {
    font-family: ui-monospace, monospace;
    font-size: 0.62rem;
    color: #71717a;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pill {
    font-family: ui-monospace, monospace;
    font-size: 0.6rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    padding: 0.12rem 0.5rem;
    border-radius: 999px;
    background: #1c1c26;
    color: #71717a;
    border: 1px solid #2a2a38;
  }
  .pill.on { background: rgba(52, 211, 153, 0.12); color: #6ee7b7; border-color: rgba(52, 211, 153, 0.35); }
  .pill.off { background: rgba(248, 113, 113, 0.1); color: #fca5a5; border-color: rgba(248, 113, 113, 0.3); }

  .gpu { display: flex; flex-direction: column; gap: 0.3rem; }
  .gpu-top { display: flex; justify-content: space-between; align-items: baseline; gap: 0.5rem; }
  .gpu-name {
    font-family: ui-monospace, monospace;
    font-size: 0.66rem;
    color: #d4d4d8;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gpu-meta { font-family: ui-monospace, monospace; font-size: 0.66rem; font-weight: 600; white-space: nowrap; }
  .gauge { height: 0.5rem; border-radius: 999px; background: #18181f; overflow: hidden; }
  .gauge.sm { height: 0.3rem; }
  .gauge-fill { height: 100%; border-radius: 999px; transition: width 0.6s cubic-bezier(0.2, 0.8, 0.2, 1); }
  .vram-row {
    display: flex;
    justify-content: space-between;
    font-family: ui-monospace, monospace;
    font-size: 0.6rem;
    color: #8b8b96;
  }
  .unified {
    font-size: 0.52rem;
    color: #a78bfa;
    border: 1px solid rgba(167, 139, 250, 0.4);
    border-radius: 3px;
    padding: 0 0.2rem;
    margin-left: 0.15rem;
  }

  .sys-bars { display: flex; flex-direction: column; gap: 0.4rem; }
  .sbar { display: flex; flex-direction: column; gap: 0.25rem; }
  .sbar-top {
    display: flex; justify-content: space-between; align-items: baseline;
    font-family: ui-monospace, monospace; font-size: 0.62rem; color: #8b8b96;
  }
  .sbar-top span:last-child { font-weight: 600; white-space: nowrap; }

  .tiles { display: grid; grid-template-columns: repeat(3, 1fr); gap: 0.4rem; }
  .tile { background: rgba(12, 12, 20, 0.5); border: 1px solid #20202b; border-radius: 0.5rem; padding: 0.4rem; text-align: center; }
  .tile-num { font-family: ui-monospace, monospace; font-size: 0.82rem; color: #e4e4e7; font-weight: 600; }
  .tile-lbl { font-family: ui-monospace, monospace; font-size: 0.55rem; color: #71717a; text-transform: uppercase; letter-spacing: 0.05em; }
  .chips { display: flex; flex-wrap: wrap; gap: 0.25rem; }
  .chip {
    font-family: ui-monospace, monospace;
    font-size: 0.58rem;
    color: #a5b4fc;
    background: rgba(99, 102, 241, 0.1);
    border: 1px solid rgba(99, 102, 241, 0.25);
    border-radius: 4px;
    padding: 0.05rem 0.35rem;
  }
  .errline { font-family: ui-monospace, monospace; font-size: 0.62rem; color: #fca5a5; }
  .errline.dim { color: #52525b; }
  .sys-power { display: flex; gap: 0.3rem; margin-top: 0.1rem; padding-top: 0.55rem; border-top: 1px solid #1c1c26; }
  .pw-btn { font-family: ui-monospace, monospace; font-size: 0.58rem; color: #a1a1aa; background: #14141d; border: 1px solid #2a2a38; border-radius: 0.3rem; padding: 0.22rem 0.45rem; cursor: pointer; flex: 1; }
  .pw-btn:hover:not(:disabled) { border-color: rgba(248, 113, 113, 0.5); color: #fca5a5; }
  .pw-btn.wake:hover:not(:disabled) { border-color: rgba(52, 211, 153, 0.5); color: #6ee7b7; }
  .pw-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .docker-btn {
    font-size: 0.9rem; line-height: 1; padding: 0.15rem 0.3rem; border-radius: 0.35rem;
    background: rgba(96, 165, 250, 0.1); border: 1px solid rgba(96, 165, 250, 0.3); cursor: pointer;
  }
  .docker-btn:hover { background: rgba(96, 165, 250, 0.2); border-color: rgba(96, 165, 250, 0.55); }
  .term-btn {
    font-size: 0.72rem; font-weight: 700; line-height: 1; padding: 0.22rem 0.34rem;
    border-radius: 0.35rem; font-family: ui-monospace, monospace; letter-spacing: -0.02em;
    color: #c4b5fd; background: rgba(167, 139, 250, 0.1);
    border: 1px solid rgba(167, 139, 250, 0.3); cursor: pointer;
  }
  .term-btn:hover { background: rgba(167, 139, 250, 0.22); border-color: rgba(167, 139, 250, 0.6); color: #ddd6fe; }
  .tab-count {
    display: inline-flex; align-items: center; justify-content: center;
    min-width: 1.1rem; height: 1.1rem; margin-left: 0.35rem; padding: 0 0.25rem;
    font-size: 0.62rem; font-weight: 700; border-radius: 999px;
    background: rgba(167, 139, 250, 0.2); color: #c4b5fd; border: 1px solid rgba(167, 139, 250, 0.4);
  }

  /* ── E4: Containers ── */
  .c-sel { display: flex; flex-wrap: wrap; gap: 0.35rem; }
  .c-sel-btn {
    font-family: ui-monospace, monospace; font-size: 0.66rem; color: #a1a1aa;
    background: #12121a; border: 1px solid #2a2a38; border-radius: 0.4rem; padding: 0.3rem 0.6rem; cursor: pointer;
    display: inline-flex; align-items: center; gap: 0.3rem;
  }
  .c-sel-btn:hover { color: #e4e4e7; border-color: #3f3f5a; }
  .c-sel-btn.active { color: #c4b5fd; border-color: rgba(167, 139, 250, 0.5); background: rgba(167, 139, 250, 0.1); }

  .ctr-list { display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 0.6rem; }
  .ctr {
    border: 1px solid #23232f; border-radius: 0.6rem; padding: 0.6rem 0.7rem;
    background: linear-gradient(160deg, rgba(24, 24, 34, 0.6), rgba(12, 12, 20, 0.6));
    display: flex; flex-direction: column; gap: 0.4rem;
  }
  .ctr-main { display: flex; align-items: center; gap: 0.5rem; }
  .ctr-state {
    font-family: ui-monospace, monospace; font-size: 0.55rem; text-transform: uppercase; letter-spacing: 0.05em;
    padding: 0.1rem 0.4rem; border-radius: 999px; flex: none; white-space: nowrap;
  }
  .cst-run { background: rgba(52, 211, 153, 0.14); color: #6ee7b7; border: 1px solid rgba(52, 211, 153, 0.4); }
  .cst-exit { background: rgba(248, 113, 113, 0.12); color: #fca5a5; border: 1px solid rgba(248, 113, 113, 0.35); }
  .cst-other { background: #1c1c26; color: #a1a1aa; border: 1px solid #2a2a38; }
  .ctr-id { min-width: 0; flex: 1; }
  .ctr-name { font-size: 0.8rem; color: #f4f4f5; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ctr-img { font-family: ui-monospace, monospace; font-size: 0.58rem; color: #71717a; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ctr-actions { display: flex; gap: 0.25rem; flex: none; }
  .ctr-btn {
    font-family: ui-monospace, monospace; font-size: 0.62rem; color: #a1a1aa; background: #14141d;
    border: 1px solid #2a2a38; border-radius: 0.3rem; padding: 0.18rem 0.4rem; cursor: pointer; min-width: 1.6rem;
  }
  .ctr-btn:hover:not(:disabled) { color: #e4e4e7; border-color: #3f3f5a; }
  .ctr-btn.start:hover:not(:disabled) { border-color: rgba(52, 211, 153, 0.5); color: #6ee7b7; }
  .ctr-btn.stop:hover:not(:disabled) { border-color: rgba(248, 113, 113, 0.5); color: #fca5a5; }
  .ctr-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .ctr-status { font-family: ui-monospace, monospace; font-size: 0.6rem; color: #8b8b96; }
  .ctr-proj { color: #a5b4fc; }
  .ctr-ports { font-family: ui-monospace, monospace; font-size: 0.56rem; color: #5b6478; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ctr-meters { display: grid; grid-template-columns: 1fr 1fr; gap: 0.5rem; margin-top: 0.1rem; }
  .meter { display: flex; flex-direction: column; gap: 0.2rem; }
  .meter-top { display: flex; justify-content: space-between; font-family: ui-monospace, monospace; font-size: 0.56rem; color: #8b8b96; }

  /* compose */
  .cmp-list { display: flex; flex-direction: column; gap: 0.35rem; }
  .cmp {
    display: flex; align-items: center; gap: 0.5rem; padding: 0.4rem 0.55rem;
    border: 1px solid #20202b; border-radius: 0.5rem; background: rgba(12, 12, 20, 0.5);
  }
  .cmp-dot { width: 0.5rem; height: 0.5rem; border-radius: 999px; flex: none; background: #52525b; }
  .cmp-dot.up { background: #34d399; box-shadow: 0 0 7px #34d399; }
  .cmp-path { font-family: ui-monospace, monospace; font-size: 0.6rem; color: #c4c4cc; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cmp-state { font-family: ui-monospace, monospace; font-size: 0.55rem; color: #71717a; text-transform: uppercase; flex: none; }
  .cmp-actions { display: flex; gap: 0.25rem; flex: none; }
  .cmp-out {
    font-family: ui-monospace, monospace; font-size: 0.6rem; color: #c4c4cc; background: #0a0a10;
    border-radius: 0.35rem; padding: 0.5rem; max-height: 12rem; overflow: auto; white-space: pre-wrap; word-break: break-word;
  }
  .cmp-editor { border: 1px dashed #3f3f5a; border-radius: 0.6rem; padding: 0.6rem; display: flex; flex-direction: column; gap: 0.5rem; }
  .cmp-ta {
    width: 100%; min-height: 18rem; font-family: ui-monospace, monospace; font-size: 0.66rem; color: #d4d4dc;
    background: #0a0a10; border: 1px solid #20202b; border-radius: 0.4rem; padding: 0.6rem; resize: vertical; line-height: 1.5;
  }
  .cmp-ta-pre {
    font-family: ui-monospace, monospace; font-size: 0.62rem; color: #c4c4cc; background: #0a0a10;
    border: 1px solid #20202b; border-radius: 0.4rem; padding: 0.6rem; max-height: 18rem; overflow: auto;
    white-space: pre-wrap; word-break: break-word; line-height: 1.5;
  }

  /* ── E5: Easy Deploy — model + container picker ── */
  .deploy {
    border: 1px solid rgba(167, 139, 250, 0.25); border-radius: 0.75rem; padding: 0.9rem 1rem;
    background: linear-gradient(160deg, rgba(40, 30, 60, 0.35), rgba(12, 12, 20, 0.6)); display: flex; flex-direction: column; gap: 0.7rem;
  }
  .dep-via {
    font-family: ui-monospace, monospace; font-size: 0.55rem; text-transform: uppercase; letter-spacing: 0.06em;
    color: #6ee7b7; background: rgba(52, 211, 153, 0.12); border: 1px solid rgba(52, 211, 153, 0.35); border-radius: 999px; padding: 0.08rem 0.5rem;
  }
  .dep-noteligible {
    font-family: ui-monospace, monospace; font-size: 0.62rem; color: #d4b483;
    background: rgba(245, 158, 11, 0.08); border: 1px dashed rgba(245, 158, 11, 0.3); border-radius: 0.4rem; padding: 0.5rem 0.6rem; line-height: 1.45;
  }
  .dep-sub { font-family: ui-monospace, monospace; font-size: 0.58rem; text-transform: uppercase; letter-spacing: 0.08em; color: #8b8b96; margin-top: 0.1rem; }

  /* already-on-box chips */
  .dep-installed { display: flex; flex-direction: column; gap: 0.35rem; }
  .dep-inst-chips { display: flex; flex-wrap: wrap; gap: 0.3rem; }
  .dep-chip {
    font-family: ui-monospace, monospace; font-size: 0.58rem; border-radius: 999px; padding: 0.1rem 0.5rem;
    display: inline-flex; align-items: center; gap: 0.3rem; max-width: 22rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .dep-chip.model { color: #a5b4fc; background: rgba(99, 102, 241, 0.12); border: 1px solid rgba(99, 102, 241, 0.3); }
  .dep-chip.ctr { color: #9ca3af; background: rgba(63, 63, 70, 0.4); border: 1px solid #3f3f5a; }
  .dep-cdot { width: 0.4rem; height: 0.4rem; border-radius: 999px; background: #52525b; flex: none; }
  .dep-cdot.run { background: #34d399; box-shadow: 0 0 6px #34d399; }

  /* catalog cards */
  .dep-cards { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 0.55rem; }
  .dep-card {
    text-align: left; display: flex; flex-direction: column; gap: 0.3rem; padding: 0.6rem 0.7rem;
    border: 1px solid #23232f; border-radius: 0.6rem; background: rgba(12, 12, 20, 0.5); cursor: pointer; transition: border-color 0.15s, background 0.15s;
  }
  .dep-card:hover { border-color: #3f3f5a; }
  .dep-card.active { border-color: rgba(167, 139, 250, 0.6); background: rgba(167, 139, 250, 0.1); box-shadow: 0 0 0 1px rgba(167, 139, 250, 0.3) inset; }
  .dep-card-top { display: flex; align-items: center; justify-content: space-between; gap: 0.4rem; }
  .dep-kind-badge {
    font-family: ui-monospace, monospace; font-size: 0.52rem; text-transform: uppercase; letter-spacing: 0.05em;
    padding: 0.08rem 0.4rem; border-radius: 4px; color: #c4b5fd; background: rgba(167, 139, 250, 0.12); border: 1px solid rgba(167, 139, 250, 0.3);
  }
  .dep-kind-badge.imagegen { color: #f0abfc; background: rgba(217, 70, 239, 0.12); border-color: rgba(217, 70, 239, 0.3); }
  .dep-kind-badge.embedding { color: #67e8f9; background: rgba(34, 211, 238, 0.1); border-color: rgba(34, 211, 238, 0.3); }
  .dep-kind-badge.tts { color: #fcd34d; background: rgba(251, 191, 36, 0.1); border-color: rgba(251, 191, 36, 0.3); }
  .dep-inst-tag {
    font-family: ui-monospace, monospace; font-size: 0.5rem; text-transform: uppercase; letter-spacing: 0.05em;
    color: #6ee7b7; background: rgba(52, 211, 153, 0.14); border: 1px solid rgba(52, 211, 153, 0.4); border-radius: 4px; padding: 0.04rem 0.3rem;
  }
  .dep-card-model { font-size: 0.78rem; color: #f4f4f5; font-weight: 600; line-height: 1.2; word-break: break-word; }
  .dep-card-img { display: flex; }
  .dep-img-chip {
    font-family: ui-monospace, monospace; font-size: 0.56rem; color: #93c5fd; background: rgba(59, 130, 246, 0.1);
    border: 1px solid rgba(59, 130, 246, 0.25); border-radius: 4px; padding: 0.05rem 0.4rem; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .dep-card-desc { font-size: 0.6rem; color: #71717a; line-height: 1.4; }

  /* selected-entry editor */
  .dep-editor {
    border: 1px solid rgba(167, 139, 250, 0.3); border-radius: 0.6rem; padding: 0.75rem; display: flex; flex-direction: column; gap: 0.6rem;
    background: rgba(167, 139, 250, 0.05);
  }
  .dep-ed-head { display: flex; align-items: baseline; gap: 0.5rem; flex-wrap: wrap; }
  .dep-ed-title { font-size: 0.82rem; color: #f4f4f5; font-weight: 700; }
  .dep-ed-img { font-family: ui-monospace, monospace; font-size: 0.6rem; color: #93c5fd; }
  .dep-ed-desc { font-size: 0.64rem; color: #a1a1aa; line-height: 1.45; }

  .dep-in {
    background: #0a0a10; border: 1px solid #2a2a38; border-radius: 0.35rem; padding: 0.3rem 0.5rem;
    font-family: ui-monospace, monospace; font-size: 0.7rem; color: #d4d4dc;
  }
  .dep-in:focus { outline: none; border-color: #6d5fd0; }
  .dep-f { display: flex; flex-direction: column; gap: 0.25rem; }
  .dep-f > span { font-family: ui-monospace, monospace; font-size: 0.56rem; color: #8b8b96; text-transform: uppercase; letter-spacing: 0.04em; }
  .dep-f .dep-in { width: 100%; }
  .dep-f.wide { width: 100%; }

  /* the four highlighted (tunable) flags */
  .dep-hl-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 0.6rem; }
  .dep-hl {
    display: flex; flex-direction: column; gap: 0.2rem; padding: 0.55rem 0.6rem;
    border: 1px solid rgba(167, 139, 250, 0.35); border-radius: 0.5rem; background: rgba(167, 139, 250, 0.07);
  }
  .dep-hl-lbl { font-size: 0.68rem; color: #ede9fe; font-weight: 600; }
  .dep-hl-flag { font-family: ui-monospace, monospace; font-size: 0.54rem; color: #a78bfa; }
  .dep-hl .dep-in { width: 100%; margin-top: 0.15rem; font-size: 0.78rem; }

  /* headline GPU VRAM % slider */
  .dep-vram {
    display: flex; flex-direction: column; gap: 0.35rem; padding: 0.6rem 0.7rem;
    border: 1px solid rgba(167, 139, 250, 0.45); border-radius: 0.55rem;
    background: rgba(167, 139, 250, 0.1);
  }
  .dep-vram-head { display: flex; align-items: baseline; gap: 0.6rem; flex-wrap: wrap; }
  .dep-vram-pct {
    font-family: ui-monospace, monospace; font-size: 1rem; font-weight: 700; color: #c4b5fd;
    margin-left: auto;
  }
  .dep-slider { width: 100%; accent-color: #a78bfa; cursor: pointer; }
  .dep-vram-note { font-size: 0.58rem; color: #71717a; line-height: 1.4; }

  .dep-adv-toggle {
    align-self: flex-start; font-family: ui-monospace, monospace; font-size: 0.62rem; color: #a1a1aa;
    background: none; border: none; cursor: pointer; padding: 0.1rem 0;
  }
  .dep-adv-toggle:hover { color: #d4d4dc; }
  .dep-adv-note { font-size: 0.58rem; color: #71717a; line-height: 1.4; }

  .dep-go { display: flex; align-items: center; gap: 0.8rem; flex-wrap: wrap; }
  .dep-go-hint { font-family: ui-monospace, monospace; font-size: 0.58rem; color: #71717a; }

  /* install progress bar */
  .dep-progress {
    display: flex; flex-direction: column; gap: 0.4rem; padding: 0.6rem 0.7rem;
    border: 1px solid #23232f; border-radius: 0.6rem; background: rgba(12, 12, 20, 0.6);
  }
  .dep-prog-top { display: flex; align-items: baseline; justify-content: space-between; }
  .dep-prog-phase { font-family: ui-monospace, monospace; font-size: 0.66rem; font-weight: 600; }
  .dep-prog-pct { font-family: ui-monospace, monospace; font-size: 0.66rem; color: #d4d4dc; }
  .dep-prog-log {
    font-family: ui-monospace, monospace; font-size: 0.56rem; color: #8b8b96; background: #0a0a10;
    border-radius: 0.35rem; padding: 0.45rem; max-height: 8rem; overflow: auto; white-space: pre-wrap; word-break: break-word; line-height: 1.4;
  }
  .dep-compose-d { font-family: ui-monospace, monospace; font-size: 0.6rem; color: #a1a1aa; }
  .dep-compose-d summary { cursor: pointer; color: #c4b5fd; }
  .dep-catnote { font-size: 0.56rem; color: #52525b; line-height: 1.4; font-style: italic; }

  /* ── agent roster ── */
  .roster-stats { font-family: ui-monospace, monospace; font-size: 0.66rem; color: #a1a1aa; display: flex; gap: 0.4rem; align-items: center; }
  .roster-stats b { color: #e4e4e7; }
  .dot-sep { color: #3f3f46; }
  .agent-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
    gap: 0.6rem;
  }
  .agent {
    border: 1px solid #23232f;
    border-radius: 0.6rem;
    padding: 0.6rem 0.65rem;
    background: linear-gradient(160deg, rgba(24, 24, 34, 0.6), rgba(12, 12, 20, 0.6));
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    transition: border-color 0.2s, transform 0.15s;
  }
  .agent:hover { border-color: #3f3f5a; transform: translateY(-1px); }
  .agent.is-working { border-color: rgba(52, 211, 153, 0.4); background: linear-gradient(160deg, rgba(16, 40, 32, 0.5), rgba(12, 16, 20, 0.6)); }
  .agent.is-default { box-shadow: inset 0 0 0 1px rgba(251, 191, 36, 0.35); }
  .agent-top { display: flex; align-items: center; gap: 0.45rem; }
  .agent-emoji { font-size: 1.15rem; line-height: 1; }
  .agent-id { min-width: 0; flex: 1; }
  .agent-name {
    font-size: 0.78rem;
    color: #f4f4f5;
    font-weight: 600;
    line-height: 1.15;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .def-star { color: #fbbf24; margin-left: 0.2rem; font-size: 0.7rem; }
  .agent-model {
    font-family: ui-monospace, monospace;
    font-size: 0.55rem;
    color: #71717a;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot { width: 0.5rem; height: 0.5rem; border-radius: 999px; flex-shrink: 0; }
  .dot.pulse { animation: pulse 1.4s ease-in-out infinite; }
  @keyframes pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.5; transform: scale(1.35); }
  }
  .agent-status {
    font-family: ui-monospace, monospace;
    font-size: 0.55rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .agent-stats { display: grid; grid-template-columns: repeat(3, 1fr); gap: 0.2rem; }
  .agent-stats > div { display: flex; flex-direction: column; align-items: center; }
  .as-num { font-family: ui-monospace, monospace; font-size: 0.72rem; color: #e4e4e7; font-weight: 600; }
  .as-lbl { font-family: ui-monospace, monospace; font-size: 0.5rem; color: #71717a; text-transform: uppercase; }
  .tok-bar { height: 0.2rem; border-radius: 999px; background: #18181f; overflow: hidden; }
  .tok-bar > div { height: 100%; border-radius: 999px; opacity: 0.8; transition: width 0.6s ease; }

  /* ── usage banner ── */
  .usage-banner {
    border: 1px solid #2a2740;
    border-radius: 0.75rem;
    padding: 0.9rem 1rem;
    background: linear-gradient(160deg, rgba(40, 32, 66, 0.45), rgba(12, 12, 20, 0.55));
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .ub-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; }
  .ub-title { font-family: ui-monospace, monospace; font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.12em; color: #c4b5fd; }
  .ub-sub { color: #52525b; }
  .ub-meta { font-family: ui-monospace, monospace; font-size: 0.6rem; color: #71717a; margin-top: 0.25rem; }
  .ub-meta b { color: #a1a1aa; }
  .ub-total { text-align: right; flex-shrink: 0; }
  .ub-total-num { font-family: ui-monospace, monospace; font-size: 1.6rem; font-weight: 700; color: #f4f4f5; line-height: 1; }
  .ub-total-lbl { font-family: ui-monospace, monospace; font-size: 0.55rem; color: #a78bfa; text-transform: uppercase; letter-spacing: 0.08em; margin-top: 0.2rem; }
  .ub-controls { display: flex; flex-wrap: wrap; gap: 0.35rem; }
  .ub-btn,
  .ub-sel {
    font-family: ui-monospace, monospace;
    font-size: 0.62rem;
    color: #a1a1aa;
    background: #14141d;
    border: 1px solid #2a2a38;
    border-radius: 0.35rem;
    padding: 0.24rem 0.6rem;
    cursor: pointer;
  }
  .ub-btn:hover,
  .ub-sel:hover { border-color: #3f3f5a; color: #e4e4e7; }
  .ub-btn.active,
  .ub-sel.active { background: rgba(167, 139, 250, 0.18); border-color: #a78bfa; color: #ddd6fe; }
  .ub-sel { appearance: none; -webkit-appearance: none; }
  .ub-chart {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    height: 100px;
    background: rgba(10, 10, 16, 0.5);
    border-radius: 0.5rem;
    padding: 0.5rem 0.45rem 0.2rem;
  }
  .ub-bar-wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: flex-end;
    height: 100%;
    min-width: 0;
  }
  .ub-bar {
    width: 100%;
    max-width: 24px;
    border-radius: 3px 3px 0 0;
    background: linear-gradient(to top, #7c3aed, #a78bfa);
    transition: height 0.5s ease;
    min-height: 2px;
  }
  .ub-bar-wrap:hover .ub-bar { background: linear-gradient(to top, #8b5cf6, #c4b5fd); box-shadow: 0 0 10px rgba(167, 139, 250, 0.5); }
  .ub-bar-lbl {
    font-family: ui-monospace, monospace;
    font-size: 0.48rem;
    color: #52525b;
    margin-top: 0.25rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
  }
  .ub-link {
    color: #a78bfa;
    text-decoration: underline;
    cursor: pointer;
    background: none;
    border: none;
    font: inherit;
    padding: 0;
  }
  .ub-empty {
    font-family: ui-monospace, monospace;
    font-size: 0.62rem;
    color: #52525b;
    display: flex;
    align-items: center;
    justify-content: center;
    text-align: center;
    width: 100%;
    padding: 0 1.5rem;
    line-height: 1.5;
  }

  /* ── agent detail modal ── */
  .agent-click { cursor: pointer; }
  .agent-click:focus-visible { outline: 1px solid #a78bfa; outline-offset: 1px; }
  .agd-overlay {
    position: fixed;
    inset: 0;
    z-index: 50;
    background: rgba(5, 5, 10, 0.72);
    backdrop-filter: blur(3px);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1rem;
  }
  .agd {
    width: 100%;
    max-width: 30rem;
    max-height: 88vh;
    overflow-y: auto;
    background: linear-gradient(160deg, #16131f, #0c0c14);
    border: 1px solid #2e2a44;
    border-radius: 0.9rem;
    padding: 1.1rem 1.2rem;
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.6);
  }
  .agd-head { display: flex; align-items: center; gap: 0.6rem; }
  .agd-emoji { font-size: 1.7rem; line-height: 1; }
  .agd-name { font-size: 1.05rem; color: #f4f4f5; font-weight: 700; }
  .agd-model { font-family: ui-monospace, monospace; font-size: 0.62rem; color: #71717a; }
  .agd-close { color: #71717a; font-size: 0.9rem; padding: 0.2rem 0.5rem; border-radius: 0.35rem; }
  .agd-close:hover { color: #e4e4e7; background: #20202b; }
  .agd-sec {
    border: 1px solid #23232f;
    border-radius: 0.6rem;
    padding: 0.7rem 0.8rem;
    background: rgba(10, 10, 16, 0.4);
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .agd-h3 { font-family: ui-monospace, monospace; font-size: 0.62rem; text-transform: uppercase; letter-spacing: 0.1em; color: #c4b5fd; }
  .agd-dim { font-size: 0.72rem; color: #71717a; line-height: 1.45; }
  .agd-mono { font-family: ui-monospace, monospace; font-size: 0.66rem; color: #a1a1aa; }
  .agd-clip { overflow: hidden; display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical; }
  .agd-prov { display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap; }
  .agd-badge { font-family: ui-monospace, monospace; font-size: 0.58rem; text-transform: uppercase; padding: 0.1rem 0.45rem; border-radius: 999px; }
  .agd-badge.on { background: rgba(52, 211, 153, 0.14); color: #6ee7b7; border: 1px solid rgba(52, 211, 153, 0.4); }
  .agd-token { border: 1px dashed #3f3f5a; border-radius: 0.5rem; padding: 0.6rem; display: flex; flex-direction: column; gap: 0.4rem; }
  .agd-token-row { display: flex; justify-content: space-between; align-items: center; }
  .agd-copy { font-family: ui-monospace, monospace; font-size: 0.58rem; color: #a78bfa; }
  .agd-copy:hover { text-decoration: underline; }
  .agd-code { font-family: ui-monospace, monospace; font-size: 0.66rem; color: #6ee7b7; background: #0a0a10; border-radius: 0.35rem; padding: 0.45rem 0.55rem; word-break: break-all; }
  .agd-note { font-family: ui-monospace, monospace; font-size: 0.58rem; color: #fbbf24; line-height: 1.45; }
  .agd-chips { display: flex; flex-wrap: wrap; gap: 0.3rem; }
  .agd-chip { font-family: ui-monospace, monospace; font-size: 0.6rem; color: #a5b4fc; background: rgba(99, 102, 241, 0.1); border: 1px solid rgba(99, 102, 241, 0.25); border-radius: 4px; padding: 0.08rem 0.4rem; }
  .agd-chip.am { color: #6ee7b7; background: rgba(52, 211, 153, 0.12); border-color: rgba(52, 211, 153, 0.4); }
  .agd-avail { margin-top: 0.1rem; }
  .agd-grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 0.6rem; }
  .agd-soon { font-family: ui-monospace, monospace; font-size: 0.56rem; color: #52525b; text-align: center; line-height: 1.5; }
  .agd-adminonly { font-size: 0.5rem; color: #fbbf24; border: 1px solid rgba(251, 191, 36, 0.4); border-radius: 3px; padding: 0 0.25rem; margin-left: 0.3rem; vertical-align: middle; text-transform: uppercase; letter-spacing: 0.04em; }
  .agd-sudo { display: flex; align-items: center; gap: 0.4rem; font-size: 0.72rem; color: #d4d4d8; cursor: pointer; flex-wrap: wrap; }
  .agd-warn { font-size: 0.62rem; color: #fbbf24; line-height: 1.45; }
  .agd-key { white-space: pre-wrap; word-break: break-all; max-height: 7rem; overflow-y: auto; color: #fca5a5; }
  .agd-inline { font-family: ui-monospace, monospace; font-size: 0.62rem; color: #a5b4fc; background: #0a0a10; border-radius: 3px; padding: 0 0.25rem; }
  /* ── E1: avatar ── */
  .agd-avatar { display: flex; gap: 0.7rem; align-items: center; }
  .agd-avatar-pic {
    width: 3.4rem; height: 3.4rem; flex: none; border-radius: 0.6rem; overflow: hidden;
    background: #0a0a10; border: 1px solid #2a2a38; display: flex; align-items: center; justify-content: center;
  }
  .agd-avatar-pic img { width: 100%; height: 100%; object-fit: cover; }
  .agd-avatar-emoji { font-size: 1.8rem; line-height: 1; }
  .agd-avatar-body { display: flex; flex-direction: column; gap: 0.35rem; min-width: 0; flex: 1; }
  .agd-filebtn { display: inline-block; width: fit-content; cursor: pointer; }
  .agd-disabled { opacity: 0.5; pointer-events: none; }
  /* ── E1: corpus browser ── */
  .agd-corpus-list {
    max-height: 12rem; overflow-y: auto; border: 1px solid #23232f; border-radius: 0.4rem;
    background: #0a0a10; display: flex; flex-direction: column;
  }
  .agd-corpus-row {
    display: flex; justify-content: space-between; gap: 0.5rem; align-items: center;
    padding: 0.2rem 0.5rem; font-family: ui-monospace, monospace; font-size: 0.62rem;
    color: #a1a1aa; text-align: left; border-bottom: 1px solid #16161f;
  }
  .agd-corpus-row:hover { background: #16161f; color: #e4e4e7; }
  .agd-corpus-row.sel { background: rgba(99, 102, 241, 0.14); color: #c4b5fd; }
  .agd-corpus-path { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .agd-corpus-size { color: #52525b; flex: none; }
  .agd-corpus-view { border: 1px dashed #3f3f5a; border-radius: 0.5rem; padding: 0.5rem; display: flex; flex-direction: column; gap: 0.35rem; }
  .agd-corpus-pre {
    font-family: ui-monospace, monospace; font-size: 0.62rem; color: #c4c4cc; background: #0a0a10;
    border-radius: 0.35rem; padding: 0.5rem; max-height: 16rem; overflow: auto; white-space: pre-wrap;
    word-break: break-word; line-height: 1.45;
  }
  /* ── E1: add-skill ── */
  .agd-chip-add { cursor: pointer; }
  .agd-chip-add:hover:not(:disabled) { color: #6ee7b7; background: rgba(52, 211, 153, 0.12); border-color: rgba(52, 211, 153, 0.4); }
  .agd-chip-add:disabled { opacity: 0.5; cursor: default; }
  .agd-skill-upload { display: flex; flex-direction: column; gap: 0.35rem; border-top: 1px solid #1c1c26; padding-top: 0.5rem; }
  .agd-skill-upload input[type='file'] { font-size: 0.62rem; color: #a1a1aa; }

  /* ── E2: multi-pane web SSH terminal ── */
  .term-wrap {
    height: 100%; display: flex; flex-direction: column;
    padding: 0.85rem 1rem 1rem; gap: 0.75rem;
  }
  .term-toolbar {
    display: flex; flex-wrap: wrap; align-items: center; gap: 0.4rem;
    padding-bottom: 0.6rem; border-bottom: 1px solid #16161f;
  }
  .term-tb-label {
    font-family: ui-monospace, monospace; font-size: 0.66rem; text-transform: uppercase;
    letter-spacing: 0.08em; color: #71717a; margin-right: 0.15rem;
  }
  .term-plus { color: #6ee7b7; font-weight: 700; margin-right: 0.1rem; }
  .term-open-btn {
    display: inline-flex; align-items: center; gap: 0.35rem;
    padding: 0.28rem 0.6rem; border-radius: 0.45rem; cursor: pointer;
    font-size: 0.72rem; color: #d4d4d8;
    background: #101019; border: 1px solid #26263a;
    transition: border-color 0.12s, color 0.12s, background 0.12s;
  }
  .term-open-btn:hover { color: #ddd6fe; border-color: rgba(167, 139, 250, 0.55); background: rgba(167, 139, 250, 0.1); }
  .term-closeall {
    margin-left: auto; font-size: 0.68rem; font-family: ui-monospace, monospace;
    color: #fca5a5; background: rgba(248, 113, 113, 0.08);
    border: 1px solid rgba(248, 113, 113, 0.3); border-radius: 0.4rem;
    padding: 0.26rem 0.55rem; cursor: pointer;
  }
  .term-closeall:hover { background: rgba(248, 113, 113, 0.18); border-color: rgba(248, 113, 113, 0.55); }

  .term-empty {
    flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center;
    gap: 0.4rem; text-align: center; color: #71717a;
  }
  .term-empty-glyph {
    font-family: ui-monospace, monospace; font-size: 2.4rem; font-weight: 700;
    color: rgba(167, 139, 250, 0.45); letter-spacing: -0.05em; margin-bottom: 0.3rem;
  }
  .term-empty p { font-size: 0.85rem; }
  .term-empty-sub { max-width: 30rem; font-size: 0.72rem; color: #52525b; line-height: 1.5; }
  .term-empty code {
    font-family: ui-monospace, monospace; font-weight: 700; color: #c4b5fd;
    background: rgba(167, 139, 250, 0.12); padding: 0 0.25rem; border-radius: 0.25rem;
  }

  .term-grid {
    flex: 1; min-height: 0; display: grid; gap: 0.7rem;
    grid-template-columns: repeat(var(--cols, 1), minmax(0, 1fr));
    grid-auto-rows: minmax(16rem, 1fr);
  }
  @media (max-width: 900px) {
    .term-grid { grid-template-columns: 1fr; }
  }
  .term-pane {
    display: flex; flex-direction: column; min-height: 0; overflow: hidden;
    border: 1px solid #23233360; border-radius: 0.6rem; background: #0b0b12;
    box-shadow: 0 1px 0 rgba(255, 255, 255, 0.02) inset, 0 6px 18px rgba(0, 0, 0, 0.35);
  }
  .term-pane-head {
    display: flex; align-items: center; gap: 0.45rem; flex: none;
    padding: 0.35rem 0.55rem; background: #101019; border-bottom: 1px solid #1c1c28;
  }
  .term-dot { width: 0.5rem; height: 0.5rem; border-radius: 999px; background: #3f3f46; flex: none; }
  .term-dot.on { background: #34d399; box-shadow: 0 0 7px #34d399; }
  .term-dot.connecting { background: #fbbf24; box-shadow: 0 0 7px #fbbf24; animation: term-pulse 1s ease-in-out infinite; }
  .term-dot.off { background: #f87171; box-shadow: 0 0 6px #f8717180; }
  @keyframes term-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.35; } }
  .term-pane-label {
    font-size: 0.74rem; color: #e4e4e7; font-weight: 600;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0;
  }
  .term-pane-state {
    font-family: ui-monospace, monospace; font-size: 0.6rem; text-transform: uppercase;
    letter-spacing: 0.06em; color: #71717a; margin-left: 0.1rem;
  }
  .term-reconnect, .term-pane-close {
    line-height: 1; border-radius: 0.3rem; cursor: pointer; padding: 0.12rem 0.3rem;
    background: transparent; border: 1px solid transparent; color: #a1a1aa; font-size: 0.8rem;
  }
  .term-reconnect { margin-left: auto; }
  .term-pane-close { margin-left: 0.1rem; }
  .term-reconnect:hover { color: #c4b5fd; border-color: rgba(167, 139, 250, 0.4); }
  .term-pane-close:hover { color: #fca5a5; border-color: rgba(248, 113, 113, 0.4); }
  /* the xterm container — fills the pane below the header */
  .term-pane-body { flex: 1; min-height: 0; padding: 0.3rem 0.1rem 0.1rem 0.4rem; overflow: hidden; }
  .term-pane-body :global(.xterm) { height: 100%; }
  .term-pane-body :global(.xterm-viewport) { background: transparent !important; }
  .term-pane-body :global(.xterm-viewport)::-webkit-scrollbar { width: 8px; }
  .term-pane-body :global(.xterm-viewport)::-webkit-scrollbar-thumb { background: #2a2a3a; border-radius: 4px; }

  /* ── F7a: persona (Soul/Identity) editor ── */
  .agd-persona-edit {
    width: 100%; min-height: 11rem; resize: vertical;
    font-family: ui-monospace, monospace; font-size: 0.66rem; color: #d4d4dc; line-height: 1.5;
    background: #0a0a10; border: 1px solid #23232f; border-radius: 0.4rem; padding: 0.5rem;
  }
  .agd-persona-edit:focus { outline: none; border-color: #6d28d9; }
  .agd-persona-edit:disabled { opacity: 0.6; }
  .agd-persona-short { min-height: 6rem; }
  .agd-persona-actions { display: flex; align-items: center; gap: 0.6rem; flex-wrap: wrap; }

  /* ── F7b: new-persona button + modal ── */
  .new-persona-btn {
    font-family: ui-monospace, monospace; font-size: 0.6rem; color: #6ee7b7;
    background: rgba(52, 211, 153, 0.1); border: 1px solid rgba(52, 211, 153, 0.35);
    border-radius: 0.4rem; padding: 0.18rem 0.55rem; cursor: pointer; line-height: 1.2;
    display: inline-flex; align-items: center; gap: 0.25rem;
  }
  .new-persona-btn:hover { background: rgba(52, 211, 153, 0.18); border-color: rgba(52, 211, 153, 0.6); color: #a7f3d0; }
  .np-plus { font-size: 0.85rem; font-weight: 700; line-height: 1; }
  .np-modal { max-width: 640px; }
  .np-row2 { display: grid; grid-template-columns: 1fr 1fr; gap: 0.6rem; }
  .np-emoji-field { max-width: 7rem; }
  .np-field { display: flex; flex-direction: column; gap: 0.25rem; }
  .np-lbl { font-family: ui-monospace, monospace; font-size: 0.58rem; text-transform: uppercase; letter-spacing: 0.06em; color: #a1a1aa; }
  .np-submit { display: flex; align-items: center; gap: 0.6rem; flex-wrap: wrap; }
  .np-result-ok { border-color: rgba(52, 211, 153, 0.35); }
  .np-steps { display: flex; flex-direction: column; gap: 0.25rem; font-family: ui-monospace, monospace; font-size: 0.62rem; }
  .np-steps li { display: flex; align-items: baseline; gap: 0.4rem; flex-wrap: wrap; }
  .np-ok .np-step-mark { color: #6ee7b7; }
  .np-bad .np-step-mark { color: #fca5a5; }
  .np-step-name { color: #d4d4dc; }
  .np-step-detail { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; opacity: 0.8; }
  .np-manual { margin-top: 0.2rem; }
  .np-why { font-size: 0.62rem; }
  .np-cmd { max-height: 11rem; }
</style>
