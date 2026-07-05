// Tiny typed REST client for /api/*. The supervisor proxies these to the
// streamer + hid daemons. All POSTs send JSON; all GETs return JSON unless
// noted otherwise (snapshot returns a JPEG blob).

const API = '/api';

export interface StreamerState {
  ok: boolean;
  mode?: { format: string; resolution: string };
  online: boolean;
  captured_fps: number;
  enum_hash?: string;
  relaunch_count?: number;
  pipeline?: string | null;
}

export interface HidStatus {
  ok: boolean;
  persona: string;
  keyboard_online?: boolean;
  mouse_online?: boolean;
}

export interface SupervisorState {
  ok: boolean;
  streamer: StreamerState | null;
  hid: HidStatus | null;
}

export interface MeResponse {
  ok: boolean;
  state: 'open' | 'locked';
  needs_setup?: boolean;
  authenticated?: boolean;
  user?: string;
  scope?: 'admin' | 'full' | 'macros' | 'read';
  admin_username_default?: string;
  /// True on the headless "Orb server" container (no HID/video/GPIO/host-network).
  server?: boolean;
}

export interface Token {
  id: string;
  name: string;
  scope: 'admin' | 'full' | 'macros' | 'read';
  created_at_ms: number;
  last_used_at_ms: number | null;
}

export interface NewTokenResponse {
  ok: boolean;
  id: string;
  name: string;
  scope: string;
  token: string; // plaintext — shown only once
}

export class HttpError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function req<T>(
  method: string,
  path: string,
  body?: unknown,
  opts: { textResponse?: boolean } = {},
): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined && !(body instanceof FormData)) {
    headers['Content-Type'] = 'application/json';
  }
  const res = await fetch(API + path, {
    method,
    credentials: 'same-origin',
    headers,
    body: body !== undefined
      ? body instanceof FormData ? body : JSON.stringify(body)
      : undefined,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new HttpError(res.status, `HTTP ${res.status}: ${text.slice(0, 200)}`);
  }
  if (opts.textResponse) {
    return (await res.text()) as unknown as T;
  }
  return (await res.json()) as T;
}

// ── Auth / setup ───────────────────────────────────────────────────────

export const getMe = () => req<MeResponse>('GET', '/auth/me');

export const setupPassword = (password: string, username = 'admin') =>
  req<{ ok: boolean; user: string }>('POST', '/setup/password', {
    username,
    password,
  });

export const login = (username: string, password: string) =>
  req<{ ok: boolean }>('POST', '/login', { username, password });

export const logout = () => req<{ ok: boolean }>('POST', '/logout');

export const changePassword = (current_password: string, new_password: string) =>
  req<{ ok: boolean }>('POST', '/auth/change-password', {
    current_password,
    new_password,
  });

// ── Tokens ────────────────────────────────────────────────────────────

export const listTokens = () =>
  req<{ tokens: Token[] }>('GET', '/auth/tokens');

export const createToken = (
  name: string,
  scope: 'admin' | 'full' | 'macros' | 'read' = 'full',
) => req<NewTokenResponse>('POST', '/auth/tokens', { name, scope });

export const revokeToken = (id: string) =>
  req<{ ok: boolean }>('DELETE', `/auth/tokens/${encodeURIComponent(id)}`);

// ── system ─────────────────────────────────────────────────────────────

export const getSystemState = () => req<SupervisorState>('GET', '/state');

// ── streamer ───────────────────────────────────────────────────────────

export const getStreamerState = () => req<StreamerState>('GET', '/streamer/state');
export const relaunchStreamer = () => req<{ ok: boolean }>('POST', '/streamer/relaunch');

// ── capture-source picker (which video source the console view shows) ──
export interface StreamerConfig {
  ok: boolean;
  source: string;            // auto | cam-link-usb | hdmi-csi | camera-csi
  platform: string;          // pi4 | pi5 | other
  available_sources: string[];
  fps?: number;
  format?: string;
  width?: number;            // encode width  (used when match_source = false)
  height?: number;           // encode height (used when match_source = false)
  match_source?: boolean;    // true = track source res (capped 1080p); false = fixed W×H
  rotation?: number;         // clockwise degrees: 0 | 90 | 180 | 270
  hflip?: boolean;           // mirror left↔right (after rotation)
  vflip?: boolean;           // mirror top↔bottom (after rotation)
}
export const getStreamerConfig = () => req<StreamerConfig>('GET', '/streamer/config');
export const setStreamerSource = (source: string) =>
  req<{ ok: boolean; source: string; restarted?: boolean }>('PUT', '/streamer/config', { source });

// Encode resolution. Lower W×H (or match_source:false at 1280×720) sharply cuts
// the Pi 5 software-H.264 CPU + power on the capture-card paths. Restarts the
// streamer so the new resolution applies to stream/snapshot/recording/vision.
export interface ResolutionPatch { width?: number; height?: number; match_source?: boolean }
export const setStreamerResolution = (r: ResolutionPatch) =>
  req<{ ok: boolean; width?: number; height?: number; match_source?: boolean; restarted?: boolean }>(
    'PUT', '/streamer/config', r);

// Server-side display orientation. Restarts the streamer so the rotated
// pipeline applies to the live stream, /snapshot, recordings, and vision.
export interface OrientationPatch { rotation?: number; hflip?: boolean; vflip?: boolean }
export const setStreamerOrientation = (o: OrientationPatch) =>
  req<{ ok: boolean; rotation?: number; hflip?: boolean; vflip?: boolean; restarted?: boolean }>(
    'PUT', '/streamer/config', o);

// ── USB webcam passthrough (expose a video source to the target as a webcam) ──
export interface WebcamConfig {
  ok: boolean;
  enabled: boolean;
  source: string;            // off | camera-csi | hdmi-csi | cam-link-usb
  available_sources: string[];
  supported: boolean;        // gadget supported (usb_f_uvc present)
}
export const getWebcamConfig = () => req<WebcamConfig>('GET', '/webcam');
export const setWebcam = (source: string) =>      // "off" disables the webcam function
  req<{ ok: boolean; source: string; restarted?: boolean }>('PUT', '/webcam', { source });

// ── UPS / battery (Waveshare UPS HAT (E) — 2× 21700, I2C 0x2D) ──
// Mirrors /run/aeon/ups.json (aeon-ups.py) relayed by the supervisor at /api/ups.
// `present:false` means no UPS HAT detected (mains-only Orb) — hide the meter.
export interface UpsStatus {
  present: boolean;
  ok?: boolean;
  state?: string;             // "fast-charging" | "charging" | "discharging" | "idle"
  charging?: boolean;
  fast_charging?: boolean;
  on_battery?: boolean;       // running off cells (no meaningful Type-C input)
  percent?: number;           // 0–100 pack state-of-charge
  battery_mv?: number;
  battery_ma?: number;        // <0 = discharging into the Pi
  remaining_mah?: number;
  minutes_to_empty?: number | null;   // present only while discharging
  minutes_to_full?: number | null;    // present only while charging
  vbus_mv?: number;           // Type-C input voltage (mV)
  cells_mv?: number[];        // per-21700-cell voltages
  min_cell_mv?: number;
  low_voltage_mv?: number;    // safe-shutdown threshold (per cell)
  shutdown_pending_s?: number | null; // counting down to low-batt poweroff, else null
}
export const getUps = () => req<UpsStatus>('GET', '/ups');

// ── Audio in/out volume (BrainCraft WM8960 via amixer) ──
// present:false when no codec card is fitted (mains-only / no BrainCraft).
export interface AudioVolume {
  present: boolean;
  card?: { index: number; name: string };
  playback?: { control: string; percent: number } | null;   // speaker/headphone
  capture?: { control: string; percent: number; muted: boolean } | null; // mic
}
export const getAudioVolume = () => req<AudioVolume>('GET', '/audio/volume');
export const setAudioVolume = (p: { playback?: number; capture?: number; capture_muted?: boolean }) =>
  req<AudioVolume>('PUT', '/audio/volume', p);

// ── fleet (decentralized roster of peer Orbs over the tailnet/LAN) ──
// Offline peers come back as { online:false, addr } stubs — the rich fields
// are only present for Orbs that answered their heartbeat, so all but `online`
// are optional.
/// One AI model shared over IPFS by an Orb. origin_* = the fleet identity of
/// the Orb that FIRST shared it (provenance survives mirroring).
export interface IpfsModel {
  cid: string;
  name: string;
  size_bytes: number;
  sha256?: string;
  kind?: string;             // llm | vlm | vision | stt | tts | other
  desc?: string;
  license?: string;
  added_at_ms?: number;
  origin_id?: string;
  origin_label?: string;
}

export interface FleetOrb {
  online: boolean;
  addr?: string;
  is_self?: boolean;
  id?: string;
  hostname?: string;
  label?: string;
  model?: string;            // pi5 | pi4 | other
  lan_ip?: string;
  addrs?: string[];
  sources?: string[];
  view_source?: string;
  webcam?: { enabled: boolean; source: string };
  ups?: { present: boolean; charge?: number; on_battery?: boolean };
  health?: {
    uptime_seconds: number;
    cpu_temp_c: number;
    loadavg: { '1m': number; '5m': number; '15m': number };
    mem_total_kb: number;
    mem_available_kb: number;
  };
  version?: string;
  /// Shared-model catalog + kubo PeerID, riding the fleet heartbeat — the
  /// federated model index is the union of this field across the roster.
  ipfs_models?: { enabled: boolean; peer_id?: string; models?: IpfsModel[] };
}
export interface FleetRoster {
  ok: boolean;
  configured: boolean;       // false until this Orb has a shared fleet token
  self: FleetOrb;
  peers: FleetOrb[];
}
export const getFleetRoster = () => req<FleetRoster>('GET', '/fleet/roster');

export interface FleetConfig {
  ok: boolean;
  configured: boolean;
  id: string;
  label: string;
  tailscale: boolean;
  seeds: string[];
}
export const getFleetConfig = () => req<FleetConfig>('GET', '/fleet/config');
export const setFleetConfig = (patch: { label?: string; seeds?: string[]; tailscale?: boolean }) =>
  req<{ ok: boolean }>('PUT', '/fleet/config', patch);

// ── screen recording ──
export interface RecordingInfo {
  id: string;
  started_ms: number;
  duration_s: number;
  status: string;
  elapsed_s?: number;
  size_bytes?: number;
  has_thumb?: boolean;
}
export const recordStart = (duration_s?: number) =>
  req<{ ok: boolean; recording: RecordingInfo }>('POST', '/streamer/record/start', { duration_s });
export const recordStop = () =>
  req<{ ok: boolean; recording: RecordingInfo }>('POST', '/streamer/record/stop');
export const getRecordingState = () =>
  req<{
    ok: boolean;
    active: RecordingInfo | null;
    recordings: RecordingInfo[];
    note?: string | null;
  }>('GET', '/streamer/record/state');
export const deleteRecording = (id: string) =>
  req<{ ok: boolean }>('DELETE', `/streamer/recordings/${id}`);
export const recordingURL = (id: string) => `${API}/streamer/recordings/${id}`;
export const recordingThumbURL = (id: string) => `${API}/streamer/recordings/${id}/thumb`;

// ── Agent Dash — Connected Systems ──
export interface ConnectedSystem {
  id: string;
  label: string;
  address: string;
  ssh_user: string;
  port: number;
  roles: string[];
  status: string;
  last_checked_ms: number;
}
export const getAgentPubkey = () =>
  req<{ ok: boolean; pubkey: string; authorize_command: string }>('GET', '/agent/pubkey');
export const listSystems = () =>
  req<{ ok: boolean; systems: ConnectedSystem[] }>('GET', '/agent/systems');
export const addSystem = (body: {
  label: string;
  address: string;
  ssh_user: string;
  port: number;
  roles: string[];
}) => req<{ ok: boolean; id?: string; err?: string }>('POST', '/agent/systems', body);
export const removeSystem = (id: string) =>
  req<{ ok: boolean }>('DELETE', `/agent/systems/${id}`);
export const registerSystem = (id: string, password: string) =>
  req<{ ok: boolean; status?: string; err?: string; authorize_command?: string; hint?: string }>(
    'POST',
    `/agent/systems/${id}/register`,
    { password },
  );
export const testSystem = (id: string) =>
  req<{ ok: boolean; status?: string }>('POST', `/agent/systems/${id}/test`);

// ── Aeon Bench — deploy the benchmarking pod (this Orb or a connected server) ──
export interface BenchStatus {
  ok: boolean;
  phase?: string; // queued|cloning|building|installing-docker|running|failed|stopped|idle
  log?: string;
  running?: boolean;
  dash_port?: number;
  host?: string | null; // remote target address; null → use the browser's own host
  err?: string;
}
export const benchDeploy = (body: {
  target: string; // 'local' or a connected-system id
  hf_link: string;
  hf_token?: string;
  env?: Record<string, string>;
}) =>
  req<{ ok: boolean; target?: string; dash_port?: number; out?: string; err?: string }>(
    'POST',
    '/bench/deploy',
    body,
  );
export const benchStatus = (target: string) =>
  req<BenchStatus>('GET', `/bench/status?target=${encodeURIComponent(target)}`);
export const benchStop = (target: string) =>
  req<{ ok: boolean; err?: string }>('POST', '/bench/stop', { target });
export interface BenchUpdates {
  ok: boolean;
  deployed?: string | null; // short commit deployed on the target
  latest?: string | null; // short commit available on GitHub
  update_available?: boolean;
  err?: string;
}
// Is a newer Aeon-Bench-Pod build available for the pod deployed on this target?
export const benchUpdates = (target: string) =>
  req<BenchUpdates>('GET', `/bench/updates?target=${encodeURIComponent(target)}`);
// Hot-update the pod in place (fetch latest + rebuild, keep the model config).
export const benchUpdate = (target: string) =>
  req<{ ok: boolean; target?: string; out?: string; err?: string }>('POST', '/bench/update', {
    target,
  });
export interface BenchModelInfo {
  ok: boolean;
  id?: string;
  quant?: string | null; // e.g. "AWQ" / "GPTQ" / "FP8" / null = full precision
  native_ctx?: number | null;
  effective_ctx?: number | null;
  gated?: boolean;
  is_gguf?: boolean;
  params_b?: number | null;
  dtype?: string | null;
  arch?: string | null;
  warnings?: string[];
  err?: string;
}
export const benchModelInfo = (hfLink: string) =>
  req<BenchModelInfo>('GET', `/bench/model-info?hf_link=${encodeURIComponent(hfLink)}`);
// Devices discovered on the Pi's tailnet (for "+ Add device from Tailscale").
// `address` is the stable 100.x Tailscale IP we add the system by.
export interface TailscaleDevice {
  hostname: string;
  dns_name: string;
  address: string;
  os: string;
  online: boolean;
  is_self: boolean;
}
export const tailscaleDevices = () =>
  req<{ ok: boolean; up: boolean; devices: TailscaleDevice[]; err?: string }>(
    'GET',
    '/agent/tailscale/devices',
  );
export interface SystemMetrics {
  reachable: boolean;
  host?: string;
  load?: string;
  cpu?: string;   // CPU busy %, 0–100 integer string
  mem?: string;
  gpus?: { name: string; util: string; mem_used: string; mem_total: string; temp: string }[];
  containers?: string[];
  mac?: string;
  err?: string;
}
export const getSystemMetrics = (id: string) =>
  req<{ ok: boolean; metrics: SystemMetrics }>('GET', `/agent/systems/${id}/metrics`);
export const powerSystem = (id: string, action: 'shutdown' | 'reboot' | 'wake', mac = '') =>
  req<{ ok: boolean; action?: string; note?: string; err?: string; hint?: string }>(
    'POST',
    `/agent/systems/${id}/power`,
    { action, mac },
  );

// ── E4: Container Management ──
/** Live `docker stats` snapshot merged onto a container (null if not running). */
export interface ContainerStats {
  cpu: string;       // "0.95%"
  mem: string;       // used side, e.g. "56.73MiB"
  mem_full: string;  // "56.73MiB / 30.14GiB"
  mem_pct: string;   // "0.18%"
}
export interface ContainerInfo {
  name: string;
  image: string;
  state: string;     // running | exited | created | paused | … (raw, may be unreliable)
  status: string;    // "Up 4 hours" | "Exited (137) 2 months ago"
  running: boolean;  // robust: State=="running" || Status starts "Up"
  ports: string;
  compose_project: string;
  compose_config_files: string;
  stats: ContainerStats | null;
}
export interface ComposeFileEntry {
  path: string;      // absolute path on the system
  up: boolean;       // a running container references this compose file
}
export interface ContainerList {
  ok: boolean;
  containers?: ContainerInfo[];
  running?: number;
  total?: number;
  compose?: ComposeFileEntry[];
  err?: string;
}
export const getContainers = (sysId: string) =>
  req<ContainerList>('GET', `/agent/systems/${sysId}/containers`);

export const containerAction = (
  sysId: string,
  name: string,
  action: 'start' | 'stop' | 'restart',
) =>
  req<{ ok: boolean; action?: string; out?: string; err?: string }>(
    'POST',
    `/agent/systems/${sysId}/containers/${encodeURIComponent(name)}/action`,
    { action },
  );

export const getComposeFile = (sysId: string, path: string) =>
  req<{ ok: boolean; path?: string; content?: string; err?: string }>(
    'GET',
    `/agent/systems/${sysId}/compose?path=${encodeURIComponent(path)}`,
  );

export const putComposeFile = (sysId: string, path: string, content: string) =>
  req<{ ok: boolean; path?: string; err?: string }>(
    'PUT',
    `/agent/systems/${sysId}/compose?path=${encodeURIComponent(path)}`,
    { content },
  );

export const composeAction = (sysId: string, path: string, action: 'up' | 'down') =>
  req<{ ok: boolean; action?: string; out?: string; err?: string }>(
    'POST',
    `/agent/systems/${sysId}/compose/action`,
    { path, action },
  );

// ── E5: Easy Deploy — model+container picker, template flags, progress ──
/** The highlighted (tunable) flags + an advanced "extra" escape hatch. */
export interface DeployTemplateFlags {
  max_model_len: number;          // vLLM --max-model-len (0 = omit). Default 128k.
  gpu: string;                    // "all" | "1" | "0,1" — which/how many devices
  gpu_mem_util: number;           // 0.0–1.0 → vLLM --gpu-memory-utilization (the % VRAM slider). Default 0.7.
  max_num_batched_tokens: number; // vLLM --max-num-batched-tokens (0 = omit)
  max_num_seqs: number;           // vLLM --max-num-seqs = max concurrent sessions
  extra: string;                  // additional raw server args
}
/** A curated catalog entry: a model paired with its serving container + defaults. */
export interface DeployCatalogEntry {
  id: string;
  model: string;                  // HF model id / human label
  container_image: string;        // serving image (+ version), e.g. vllm/vllm-openai:vX
  kind: 'llm' | 'imagegen' | 'tts' | 'embedding' | string;
  description: string;
  template_flags: DeployTemplateFlags;
}
export interface DeployCatalog {
  ok: boolean;
  catalog?: DeployCatalogEntry[];
  deploy_dir?: string;
  allowed_via?: string;             // "dgx" | "docker+gpu"
  installed_containers?: ContainerInfo[]; // docker ps -a on the box
  installed_models?: string[];      // detected model ids / dir names
  live_catalog_todo?: string;
  ghcr_needs_token?: boolean;       // GHCR packages API is 401 unauth → containers seeded
  err?: string;
}
export const getDeployCatalog = (sysId: string) =>
  req<DeployCatalog>('GET', `/agent/systems/${sysId}/deploy/catalog`);

/** The flags POSTed to /deploy (null/0 = omit the corresponding server arg). */
export interface DeployFlags {
  max_model_len?: number | null;
  gpu?: string;                    // "all" | "1" | "0,1" — which/how many devices
  gpu_mem_util?: number;           // 0.0–1.0 → --gpu-memory-utilization (% VRAM slider)
  max_num_batched_tokens?: number | null;
  max_num_seqs?: number | null;
  extra?: string;
}
export interface DeployResult {
  ok: boolean;
  name?: string;           // deploy id for status polling
  image?: string;
  model?: string;
  kind?: string;
  compose?: string;        // the generated docker-compose.yml
  path?: string;           // where it was written on the box
  phase?: string;          // initial phase ("writing")
  out?: string;            // launcher output
  err?: string;
}
export const deployImage = (
  sysId: string,
  body: {
    model_id?: string;     // catalog entry id (preferred)
    image?: string;        // or a direct image override
    model?: string;        // model id (positional/--model for vLLM)
    name: string;
    kind: string;
    flags: DeployFlags;
  },
) => req<DeployResult>('POST', `/agent/systems/${sysId}/deploy`, body);

/** Progress poll for the install bar. */
export interface DeployStatus {
  ok: boolean;
  name?: string;
  phase?: 'writing' | 'pulling' | 'starting' | 'running' | 'failed' | string;
  percent?: number;        // 0–100
  done?: boolean;          // phase is running | failed
  log?: string;            // short tail of the docker output
  err?: string;
}
export const getDeployStatus = (sysId: string, name: string) =>
  req<DeployStatus>('GET', `/agent/systems/${sysId}/deploy/status?name=${encodeURIComponent(name)}`);

/** One agent in an OpenClaw gateway's pantheon (from its /agents API). */
export interface AgentInfo {
  id: string;
  name: string;
  emoji?: string;
  model?: string;
  provider?: string | null;
  active?: boolean;
  on_call?: boolean;
  working?: boolean;
  last_seen_s_ago?: number | null;
  tok_s?: number;
  pp_tok_s?: number;
  active_sessions?: number;
  runs_active?: number;
  subagents_active?: number;
  proc_rss_mb?: number;
  total_tokens?: number;
  in_tokens?: number;
  out_tokens?: number;
  context_tokens?: number | null;
  sessions?: number;
  current?: string | null;
  is_default?: boolean;
}
export interface AgentRoster {
  ok: boolean;
  reachable: boolean;
  ts?: number | null;
  warming?: boolean;
  agents?: AgentInfo[];
  err?: string;
}
export const getSystemAgents = (id: string) =>
  req<AgentRoster>('GET', `/agent/systems/${id}/agents`);

/** Locally-tracked token-usage history for a system (daily per-agent deltas). */
export interface UsageHistory {
  ok: boolean;
  first_day: string;
  today: string;
  gateway_total: number;
  daily: Record<string, { total: number; agents: Record<string, number> }>;
  names: Record<string, { name: string; emoji: string }>;
}
export const getSystemUsage = (id: string) =>
  req<UsageHistory>('GET', `/agent/systems/${id}/usage`);

/** Per-agent detail (gateway config + provisioned state) for the detail panel. */
export interface AgentDetail {
  ok: boolean;
  skills?: string[];
  voice?: string | null;
  corpus?: string | null;
  model?: string | null;
  available_skills?: string[];
  provisioned?: { token_id: string; at_ms: number; skill: string; dropped?: string | null } | null;
  ssh?: { user: string; admin: boolean; at_ms: number; pi_address?: string } | null;
  err?: string;
}
export const getAgentDetail = (sysId: string, agentId: string) =>
  req<AgentDetail>('GET', `/agent/systems/${sysId}/agents/${agentId}/detail`);
export const grantSsh = (sysId: string, agentId: string, admin: boolean) =>
  req<{
    ok: boolean;
    user?: string;
    admin?: boolean;
    pi_address?: string;
    private_key?: string;
    ssh_command?: string;
    dropped?: string | null;
    drop_err?: string | null;
    err?: string;
  }>('POST', `/agent/systems/${sysId}/agents/${agentId}/ssh`, { admin });
export const toggleSshAdmin = (sysId: string, agentId: string, admin: boolean) =>
  req<{ ok: boolean; admin?: boolean; err?: string }>(
    'PATCH',
    `/agent/systems/${sysId}/agents/${agentId}/ssh`,
    { admin },
  );
export const revokeSsh = (sysId: string, agentId: string) =>
  req<{ ok: boolean; user?: string }>('DELETE', `/agent/systems/${sysId}/agents/${agentId}/ssh`);
export const provisionAgent = (
  sysId: string,
  agentId: string,
  apiBase: string,
  scope: 'read' | 'full' = 'full',
) =>
  req<{
    ok: boolean;
    token_id?: string;
    token?: string;
    scope?: string;
    skill?: string;
    config_change?: string;
    dropped?: string | null;
    drop_err?: string | null;
    err?: string;
  }>('POST', `/agent/systems/${sysId}/agents/${agentId}/provision`, { api_base: apiBase, scope });
export const deprovisionAgent = (sysId: string, agentId: string) =>
  req<{ ok: boolean; revoked_token?: string | null }>(
    'DELETE',
    `/agent/systems/${sysId}/agents/${agentId}/provision`,
  );

// ── E1: per-agent profile photo → Matrix avatar ──
export interface AgentAvatar {
  ok: boolean;
  user_id?: string;
  avatar_url?: string;         // mxc://…  (empty string = none set)
  download_url?: string | null; // browser-renderable URL for the mxc
  err?: string;
}
export const getAgentAvatar = (sysId: string, agentId: string) =>
  req<AgentAvatar>('GET', `/agent/systems/${sysId}/agents/${agentId}/avatar`);
export const setAgentAvatar = (
  sysId: string,
  agentId: string,
  image_b64: string,
  content_type: string,
) =>
  req<AgentAvatar>('POST', `/agent/systems/${sysId}/agents/${agentId}/avatar`, {
    image_b64,
    content_type,
  });

// ── E1: per-agent corpus browse/view ──
export interface CorpusFileEntry {
  path: string;   // relative to the corpus root
  size: number;
}
export interface CorpusList {
  ok: boolean;
  root?: string | null;
  exists?: boolean;
  count?: number;
  files?: CorpusFileEntry[];
  err?: string;
}
export interface CorpusFile {
  ok: boolean;
  path?: string;
  size?: number;
  content?: string;
  err?: string;
}
export interface CorpusFileWrite {
  ok: boolean;
  path?: string;
  abs_path?: string;
  bytes_written?: number | null;
  err?: string;
}
export interface CorpusFileDelete {
  ok: boolean;
  path?: string;
  abs_path?: string;
  deleted?: boolean;
  err?: string;
}
export const getAgentCorpus = (sysId: string, agentId: string) =>
  req<CorpusList>('GET', `/agent/systems/${sysId}/agents/${agentId}/corpus`);
export const getAgentCorpusFile = (sysId: string, agentId: string, path: string) =>
  req<CorpusFile>(
    'GET',
    `/agent/systems/${sysId}/agents/${agentId}/corpus/file?path=${encodeURIComponent(path)}`,
  );
/** Upload a new corpus file or overwrite an existing one (content as base64). */
export const putAgentCorpusFile = (
  sysId: string,
  agentId: string,
  path: string,
  content_b64: string,
) =>
  req<CorpusFileWrite>('POST', `/agent/systems/${sysId}/agents/${agentId}/corpus/file`, {
    path,
    content_b64,
  });
export const deleteAgentCorpusFile = (sysId: string, agentId: string, path: string) =>
  req<CorpusFileDelete>(
    'DELETE',
    `/agent/systems/${sysId}/agents/${agentId}/corpus/file?path=${encodeURIComponent(path)}`,
  );

// ── E1: per-agent voice ──
export interface AgentVoice {
  ok: boolean;
  voice?: string | null;       // the effective voice value
  source?: string | null;      // where it came from (agent override / gateway default)
  kind?: 'clone' | 'designer' | 'none';
  is_override?: boolean;       // true = per-agent override, false = inherits global
  global?: string | null;      // the gateway-wide default voice
  provider?: string | null;    // TTS provider (e.g. "openai")
  // voip-env (gateway ~/voip-<id>/.env) — the *manageable* per-agent voice:
  env_path?: string | null;    // resolved .env path on the gateway
  env_exists?: boolean;        // whether that .env exists yet
  clone_name?: string | null;  // VOXTRAL_VOICE (named clone)
  description?: string | null; // VOXTRAL_VOICE_DESCRIPTION (designer prompt)
  err?: string;
}
export const getAgentVoice = (sysId: string, agentId: string) =>
  req<AgentVoice>('GET', `/agent/systems/${sysId}/agents/${agentId}/voice`);

// Voice management: designer-description + named-clone selection (gateway env);
// clone listing + upload (DGX/TTS host).
export interface VoiceDesignerResult {
  ok: boolean;
  env_path?: string;
  description?: string;
  err?: string;
}
export interface VoiceCloneSetResult {
  ok: boolean;
  env_path?: string;
  clone_name?: string;
  err?: string;
}
export interface VoiceClonesList {
  ok: boolean;
  clones?: string[];           // basenames of the DGX *.wav samples (no .wav)
  dir?: string;
  note?: string;               // e.g. "no DGX/TTS host registered"
  err?: string;
}
export interface VoiceCloneUploadResult {
  ok: boolean;
  clone_name?: string;
  wav_path?: string;
  bytes_written?: number | null;
  env_path?: string;
  note?: string;
  err?: string;
}
export const putAgentVoiceDesigner = (sysId: string, agentId: string, description: string) =>
  req<VoiceDesignerResult>('POST', `/agent/systems/${sysId}/agents/${agentId}/voice/designer`, {
    description,
  });
export const setAgentVoiceClone = (sysId: string, agentId: string, name: string) =>
  req<VoiceCloneSetResult>('POST', `/agent/systems/${sysId}/agents/${agentId}/voice/clone`, {
    name,
  });
export const getAgentVoiceClones = (sysId: string, agentId: string) =>
  req<VoiceClonesList>('GET', `/agent/systems/${sysId}/agents/${agentId}/voice/clones`);
export const uploadAgentVoiceClone = (
  sysId: string,
  agentId: string,
  name: string,
  wav_b64: string,
) =>
  req<VoiceCloneUploadResult>('POST', `/agent/systems/${sysId}/agents/${agentId}/voice/clone/upload`, {
    name,
    wav_b64,
  });

// ── E1: per-agent add-skill ──
export interface AddSkillResult {
  ok: boolean;
  skill?: string;
  dropped?: string | null;     // gateway dir the custom skill landed in
  config_change?: string;      // the non-invasive skills-array change to apply
  err?: string;
}
/** kind: 'existing' (quick-add, no file) | 'md' (SKILL.md) | 'tar' (tar/tgz). */
export const addAgentSkill = (
  sysId: string,
  agentId: string,
  name: string,
  kind: 'existing' | 'md' | 'tar',
  file_b64 = '',
) =>
  req<AddSkillResult>('POST', `/agent/systems/${sysId}/agents/${agentId}/skill`, {
    name,
    kind,
    file_b64,
  });

// ── F7a: per-agent persona files (Soul + Identity) ──
export type PersonaWhich = 'soul' | 'identity';
export interface PersonaFile {
  ok: boolean;
  which?: PersonaWhich;
  filename?: string;          // SOUL.md | IDENTITY.md
  workspace?: string | null;  // resolved workspace dir on the gateway
  path?: string | null;
  exists?: boolean;
  size?: number;
  content?: string;
  err?: string;
}
export interface PersonaFileWrite {
  ok: boolean;
  which?: PersonaWhich;
  filename?: string;
  path?: string;
  bytes_written?: number | null;
  err?: string;
}
export const getAgentPersonaFile = (sysId: string, agentId: string, which: PersonaWhich) =>
  req<PersonaFile>(
    'GET',
    `/agent/systems/${sysId}/agents/${agentId}/persona-file?which=${which}`,
  );
export const putAgentPersonaFile = (
  sysId: string,
  agentId: string,
  which: PersonaWhich,
  content: string,
) =>
  req<PersonaFileWrite>('PUT', `/agent/systems/${sysId}/agents/${agentId}/persona-file`, {
    which,
    content,
  });

// ── F7b: deploy a new agent persona ──
export interface NewPersonaInput {
  id: string;
  name?: string;
  emoji?: string;
  identity?: string;
  soul?: string;
  voice?: string;
  corpus_seed?: string;
  model?: string;
}
export interface PersonaStep {
  step: string;
  ok: boolean;
  detail: string;
}
export interface ManualStep {
  title: string;
  why: string;
  cmd: string;
}
export interface NewPersonaResult {
  ok: boolean;
  id?: string;
  display?: string;
  emoji?: string;
  model?: string;
  registered?: boolean;
  report?: { id?: string; workspace?: string; steps?: PersonaStep[]; registered?: boolean; err?: string };
  manual_steps?: ManualStep[];
  todo?: string;
  err?: string;
}
export const createPersona = (sysId: string, input: NewPersonaInput) =>
  req<NewPersonaResult>('POST', `/agent/systems/${sysId}/personas`, input);

export const snapshotURL = () => `${API}/streamer/snapshot?t=${Date.now()}`;
export const streamURL = () => `${API}/streamer/stream`;

// v64: H.264 low-latency live view over WebSocket (WebCodecs client). Same-origin
// wss:// so the aeon_session cookie authenticates the upgrade automatically
// (browsers can't set Authorization headers on a WebSocket handshake).
export const streamWsURL = () =>
  (location.protocol === 'https:' ? 'wss://' : 'ws://') +
  location.host +
  '/api/streamer/ws';

// E2: per-system interactive SSH terminal over WebSocket. Same-origin wss://
// so the aeon_session cookie authenticates the upgrade (Admin scope) — the
// supervisor spawns ssh inside a PTY against the registered system.
export const terminalWsURL = (id: string) =>
  (location.protocol === 'https:' ? 'wss://' : 'ws://') +
  location.host +
  `/api/agent/systems/${encodeURIComponent(id)}/terminal/ws`;

// ── HID ────────────────────────────────────────────────────────────────

export const getHidStatus = () => req<HidStatus>('GET', '/hid/status');

export const typeText = (text: string) =>
  req<{ ok: boolean }>('POST', '/hid/type', { text });

export const sendKey = (keys: string[], hold_ms = 50) =>
  req<{ ok: boolean }>('POST', '/hid/key', { keys, hold_ms });

export const click = (button: 'left' | 'right' | 'middle' = 'left', count = 1) =>
  req<{ ok: boolean }>('POST', '/hid/click', { button, count });

/// Press-and-hold (down=true) or release (down=false) a mouse button, for
/// click-and-drag. Held buttons ride along with subsequent moveMouse calls;
/// releaseAll clears them. A quick down→up with no move is just a click.
export const mouseButton = (down: boolean, button: 'left' | 'right' | 'middle' = 'left') =>
  req<{ ok: boolean }>('POST', '/hid/button', { button, down });

export const moveMouse = (dx: number, dy: number) =>
  req<{ ok: boolean }>('POST', '/hid/move', { dx, dy });

export const scroll = (dy: number) =>
  req<{ ok: boolean }>('POST', '/hid/scroll', { dy });

export const releaseAll = () => req<{ ok: boolean }>('POST', '/hid/release_all');

export const setPersona = (persona: string) =>
  req<{ ok: boolean }>('POST', '/hid/persona', { persona });

// ── Macros / prompts ───────────────────────────────────────────────────

export const listMacros = () => req<{ macros: string[] }>('GET', '/macros');
export const getMacro = (name: string) =>
  req<string>('GET', `/macros/${encodeURIComponent(name)}`, undefined, {
    textResponse: true,
  });
export const runMacro = (name: string, params: Record<string, string> = {}) =>
  req<{ ok: boolean; steps_run: number; snapshots?: string[]; error?: string }>(
    'POST',
    `/macros/${encodeURIComponent(name)}/run`,
    { params },
  );

export const listPrompts = () => req<{ prompts: string[] }>('GET', '/prompts');
export const getPrompt = (name: string) =>
  req<string>('GET', `/prompts/${encodeURIComponent(name)}`, undefined, {
    textResponse: true,
  });

// ── USB ethernet passthrough ───────────────────────────────────────────

export type UsbNetMode = 'isolation' | 'sharing' | 'restricted';

export interface UsbNetState {
  ok: boolean;
  enabled: boolean;
  mode: UsbNetMode;
  subnet: string;
  pi_addr: string;
  dhcp_range: string;
}

export const getUsbNet = () => req<UsbNetState>('GET', '/network/usb');

export const setUsbNet = (
  patch: { enabled?: boolean; mode?: UsbNetMode },
) =>
  req<{ ok: boolean; enabled: boolean; mode: string; hid_restarted: boolean }>(
    'PUT',
    '/network/usb',
    patch,
  );

// ── DNSCrypt (encrypted DNS) ───────────────────────────────────────────

export interface DnscryptProvider {
  id: string;
  label: string;
  blurb: string;
  transport: string;          // "DoH" / "DoT" / "DNSCrypt" / "any"
  log_policy: string;         // "no_logs" / "anonymized" / "self_logs" / "varies"
  log_detail: string;         // human-readable detail
  security: string;           // "basic" / "filtered" / "family" / "ad_block" / "varies"
  jurisdiction: string;       // ISO country code or "varies"
  homepage: string;
}

export interface DnscryptLocation {
  id: string;
  label: string;
}

/// Anonymized-relay metadata. v52 ships the full ~190-entry upstream
/// catalog with parsed country / operator / Eyes-tier annotations.
export interface AnonymizedRelay {
  name: string;          // matches dnscrypt-proxy resolver name
  label: string;
  operator: string;
  country: string;       // ISO 3166 alpha-2 (empty if unknown)
  eyes: 'none' | 'five' | 'nine' | 'fourteen' | 'unknown';
  no_logs: boolean;
  description?: string;  // first ~200 chars of the upstream description
}

export interface AnonymizedCriteria {
  no_logs?: boolean;
  outside_five_eyes?: boolean;
  outside_fourteen_eyes?: boolean;
  dnssec?: boolean;
}

export interface AnonymizedState {
  enabled: boolean;
  mode: 'auto' | 'specific';
  criteria: AnonymizedCriteria;
  specific_relays: string[];
  currently_picked: string[];
  catalog: AnonymizedRelay[];
}

/// Full upstream DNSCrypt v2 resolver catalog (v55+). ~226 entries
/// parsed from public-resolvers.md with privacy + trust scoring.
export interface DnscryptCatalogEntry {
  name: string;
  label: string;
  operator: string;
  country: string;          // ISO 3166 alpha-2 (empty if unknown)
  eyes: 'none' | 'five' | 'nine' | 'fourteen' | 'unknown';
  transport: 'DNSCrypt';
  port: number;
  addr: string;
  dnssec: boolean;          // operator-declared DNSSEC validation
  no_logs: boolean;         // operator-declared no-log policy
  no_filter: boolean;       // operator-declared no on-server filter
  filters: string[];        // ["malware", "adult", "ads", "crypto-mining"]
  privacy_score: number;    // 0-5
  trust_score: number;      // 0-5
  operator_tier: number;    // 1-3
  description: string;
}

export interface ResolverCriteria {
  no_logs?: boolean;
  dnssec?: boolean;
  no_filter?: boolean;
  outside_five_eyes?: boolean;
  outside_fourteen_eyes?: boolean;
  /// 0 = don't care; 1-5 = minimum required trust_score
  min_trust_score?: number;
  /// v55.1: auto-set by the supervisor when Tor is the active VPN.
  /// Limits the pool to port-443 resolvers — Tor exits universally
  /// permit 443 but commonly block 8443/5443/etc. Read-only from the
  /// UI's perspective (we don't accept user input here), but
  /// surfaced so the UI can render a "Tor-active filter is on" hint.
  tor_friendly_port?: boolean;
}

export interface DnscryptServersState {
  mode: 'specific' | 'auto';
  auto_criteria: ResolverCriteria;
  auto_picked: string[];          // resolver names handed to dnscrypt-proxy
  catalog: DnscryptCatalogEntry[];
}

export interface DnscryptState {
  ok: boolean;
  enabled: boolean;
  provider: string;
  location: string;
  custom_stamp: string;
  custom_label: string;
  providers: DnscryptProvider[];
  locations: DnscryptLocation[];
  servers: DnscryptServersState;  // v55+
  anonymized: AnonymizedState;
}

// The DNSCrypt status (enabled/provider/location) is tiny, but the full
// resolver + relay catalogs (~141 KB, build-time-static) are only needed
// by the config UI's picker. Pass { catalog: true } there; status pollers
// (e.g. the home page pills) omit it so they don't re-pull 141 KB.
export const getDnscrypt = (opts?: { catalog?: boolean }) =>
  req<DnscryptState>(
    'GET',
    `/network/dnscrypt${opts?.catalog ? '?catalog=true' : ''}`,
  );

export const setDnscrypt = (
  patch: {
    enabled?: boolean;
    provider?: string;
    location?: string;
    custom_stamp?: string;
    custom_label?: string;
    anonymized?: {
      enabled?: boolean;
      mode?: 'auto' | 'specific';
      criteria?: AnonymizedCriteria;
      specific_relays?: string[];
    };
    /// v55+: criteria-based auto-pick across full upstream catalog.
    servers?: {
      mode?: 'specific' | 'auto';
      auto_criteria?: ResolverCriteria;
    };
  },
) =>
  req<{ ok: boolean; enabled: boolean; provider: string; location: string }>(
    'PUT',
    '/network/dnscrypt',
    patch,
  );

// ── VPN (Tailscale / WireGuard / OpenVPN / commercial providers) ───────

export type VpnProvider =
  | 'none'
  | 'tailscale'
  | 'wireguard'
  | 'openvpn'
  // v59: commercial providers with their own setup wizard at
  // /network/vpn-providers — the radio on /network points users there
  // when one of these is selected but the provider hasn't been
  // configured yet.
  | 'mullvad'
  | 'ivpn'
  | 'azirevpn'
  | 'airvpn'
  | 'tor'
  | 'i2p';

/**
 * Provider IDs that need the dedicated setup wizard at
 * /network/vpn-providers. Used by the main /network page to render an
 * inline banner pointing users there.
 */
export const WIZARD_PROVIDERS: ReadonlySet<VpnProvider> =
  new Set(['mullvad', 'ivpn', 'airvpn']);

export interface VpnProviderInfo {
  id: VpnProvider;
  label: string;
  blurb: string;
}

export interface VpnState {
  ok: boolean;
  enabled: boolean;
  provider: VpnProvider;
  kill_switch: boolean;
  lan_bypass: string;
  tailscale: {
    // v80: tailscale is an independent top-level toggle now (like
    // tor/i2p), not a VPN provider. Split-tunnel only — exit-node
    // traffic rides the Pi's normal egress (no separate routing).
    // has_auth_key is presence-only — the backend never echoes the secret.
    enabled: boolean;
    hostname: string;
    exit_node: boolean;
    advertise_exit_node: boolean;
    has_auth_key: boolean;
  };
  wireguard: { has_config: boolean };
  openvpn: {
    has_config: boolean;
    auth_username: string;
    has_auth_password: boolean;
  };
  tor: {
    // v58.1: independent toggle + mode + over-VPN nesting.
    enabled: boolean;
    mode: 'split_tunnel' | 'transparent';
    over_vpn: boolean;
    preset: string;
    has_bridges: boolean;
    exit_country: string;
    meek_mode: boolean;
    presets: { id: string; label: string; blurb: string }[];
    modes: { id: string; label: string; blurb: string }[];
  };
  i2p: {
    enabled: boolean;
    outproxy: string;
    over_vpn: boolean;
  };
  providers: VpnProviderInfo[];
}

export interface VpnPatch {
  enabled?: boolean;
  provider?: VpnProvider;
  kill_switch?: boolean;
  lan_bypass?: string;
  tailscale?: {
    // v80: top-level independent toggle, split-tunnel mesh only.
    enabled?: boolean;
    auth_key?: string;
    hostname?: string;
    exit_node?: boolean;
    advertise_exit_node?: boolean;
  };
  wireguard?: { config?: string };
  openvpn?: {
    config?: string;
    auth_username?: string;
    auth_password?: string;
  };
  tor?: {
    // v58.1: independent toggle + routing mode + over-VPN nesting.
    enabled?: boolean;
    mode?: 'split_tunnel' | 'transparent';
    over_vpn?: boolean;
    preset?: string;
    bridges?: string;
    exit_country?: string;
    meek_mode?: boolean;
  };
  i2p?: {
    enabled?: boolean;
    outproxy?: string;
    over_vpn?: boolean;
  };
}

export const getVpn = () => req<VpnState>('GET', '/network/vpn');

export const setVpn = (patch: VpnPatch) =>
  req<{ ok: boolean; enabled: boolean; provider: string }>(
    'PUT',
    '/network/vpn',
    patch,
  );

// ── VPN live status (polled every few seconds while UI is visible) ─────

export type VpnStatusState =
  | 'establishing'
  | 'connected'
  | 'reconnecting'
  | 'failed'
  | 'disabled';

export interface VpnStatusOverlay {
  /** v61: which overlay layer this represents — clearnet VPN, Tor,
   *  I2P, or (v80) the Tailscale mesh. Each runs independently of the
   *  others so the UI shows one status panel per active overlay.
   *  aeon-vpn-status.py emits kind:"tailscale" whenever the mesh is on. */
  kind: 'vpn' | 'tor' | 'i2p' | 'tailscale';
  provider: string;
  enabled: boolean;
  state: VpnStatusState;
  bootstrap_percent: number | null;
  summary: string;
  public_ip: string | null;
  public_country: string | null;
  detail: Record<string, unknown> & {
    circuits?: { id: string; hops: string[] }[];
    peers?: { host?: string; ips?: string[]; online?: boolean; exit_node?: boolean }[];
    exit_country?: string;
    active_peers?: number;
    handshake_age_s?: number | null;
    magic_dns?: string;
  };
}

export interface VpnStatus {
  ok: boolean;
  /** v61: array of all currently-active overlay statuses. Optional
   *  to keep this interface compatible with older supervisor builds
   *  that only filled the legacy flat fields. */
  overlays?: VpnStatusOverlay[];
  // ── Legacy flat fields (pre-v61) — populated from the "primary"
  // overlay (clearnet VPN > Tor > I2P) for backward compat. New UI
  // code prefers iterating `overlays` so multi-overlay setups (e.g.
  // Mullvad + Tor split-tunnel) render both panels.
  provider: string;
  enabled: boolean;
  state: VpnStatusState;
  bootstrap_percent: number | null;
  summary: string;
  public_ip: string | null;
  public_country: string | null;
  detail: VpnStatusOverlay['detail'];
}

export const getVpnStatus = () => req<VpnStatus>('GET', '/network/vpn/status');

export const rotateVpnIdentity = () =>
  req<{ ok: boolean; provider: string; action: string; detail: string }>(
    'POST',
    '/network/vpn/rotate',
  );

// ── I2P status (v57+) ──────────────────────────────────────────────────

export interface I2pBinding {
  addr: string;
  port: number;
}

export interface I2pStatus {
  ok: boolean;
  installed: boolean;       // i2pd binary present on disk
  service_active: boolean;  // systemd unit running
  http_proxy: I2pBinding;   // browser HTTP proxy (typically :4444)
  socks_proxy: I2pBinding;  // SOCKS proxy (typically :4447)
  web_console: I2pBinding;  // i2pd router console (typically :7070)
  outproxy: string;         // configured clearnet exit, empty = I2P-only
  browser_hint: {
    http_proxy_url: string;
    console_url: string;
  };
}

export const getI2pStatus = () => req<I2pStatus>('GET', '/network/i2p/status');

// ── Mass storage (USB-CDROM library) ───────────────────────────────────

export interface IsoMeta {
  slug: string;
  display: string;
  size_bytes: number;
  sha256?: string | null;
  uploaded_at_ms: number;
}

export interface StorageState {
  ok: boolean;
  active: string;
  isos: IsoMeta[];
  free_bytes: number;
  iso_dir: string;
}

export const getStorage = () => req<StorageState>('GET', '/storage');

export const setActiveIso = (slug: string) =>
  req<{ ok: boolean; active: string; note: string }>(
    'PUT', '/storage/active', { slug });

export const deleteIso = (slug: string) =>
  req<{ ok: boolean }>('DELETE', `/storage/${encodeURIComponent(slug)}`);

/// Upload an ISO. The browser File goes into the request body directly;
/// the server streams it to disk so multi-GB files don't blow up RAM.
/// Returns the meta when the upload completes (or fails).
export async function uploadIso(
  file: File,
  onProgress?: (loaded: number, total: number) => void,
): Promise<IsoMeta> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/api/storage/upload');
    xhr.setRequestHeader('Content-Disposition', `attachment; filename="${file.name}"`);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.withCredentials = true;
    xhr.upload.addEventListener('progress', e => {
      if (e.lengthComputable && onProgress) onProgress(e.loaded, e.total);
    });
    xhr.addEventListener('load', () => {
      try {
        const data = JSON.parse(xhr.responseText);
        if (xhr.status >= 200 && xhr.status < 300 && data.ok) {
          resolve(data as IsoMeta);
        } else {
          reject(new Error(data.err || `HTTP ${xhr.status}`));
        }
      } catch (e: any) {
        reject(new Error(`bad response: ${e?.message ?? 'unknown'}`));
      }
    });
    xhr.addEventListener('error', () => reject(new Error('network error')));
    xhr.addEventListener('abort', () => reject(new Error('upload aborted')));
    xhr.send(file);
  });
}

// ── Firewall / NAT / Port-forward ──────────────────────────────────────

export interface FirewallRule {
  id: string;
  chain: string;                  // INPUT / OUTPUT / FORWARD / PREROUTING / POSTROUTING
  table: string;                  // filter / nat / mangle
  direction: 'inbound' | 'outbound' | 'forward';
  iface: string;                  // -i match (inbound), or -o on OUTPUT/POSTROUTING
  out_iface: string;              // -o match — FORWARD only (paired with iface as -i)
  proto: string;                  // tcp / udp / icmp / "" (any)
  src: string;                    // CIDR or empty
  dst: string;                    // CIDR or empty
  sport: string;                  // port range or empty
  dport: string;                  // port range or empty
  action: string;                 // ACCEPT / DROP / REJECT / DNAT / SNAT / REDIRECT / MASQUERADE
  comment: string;
  packets: number;                // hit counter (from iptables -nvL)
  bytes: number;
  redundant_with: number | null;  // 1-based rule number if redundant
}

export interface FirewallRuleDraft {
  chain: string;
  table: string;
  direction: 'inbound' | 'outbound' | 'forward';
  interface: string;
  out_iface: string;
  proto: string;
  src: string;
  dst: string;
  sport: string;
  dport: string;
  action: string;
  comment: string;
}

export const listFirewallRules = () =>
  req<{ ok: boolean; rules: FirewallRule[] }>('GET', '/firewall/rules');

export interface SystemFirewallRule {
  source: string;          // aeon-net-services / aeon-usb-net
  table: string;
  chain: string;
  target: string;          // ACCEPT / AEON_DROP / DNAT / REDIRECT / ...
  effect: string;          // human-readable target ("DROP (logged)" / "ACCEPT" / etc.)
  proto: string;
  iface: string;
  out_iface: string;
  src: string;
  dst: string;
  sport: string;
  dport: string;
  packets: number;
  bytes: number;
  match_options: string;   // free-form trailing match tokens (e.g. limit, comment)
}

export interface SystemFirewallDiagnostics {
  total_rules_per_table: Record<string, number>;
  aeon_tag_counts: Record<string, number>;
}

export const listSystemFirewallRules = () =>
  req<{
    ok: boolean;
    rules: SystemFirewallRule[];
    diagnostics?: SystemFirewallDiagnostics;
  }>('GET', '/firewall/system-rules');

export const addFirewallRule = (rule: FirewallRuleDraft) =>
  req<{ ok: boolean; id: string }>('POST', '/firewall/rules', rule);

export const deleteFirewallRule = (id: string) =>
  req<{ ok: boolean }>('DELETE', `/firewall/rules/${encodeURIComponent(id)}`);

/** Move a rule one slot up (-1) or down (+1) within its chain. */
export const moveFirewallRule = (id: string, delta: -1 | 1) =>
  req<{ ok: boolean }>(
    'POST',
    `/firewall/rules/${encodeURIComponent(id)}/move`,
    { delta },
  );

// ── DNS activity log + blacklist ───────────────────────────────────────

export interface DnsLogEntry {
  ts_ms: number;
  client: string;
  domain: string;
  qtype: string;        // A / AAAA / CNAME / etc.
  action: 'allow' | 'block';
  source: string;       // "dnsmasq" / "blacklist" / "regex"
}

export interface DnsLogState {
  ok: boolean;
  enabled: boolean;
  entries: DnsLogEntry[];
  blocked_total: number;
  allowed_total: number;
}

export const getDnsLog = () => req<DnsLogState>('GET', '/dns/log');
export const setDnsLogEnabled = (enabled: boolean) =>
  req<{ ok: boolean }>('PUT', '/dns/log', { enabled });

export interface DnsBlacklist {
  domains: string[];     // exact-match domains
  regexes: string[];     // regex patterns
}
export const getDnsBlacklist = () =>
  req<{ ok: boolean; blacklist: DnsBlacklist }>('GET', '/dns/blacklist');
export const setDnsBlacklist = (b: DnsBlacklist) =>
  req<{ ok: boolean }>('PUT', '/dns/blacklist', b);
export const uploadDnsBlacklistCsv = (csv: string) =>
  req<{ ok: boolean; added: number }>('POST', '/dns/blacklist/import', { csv });

// ── DNS blacklist subscription sources ─────────────────────────────────

export interface DnsSource {
  id: string;
  name: string;
  url: string;
  format: 'hosts' | 'domains' | 'adblock';
  refresh_hours: number;
  enabled: boolean;
  last_fetched_ms: number;
  last_attempt_ms: number;
  last_error: string;
  entry_count: number;
  sha256: string;
  stale: boolean;          // true when refresh due
}

export interface DnsSourcePreset {
  name: string;
  url: string;
  format: 'hosts' | 'domains' | 'adblock';
  blurb: string;
  category: 'general' | 'comprehensive' | 'lite' | 'security';
}

export const listDnsSources = () =>
  req<{ ok: boolean; sources: DnsSource[]; presets: DnsSourcePreset[] }>(
    'GET',
    '/dns/sources',
  );

export const addDnsSource = (
  src: { name: string; url: string; format: string; refresh_hours?: number },
) =>
  req<{ ok: boolean; id: string }>('POST', '/dns/sources', {
    refresh_hours: 24,
    ...src,
  });

export const updateDnsSource = (
  id: string,
  patch: { enabled?: boolean; refresh_hours?: number },
) => req<{ ok: boolean }>('PUT', `/dns/sources/${encodeURIComponent(id)}`, patch);

export const deleteDnsSource = (id: string) =>
  req<{ ok: boolean }>('DELETE', `/dns/sources/${encodeURIComponent(id)}`);

export const refreshDnsSource = (id: string) =>
  req<{ ok: boolean; entry_count: number; sha256: string }>(
    'POST',
    `/dns/sources/${encodeURIComponent(id)}/refresh`,
  );

// ── Audit log ─────────────────────────────────────────────────────────

export interface AuditEntry {
  ts_ms: number;
  actor: string;
  action: string;
  detail: string;
  result: 'ok' | 'fail' | string;
  err: string;
}

export interface AuditState {
  ok: boolean;
  entries: AuditEntry[];
  total_lines: number;
  max_bytes: number;
  current_bytes: number;
}

export const getAudit = (
  filters: { limit?: number; actor?: string; action?: string } = {},
) => {
  const q = new URLSearchParams();
  if (filters.limit != null) q.set('limit', String(filters.limit));
  if (filters.actor) q.set('actor', filters.actor);
  if (filters.action) q.set('action', filters.action);
  const qs = q.toString();
  return req<AuditState>('GET', `/audit${qs ? `?${qs}` : ''}`);
};

export const clearAudit = () =>
  req<{ ok: boolean }>('DELETE', '/audit');

// ── System (reboot / poweroff / health) ────────────────────────────────

export interface SystemInfo {
  ok: boolean;
  uptime_seconds: number;
  loadavg: { '1m': number; '5m': number; '15m': number };
  cpu_temp_c: number;
  cpu_count: number;
  mem_total_kb: number;
  mem_available_kb: number;
  image_version?: number | null; // flashed image version (null on a legacy image)
  track?: string; // pi4 | pi5 | other
  codename?: string | null; // bookworm | trixie
}

export const getSystemInfo = () => req<SystemInfo>('GET', '/system/info');

// ── Image-version notifier — is a newer Orb image published for this track? ──
export interface ImageUpdates {
  ok: boolean;
  track?: string;
  installed?: number | null;
  installed_known?: boolean;
  latest?: number | null;
  latest_name?: string | null;
  update_available?: boolean;
  published?: string | null;
  notes?: string | null;
  patreon_url?: string;
  err?: string;
}
export const imageUpdates = () => req<ImageUpdates>('GET', '/system/image-updates');

// ── OS package updates (apt) ──
export interface OsUpdateCheck {
  ok: boolean;
  count?: number;
  security?: number;
  packages?: string; // comma-joined package names (first 40)
  err?: string;
}
export interface OsUpdateStatus {
  phase: string; // idle | starting | refreshing | upgrading | cleaning | done | failed
  percent: number;
  done: boolean;
  ok: boolean;
  log: string;
  reboot_required: boolean;
}
export interface AutoUpdates {
  ok: boolean;
  enabled?: boolean;
  installed?: boolean;
  err?: string;
}
export const osUpdateCheck = () => req<OsUpdateCheck>('GET', '/system/update/check');
export const osUpdateApply = () =>
  req<{ ok: boolean; started: boolean; err?: string }>('POST', '/system/update', {});
export const osUpdateStatus = () => req<OsUpdateStatus>('GET', '/system/update/status');
export const autoUpdatesGet = () => req<AutoUpdates>('GET', '/system/auto-updates');
export const autoUpdatesSet = (enable: boolean) =>
  req<AutoUpdates>('POST', '/system/auto-updates', { enable });

// ── Pi-side controls (rarely needed — Pi is meant to stay up) ───────────
// Renamed in v53 from rebootSystem/poweroffSystem so callers don't
// accidentally use them when they mean the connected target. Pi
// power controls live in the System maintenance subsection now.
export const rebootPi = () =>
  req<{ ok: boolean; action: string; in_seconds: number; message: string }>(
    'POST',
    '/system/pi-reboot',
  );

export const poweroffPi = () =>
  req<{ ok: boolean; action: string; in_seconds: number; message: string }>(
    'POST',
    '/system/pi-poweroff',
  );

// ── Target (USB-connected machine) power controls (v53+) ────────────────
// These operate over the existing USB HID gadget (consumer power
// button) + Wake-on-LAN over usb0 ethernet — there's nothing the Pi
// itself can do that doesn't go through one of those channels.

export interface TargetMode {
  id: 'tap' | 'hold' | 'wake' | 'reboot';
  label: string;
  hint: string;
}
export interface TargetInfo {
  ok: boolean;
  mac_override: string;       // user-set MAC, empty if auto-discover
  mac_discovered: string | null;
  mac_effective: string | null;
  iface: string;              // typically "usb0"
  modes: TargetMode[];
}

export const getTargetInfo = () => req<TargetInfo>('GET', '/target/info');

export const setTargetConfig = (patch: { mac?: string; iface?: string }) =>
  req<{ ok: boolean; mac: string; iface: string }>(
    'PUT', '/target/config', patch);

export const targetPowerTap = () =>
  req<{ ok: boolean; mode: string; hold_ms: number }>(
    'POST', '/target/power-tap');

export const targetPowerHold = () =>
  req<{ ok: boolean; mode: string; hold_ms: number }>(
    'POST', '/target/power-hold');

export const targetWake = () =>
  req<{ ok: boolean; mac: string; iface: string }>(
    'POST', '/target/wake');

export const targetReboot = () =>
  req<{ ok: boolean; mode: string; mac: string; iface: string; phases: string[] }>(
    'POST', '/target/reboot');

// ── File transfer (HTTP server on usb0) ────────────────────────────────

export interface FileEntry {
  name: string;
  size_bytes: number;
  modified_ms: number;
}

export interface FileXferConfig {
  ok: boolean;
  enabled: boolean;
  port: number;
  allow_upload: boolean;
}

export const listFiles = () =>
  req<{ ok: boolean; files: FileEntry[]; dir: string }>('GET', '/files');

export const getFileXferConfig = () =>
  req<FileXferConfig>('GET', '/files/config');

export const setFileXferConfig = (
  patch: { enabled?: boolean; port?: number; allow_upload?: boolean },
) =>
  req<{ ok: boolean; needs_restart: boolean }>('PUT', '/files/config', patch);

export const deleteFile = (name: string) =>
  req<{ ok: boolean }>('DELETE', `/files/${encodeURIComponent(name)}`);

export function uploadFile(
  file: File,
  onProgress?: (loaded: number, total: number) => void,
): Promise<{ ok: boolean; name: string; size_bytes: number }> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/api/files/upload');
    xhr.setRequestHeader('Content-Disposition', `attachment; filename="${file.name}"`);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.withCredentials = true;
    xhr.upload.addEventListener('progress', e => {
      if (e.lengthComputable && onProgress) onProgress(e.loaded, e.total);
    });
    xhr.addEventListener('load', () => {
      try {
        const data = JSON.parse(xhr.responseText);
        if (xhr.status >= 200 && xhr.status < 300 && data.ok) resolve(data);
        else reject(new Error(data.err || `HTTP ${xhr.status}`));
      } catch (e: any) {
        reject(new Error(`bad response: ${e?.message ?? 'unknown'}`));
      }
    });
    xhr.addEventListener('error', () => reject(new Error('network error')));
    xhr.addEventListener('abort', () => reject(new Error('upload aborted')));
    xhr.send(file);
  });
}

// ── Shared clipboard ───────────────────────────────────────────────────

export interface ClipboardState {
  ok: boolean;
  text: string;
  size_bytes: number;
  max_bytes: number;
}

export const getClipboard = () =>
  req<ClipboardState>('GET', '/clipboard');
export const setClipboard = (text: string) =>
  req<{ ok: boolean; size_bytes: number; trimmed: boolean }>('PUT', '/clipboard', { text });
export const clearClipboard = () =>
  req<{ ok: boolean }>('DELETE', '/clipboard');
export const typeClipboardOnTarget = () =>
  req<{
    ok: boolean;
    // Character counts (not bytes). `typed` = chars actually sent; `skipped`
    // = chars without a HID scancode (emoji, smart quotes, em-dash, accented
    // letters); `input_chars` = total chars in the buffer. The `bytes_typed`
    // alias is kept for backwards compat with older clients but reports
    // the same value as `typed`.
    typed: number;
    skipped: number;
    input_chars: number;
    bytes_typed: number;
  }>('POST', '/clipboard/type-on-target');

// ── SSH key management ────────────────────────────────────────────────

export interface SshKey {
  id: string;
  comment: string;        // free-form trailing comment ("user@host")
  type: string;           // ssh-ed25519 / ssh-rsa / etc
  fingerprint: string;    // SHA256:…
  added_at_ms: number;
}

export const listSshKeys = () =>
  req<{ ok: boolean; keys: SshKey[] }>('GET', '/ssh/keys');
export const addSshKey = (key: string) =>
  req<{ ok: boolean; id: string }>('POST', '/ssh/keys', { key });
export const removeSshKey = (id: string) =>
  req<{ ok: boolean }>('DELETE', `/ssh/keys/${encodeURIComponent(id)}`);

// ── Security console metrics ──────────────────────────────────────────

export interface SecurityMetrics {
  ok: boolean;
  throughput_bps: { in: number; out: number };
  throughput_history: { ts_ms: number; in_bps: number; out_bps: number }[];
  blocked_24h: number;
  suspicious_events: {
    ts_ms: number;
    severity: 'info' | 'warn' | 'crit';
    label: string;
    detail: string;
  }[];
  top_blocked_domains: { domain: string; count: number }[];
  top_clients: { ip: string; bytes: number }[];
}

export const getSecurityMetrics = () =>
  req<SecurityMetrics>('GET', '/security/metrics');

export interface BlockedPacket {
  ts_ms: number;
  /// Identifies the rule that caused the block. Known tags:
  ///   "vpn-udp-forward"           VPN active: UDP from usb0 clients
  ///   "vpn-udp-output"            VPN active: local-host UDP
  ///   "vpn-killswitch"            Kill-switch: tunnel down
  ///   "usbnet-iso-rfc1918"        Isolation mode: RFC1918 destination
  ///   "usbnet-iso-pi-rfc1918"     Isolation mode: usb→Pi via private IPs
  ///   "usbnet-restricted-input"   Restricted mode: blocks Pi services
  ///   "fw-<id>"                   User firewall rule with that id
  cause_tag: string;
  in_iface: string;
  out_iface: string;
  proto: string;        // TCP / UDP / ICMP
  src: string;
  dst: string;
  sport: string;
  dport: string;
  length: number;
}

export const getBlockedPackets = (limit = 300) =>
  req<{ ok: boolean; entries: BlockedPacket[]; since: string }>(
    'GET',
    `/security/blocked?limit=${limit}`,
  );

// ── Hailo AI HAT+ (Hailo-10H) ──────────────────────────────────────────
// On-device NPU: install the runtime, watch live stats, deploy/unload
// models. All routes are ADMIN-gated like /api/orbnet/* (no allow-list
// bypass). The supervisor merges /usr/local/bin/aeon-hailo's JSON with its
// in-memory install/deploy TASKS + a curated model library. mem_* is a
// self-maintained ledger (usable budget ~5500 MB), NOT a kernel readout.

/** Live NPU telemetry. null when the device/tooling is absent. */
export interface HailoStats {
  nnc_util_pct: number;   // neural-network-core utilisation
  temp_c: number;
  power_w: number;
  cpu_util_pct?: number;
}

/** One model in the curated library / loaded set. */
export interface HailoModel {
  id: string;
  name: string;
  kind: 'llm' | 'vlm' | 'stt' | 'vision' | 'ocr';
  params?: string;        // e.g. "1B", "8B"
  quant?: string;         // e.g. "int4", "w4a16"
  size_mb: number;        // RAM footprint when loaded
  fits: boolean;          // size_mb <= free budget (greyed in UI if false)
  state: 'available' | 'downloading' | 'deployed' | 'loaded';
  license?: string;
  use_for?: string;       // short "good for OCR / chat / …" blurb
}

/** Whole-subsystem snapshot driving the three dashboard states. */
export interface HailoStatus {
  ok: boolean;
  enabled: boolean;
  device_present: boolean;
  hat: 'hailo-10h' | 'hailo-8' | 'other' | 'none';
  installed: boolean;
  // Self-maintained ledger (MB). Usable budget is ~5500 MB.
  mem_total_mb: number;
  mem_used_mb: number;
  mem_free_mb: number;
  stats: HailoStats | null;
  loaded: HailoModel[];
  consumers?: string[];   // e.g. ["aeon-vision (OCR)"]
}

export const getHailoStatus = () =>
  req<HailoStatus>('GET', '/hailo/status');

/** Kick off the non-blocking package install (apt + model-zoo debs). */
export const installHailo = () =>
  req<{ ok: boolean; started: boolean; err?: string }>('POST', '/hailo/install');

/** Poll the install progress bar — reuses the shared DeployStatus shape. */
export const getHailoInstallStatus = () =>
  req<DeployStatus>('GET', '/hailo/install/status');

/** The curated model library merged with live deployed/loaded state. */
export const getHailoModels = () =>
  req<{ ok: boolean; models: HailoModel[] }>('GET', '/hailo/models');

/** Non-blocking deploy (download + load) of a library model; poll status. */
export const deployHailoModel = (id: string) =>
  req<{ ok: boolean; started: boolean; err?: string }>(
    'POST',
    `/hailo/models/${encodeURIComponent(id)}/deploy`,
  );

/** Evict a loaded model to free NPU RAM. */
export const unloadHailoModel = (id: string) =>
  req<{ ok: boolean; err?: string }>(
    'POST',
    `/hailo/models/${encodeURIComponent(id)}/unload`,
  );

// ── BrainCraft HAT (AI interface + camera viewfinder + voice) ──────────
// The aeon-braincraft daemon drives the 240x240 TFT + buttons; the supervisor
// relays its /run/aeon/braincraft.json (`daemon`), owns braincraft.toml
// (`config`, re-read live by the daemon), pushes transient photo/record/say
// commands, and runs the button-driven voice-stack install (Piper + Vosk +
// display/audio libs, NOT baked). present:false ⇒ no HAT / daemon idle.

export type BraincraftMode = 'ai' | 'viewfinder' | 'voice';
export type BraincraftBackend = 'local' | 'hosted';

/** Persisted config (braincraft.toml). */
export interface BraincraftConfig {
  enabled: boolean;
  mode: BraincraftMode;
  overlay: boolean;
  audio: { device: string; tts: string; asr: string };
  backend: { mode: BraincraftBackend; persona: string };
  hosted: { llm_url: string; tts_url: string; asr_url: string };
}

/** Live device state from the daemon (/run/aeon/braincraft.json). */
export interface BraincraftDaemon {
  present: boolean;
  ok?: boolean;
  enabled?: boolean;
  /** When present=false, WHY: needs the install vs SPI off vs reseat the HAT. */
  reason?: 'disabled' | 'libs_missing' | 'no_spi';
  have_display_libs?: boolean;
  have_spi?: boolean;
  mode?: BraincraftMode;
  overlay?: boolean;
  display?: { driver: string; w: number; h: number; ok: boolean };
  buttons?: { chip: string; ok: boolean };
  audio?: { device: string; playback: boolean; capture: boolean; tts: string; asr: string; ok: boolean };
  backend?: { mode: string; persona: string | null; reachable: boolean };
  voice_installed?: boolean;
  last_utterance?: { role: string; text: string; ts_ms: number } | null;
  last_response?: { role: string; text: string; ts_ms: number } | null;
  captures?: { last_photo: string | null; recording: boolean };
  ts_ms?: number;
}

/** What aeon-voice reports as installed. */
export interface VoiceStackStatus {
  installed: boolean;
  sherpa: boolean;     // sherpa-onnx CPU runtime (serves both TTS + ASR)
  kokoro: boolean;     // Kokoro TTS model present
  whisper: boolean;    // Whisper ASR model present
  display_lib: boolean;
  audio: boolean;
  voice_model: string | null;
  asr_model: string | null;
}

export interface BraincraftStatus {
  ok: boolean;
  enabled: boolean;
  config: BraincraftConfig;
  daemon: BraincraftDaemon;
  voice: VoiceStackStatus;
  voice_installing: boolean;
}

export const getBraincraftStatus = () =>
  req<BraincraftStatus>('GET', '/braincraft/status');

/** Partial config update — only the present fields are merged into the toml. */
export interface BraincraftConfigPatch {
  enabled?: boolean;
  mode?: BraincraftMode;
  overlay?: boolean;
  audio_device?: string;
  tts?: string;
  asr?: string;
  backend_mode?: BraincraftBackend;
  persona?: string;
  llm_url?: string;
  tts_url?: string;
  asr_url?: string;
}
export const setBraincraftConfig = (patch: BraincraftConfigPatch) =>
  req<{ ok: boolean; config?: BraincraftConfig; err?: string }>('PUT', '/braincraft/config', patch);

/** Take a still in viewfinder mode (daemon saves the full-res frame). */
export const braincraftCapture = () =>
  req<{ ok: boolean; err?: string }>('POST', '/braincraft/capture');
/** Start/stop a viewfinder video recording. */
export const braincraftRecord = (action: 'start' | 'stop') =>
  req<{ ok: boolean; err?: string }>('POST', '/braincraft/record', { action });
/** Speak text through the active TTS path (local Piper / hosted persona voice). */
export const braincraftSay = (text: string) =>
  req<{ ok: boolean; err?: string }>('POST', '/braincraft/say', { text });

/** Kick off the non-blocking voice/display-stack install (Piper + Vosk + libs). */
export const installVoiceStack = () =>
  req<{ ok: boolean; started: boolean; err?: string }>('POST', '/braincraft/voice/install');
/** Poll the voice-install progress bar — reuses the shared DeployStatus shape. */
export const getVoiceInstallStatus = () =>
  req<DeployStatus>('GET', '/braincraft/voice/install/status');
