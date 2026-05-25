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
}

export interface HidStatus {
  ok: boolean;
  persona: string;
}

export interface SupervisorState {
  ok: boolean;
  streamer: StreamerState | null;
  hid: HidStatus | null;
}

async function jsonReq<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const res = await fetch(API + path, {
    method,
    credentials: 'same-origin',
    headers: body !== undefined ? { 'Content-Type': 'application/json' } : {},
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`HTTP ${res.status}: ${text.slice(0, 200)}`);
  }
  return (await res.json()) as T;
}

// ── system ─────────────────────────────────────────────────────────────

export const getSystemState = () => jsonReq<SupervisorState>('GET', '/state');
export const login = (username: string, password: string) =>
  jsonReq<{ ok: boolean }>('POST', '/login', { username, password });
export const logout = () => jsonReq<{ ok: boolean }>('POST', '/logout');

// ── streamer ───────────────────────────────────────────────────────────

export const getStreamerState = () => jsonReq<StreamerState>('GET', '/streamer/state');
export const relaunchStreamer = () => jsonReq<{ ok: boolean }>('POST', '/streamer/relaunch');

export const snapshotURL = () => `${API}/streamer/snapshot?t=${Date.now()}`;
export const streamURL = () => `${API}/streamer/stream`;

// ── HID ────────────────────────────────────────────────────────────────

export const getHidStatus = () => jsonReq<HidStatus>('GET', '/hid/status');

export const typeText = (text: string) =>
  jsonReq<{ ok: boolean }>('POST', '/hid/type', { text });

export const sendKey = (keys: string[], hold_ms = 50) =>
  jsonReq<{ ok: boolean }>('POST', '/hid/key', { keys, hold_ms });

export const click = (button: 'left' | 'right' | 'middle' = 'left', count = 1) =>
  jsonReq<{ ok: boolean }>('POST', '/hid/click', { button, count });

export const moveMouse = (dx: number, dy: number) =>
  jsonReq<{ ok: boolean }>('POST', '/hid/move', { dx, dy });

export const scroll = (dy: number) =>
  jsonReq<{ ok: boolean }>('POST', '/hid/scroll', { dy });

export const releaseAll = () => jsonReq<{ ok: boolean }>('POST', '/hid/release_all');
