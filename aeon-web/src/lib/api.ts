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

export const snapshotURL = () => `${API}/streamer/snapshot?t=${Date.now()}`;
export const streamURL = () => `${API}/streamer/stream`;

// ── HID ────────────────────────────────────────────────────────────────

export const getHidStatus = () => req<HidStatus>('GET', '/hid/status');

export const typeText = (text: string) =>
  req<{ ok: boolean }>('POST', '/hid/type', { text });

export const sendKey = (keys: string[], hold_ms = 50) =>
  req<{ ok: boolean }>('POST', '/hid/key', { keys, hold_ms });

export const click = (button: 'left' | 'right' | 'middle' = 'left', count = 1) =>
  req<{ ok: boolean }>('POST', '/hid/click', { button, count });

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
}

export interface DnscryptLocation {
  id: string;
  label: string;
}

export interface DnscryptState {
  ok: boolean;
  enabled: boolean;
  provider: string;
  location: string;
  providers: DnscryptProvider[];
  locations: DnscryptLocation[];
}

export const getDnscrypt = () => req<DnscryptState>('GET', '/network/dnscrypt');

export const setDnscrypt = (
  patch: { enabled?: boolean; provider?: string; location?: string },
) =>
  req<{ ok: boolean; enabled: boolean; provider: string; location: string }>(
    'PUT',
    '/network/dnscrypt',
    patch,
  );

// ── VPN (Tailscale / WireGuard / OpenVPN) ──────────────────────────────

export type VpnProvider =
  | 'none'
  | 'tailscale'
  | 'wireguard'
  | 'openvpn'
  | 'tor'
  | 'i2p';

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
  tor: { has_bridges: boolean };
  i2p: { outproxy: string };
  providers: VpnProviderInfo[];
}

export interface VpnPatch {
  enabled?: boolean;
  provider?: VpnProvider;
  kill_switch?: boolean;
  lan_bypass?: string;
  tailscale?: {
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
  tor?: { bridges?: string };
  i2p?: { outproxy?: string };
}

export const getVpn = () => req<VpnState>('GET', '/network/vpn');

export const setVpn = (patch: VpnPatch) =>
  req<{ ok: boolean; enabled: boolean; provider: string }>(
    'PUT',
    '/network/vpn',
    patch,
  );

// ── VPN live status (polled every few seconds while UI is visible) ─────

export interface VpnStatus {
  ok: boolean;
  provider: string;
  enabled: boolean;
  state: 'establishing' | 'connected' | 'reconnecting' | 'failed' | 'disabled';
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

export const getVpnStatus = () => req<VpnStatus>('GET', '/network/vpn/status');

export const rotateVpnIdentity = () =>
  req<{ ok: boolean; provider: string; action: string; detail: string }>(
    'POST',
    '/network/vpn/rotate',
  );
