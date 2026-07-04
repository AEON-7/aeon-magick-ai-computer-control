import { writable } from 'svelte/store';

/// True when the UI is talking to a headless "Orb server" container (no HID,
/// video, GPIO, or host-network appliance). Set once from /api/auth/me in the
/// root layout; pages read it to hide Pi-only surface.
export const serverMode = writable(false);
