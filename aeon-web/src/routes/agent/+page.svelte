<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from '$lib/api';

  let tab: 'overview' | 'systems' | 'containers' = 'overview';
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
  // ── E5: Easy Deploy (dgx-only) ──
  let deployCat: api.DeployCatalog | null = null;
  let deployImages: api.DeployCatalogEntry[] = []; // editable working copy
  let deploySel = 0;                               // index into deployImages
  let deployName = '';
  let dfModelLen: number | null = 4096;
  let dfMaxBatch: number | null = 256;
  let dfGpu = 'all';
  let dfMaxSessions: number | null = 8;
  let deployNow = false;
  let deployBusy = false;
  let deployResult: api.DeployResult | null = null;
  let deployErr = '';

  $: openclawSystems = systems.filter((s) => s.roles?.includes('openclaw'));
  $: cSysObj = systems.find((s) => s.id === cSys) ?? null;
  $: cIsDgx = cSysObj?.roles?.includes('dgx') ?? false;

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

  // ── connected-systems actions (unchanged) ───────────────────────────
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
    } finally {
      adding = false;
    }
  }
  async function onRegister(s: api.ConnectedSystem) {
    const pw = prompt(
      `One-time SSH password for ${s.ssh_user}@${s.address} — used ONCE to install the Pi's key, never stored. Leave blank if you'll authorize manually.`,
    );
    if (pw === null) return;
    busyId = s.id;
    delete fallback[s.id];
    fallback = fallback;
    try {
      const res = await api.registerSystem(s.id, pw);
      if (!res.ok && res.authorize_command) {
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
    deployResult = null;
    deployErr = '';
    loadContainers();
    if (systems.find((s) => s.id === sysId)?.roles?.includes('dgx')) loadDeployCatalog();
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
  function cStateCls(state: string): string {
    if (state === 'running') return 'cst-run';
    if (state === 'exited' || state === 'dead') return 'cst-exit';
    return 'cst-other';
  }
  /** "12.3%" → clamped 0-100 number for a bar width. */
  function pctNum(s?: string): number {
    if (!s) return 0;
    const n = parseFloat(s.replace('%', ''));
    return isNaN(n) ? 0 : Math.max(0, Math.min(100, n));
  }

  // ── E5: Easy Deploy ─────────────────────────────────────────────────────
  async function loadDeployCatalog() {
    deployErr = '';
    try {
      const r = await api.getDeployCatalog(cSys);
      if (r.ok) {
        deployCat = r;
        deployImages = (r.catalog ?? []).map((c) => ({ ...c }));
        deploySel = 0;
        if (deployImages.length && !deployName) deployName = suggestName(deployImages[0]);
      } else {
        deployCat = r; // carries err (e.g. not a dgx)
      }
    } catch (e) {
      deployErr = (e as any)?.message ?? String(e);
    }
  }
  function suggestName(c: api.DeployCatalogEntry): string {
    // ghcr.io/aeon-7/vllm-model-server:latest → "vllm-model-server"
    const base = (c?.image ?? '').split('/').pop() ?? '';
    return base.split(':')[0] || 'aeon-deploy';
  }
  function onPickImage(i: number) {
    deploySel = i;
    deployName = suggestName(deployImages[i]);
  }
  async function doDeploy() {
    const entry = deployImages[deploySel];
    if (!entry) return;
    if (deployNow && !confirm(
      `Deploy now will PULL ${entry.image} (can be multi-GB) and run docker compose up -d on ${cSysObj?.label}.\n\nContinue?`,
    )) return;
    deployBusy = true;
    deployErr = '';
    deployResult = null;
    try {
      const r = await api.deployImage(cSys, {
        image: entry.image,
        name: deployName,
        kind: entry.kind,
        flags: {
          model_len: dfModelLen,
          max_batch: dfMaxBatch,
          gpu: dfGpu,
          max_sessions: dfMaxSessions,
        },
        deploy_now: deployNow,
      });
      deployResult = r;
      if (!r.ok) deployErr = r.err ?? 'deploy failed';
      if (r.ok && deployNow) await loadContainers(true);
    } catch (e) {
      deployErr = (e as any)?.message ?? String(e);
    } finally {
      deployBusy = false;
    }
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
                {#if r?.reachable}
                  <div class="roster-stats">
                    <span><b>{sum.count}</b> agents</span>
                    <span class="dot-sep">·</span>
                    <span class="text-live-300"><b>{sum.active}</b> active</span>
                  </div>
                {:else if r}
                  <span class="text-amber-300 text-[11px] font-mono">{r.err || 'roster unavailable'}</span>
                {:else}
                  <span class="text-zinc-600 text-[11px] font-mono">loading roster…</span>
                {/if}
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
                      <span class="ctr-state {cStateCls(c.state)}">{c.state}</span>
                      <div class="ctr-id">
                        <div class="ctr-name">{c.name}</div>
                        <div class="ctr-img" title={c.image}>{c.image}</div>
                      </div>
                      <div class="ctr-actions">
                        {#if c.state === 'running'}
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

          <!-- ── E5: Easy Deploy (dgx-only) ── -->
          {#if cIsDgx}
            <section class="deploy">
              <div class="flex items-baseline justify-between flex-wrap gap-2">
                <h2 class="section-title">Easy Deploy <span class="text-zinc-600 font-normal">· AEON-7 GHCR → {cSysObj?.label}</span></h2>
                <span class="text-[10px] font-mono text-amber-300/80">dgx-only</span>
              </div>
              <p class="agd-dim">
                Pick a GHCR image, tune the flags, and generate a compose file on the box.
                Default is <b>generate &amp; save only</b> (no multi-GB pull). Tick “deploy now” to also run
                <code>docker compose up -d</code>.
              </p>

              {#if deployCat && !deployCat.ok}
                <p class="text-amber-300 text-xs font-mono">{deployCat.err}</p>
              {/if}
              {#if deployErr}<p class="text-red-400 text-xs font-mono">{deployErr}</p>{/if}

              <!-- editable image list (seeded placeholders) -->
              <div class="dep-images">
                {#each deployImages as img, i (i)}
                  <div class="dep-img" class:active={deploySel === i}>
                    <button class="dep-pick" on:click={() => onPickImage(i)} title="Select this image">
                      <span class="dep-radio" class:on={deploySel === i}></span>
                    </button>
                    <div class="dep-img-body">
                      <input class="dep-in dep-img-ref" bind:value={img.image} placeholder="ghcr.io/aeon-7/…" />
                      <div class="dep-img-meta">
                        <input class="dep-in dep-img-label" bind:value={img.label} placeholder="label" />
                        <select class="dep-in dep-kind" bind:value={img.kind}>
                          <option value="model-server">model-server</option>
                          <option value="comfyui">comfyui</option>
                          <option value="generic">generic</option>
                        </select>
                      </div>
                      {#if img.note}<div class="dep-note">{img.note}</div>{/if}
                    </div>
                  </div>
                {/each}
                <div class="dep-todo">
                  TODO: live AEON-7 GHCR catalog fetch needs <code>gh</code> / a registry token (not available here).
                  Edit the entries above by hand for now.
                </div>
              </div>

              <!-- flags -->
              <div class="dep-flags">
                <label class="dep-f"><span>name</span>
                  <input class="dep-in" bind:value={deployName} placeholder="container name" /></label>
                <label class="dep-f"><span>max model length</span>
                  <input class="dep-in" type="number" min="0" bind:value={dfModelLen} /></label>
                <label class="dep-f"><span>max batch size</span>
                  <input class="dep-in" type="number" min="0" bind:value={dfMaxBatch} /></label>
                <label class="dep-f"><span>GPU (all / N / 0,1)</span>
                  <input class="dep-in" bind:value={dfGpu} placeholder="all" /></label>
                <label class="dep-f"><span>max concurrent sessions</span>
                  <input class="dep-in" type="number" min="0" bind:value={dfMaxSessions} /></label>
              </div>

              <div class="dep-go">
                <label class="dep-now"><input type="checkbox" bind:checked={deployNow} /> deploy now (pull + up -d)</label>
                <button class="btn-primary text-xs" on:click={doDeploy} disabled={deployBusy || !deployName.trim()}>
                  {deployBusy ? 'working…' : deployNow ? 'generate + deploy' : 'generate + save compose'}
                </button>
              </div>

              {#if deployResult?.ok}
                <div class="dep-result">
                  <div class="text-[11px] font-mono text-live-300">
                    {deployResult.deployed ? 'deployed' : 'compose written'} → {deployResult.path}
                  </div>
                  {#if deployResult.compose}<pre class="cmp-ta-pre">{deployResult.compose}</pre>{/if}
                  {#if deployResult.out}<pre class="cmp-out">{deployResult.out}</pre>{/if}
                </div>
              {/if}

              <div class="dep-agenttodo">
                <b>TODO (agent hand-off):</b> dispatch the repo’s <code>agents.md</code> + this generated compose to an
                OpenClaw agent with SSH access to the DGX to pull / tune / launch it. Needs the agent-task dispatch
                channel (not wired yet).
              </div>
            </section>
          {/if}
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
              <div class="flex gap-2">
                <button class="btn text-xs" on:click={() => onRegister(s)} disabled={busyId === s.id}>register (SSH key)</button>
                <button class="btn text-xs" on:click={() => onTest(s)} disabled={busyId === s.id}>test</button>
                <button class="btn text-xs ml-auto text-red-300" on:click={() => onRemove(s)}>remove</button>
              </div>
              {#if fallback[s.id]}
                <div class="text-[10px] font-mono text-amber-300 space-y-1">
                  <p>Password auth unavailable — run this on {s.address}, then Test:</p>
                  <code class="block bg-ink-900 rounded p-2 break-all text-zinc-200">{fallback[s.id]}</code>
                </div>
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

  /* ── E5: Easy Deploy ── */
  .deploy {
    border: 1px solid rgba(245, 158, 11, 0.25); border-radius: 0.75rem; padding: 0.9rem 1rem;
    background: linear-gradient(160deg, rgba(50, 40, 20, 0.25), rgba(12, 12, 20, 0.5)); display: flex; flex-direction: column; gap: 0.65rem;
  }
  .dep-images { display: flex; flex-direction: column; gap: 0.45rem; }
  .dep-img { display: flex; gap: 0.5rem; align-items: flex-start; padding: 0.5rem; border: 1px solid #23232f; border-radius: 0.5rem; background: rgba(12, 12, 20, 0.5); }
  .dep-img.active { border-color: rgba(167, 139, 250, 0.5); background: rgba(167, 139, 250, 0.08); }
  .dep-pick { background: none; border: none; cursor: pointer; padding: 0.2rem 0; }
  .dep-radio { display: inline-block; width: 0.85rem; height: 0.85rem; border-radius: 999px; border: 2px solid #52525b; }
  .dep-radio.on { border-color: #a78bfa; background: #a78bfa; box-shadow: 0 0 8px #a78bfa88; }
  .dep-img-body { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 0.3rem; }
  .dep-img-meta { display: flex; gap: 0.4rem; }
  .dep-in {
    background: #0a0a10; border: 1px solid #2a2a38; border-radius: 0.35rem; padding: 0.25rem 0.45rem;
    font-family: ui-monospace, monospace; font-size: 0.66rem; color: #d4d4dc;
  }
  .dep-in:focus { outline: none; border-color: #6d5fd0; }
  .dep-img-ref { width: 100%; }
  .dep-img-label { flex: 1; }
  .dep-kind { flex: none; }
  .dep-note { font-size: 0.58rem; color: #71717a; font-style: italic; }
  .dep-todo, .dep-agenttodo {
    font-size: 0.6rem; color: #d4b483; background: rgba(245, 158, 11, 0.08);
    border: 1px dashed rgba(245, 158, 11, 0.3); border-radius: 0.4rem; padding: 0.45rem 0.55rem; line-height: 1.45;
  }
  .dep-flags { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 0.5rem; }
  .dep-f { display: flex; flex-direction: column; gap: 0.2rem; }
  .dep-f > span { font-family: ui-monospace, monospace; font-size: 0.56rem; color: #8b8b96; text-transform: uppercase; letter-spacing: 0.04em; }
  .dep-f .dep-in { width: 100%; }
  .dep-go { display: flex; align-items: center; gap: 0.8rem; flex-wrap: wrap; }
  .dep-now { font-family: ui-monospace, monospace; font-size: 0.66rem; color: #d4d4dc; display: inline-flex; align-items: center; gap: 0.35rem; }
  .dep-result { display: flex; flex-direction: column; gap: 0.4rem; }

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
</style>
