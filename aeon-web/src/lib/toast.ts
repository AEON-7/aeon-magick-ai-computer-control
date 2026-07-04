// In-app toast notifications — the replacement for native alert().
// Usage: toast('saved'), toast.error('deploy failed: …'), toast.success('token minted').
// Rendered by <Toasts/> in the root layout, so any page/component can call these.
import { writable } from 'svelte/store';

export type ToastKind = 'info' | 'success' | 'error';

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  timeout: number;
}

export const toasts = writable<Toast[]>([]);

let nextId = 1;
const MAX_VISIBLE = 5;

function push(message: string, kind: ToastKind, timeout?: number): void {
  const t: Toast = {
    id: nextId++,
    kind,
    message,
    // Errors linger longer — they carry the "what went wrong" detail
    // the old alert() forced you to acknowledge.
    timeout: timeout ?? (kind === 'error' ? 7000 : 4000),
  };
  toasts.update((list) => [...list, t].slice(-MAX_VISIBLE));
  setTimeout(() => dismiss(t.id), t.timeout);
}

export function dismiss(id: number): void {
  toasts.update((list) => list.filter((t) => t.id !== id));
}

export function toast(message: string, kind: ToastKind = 'info', timeout?: number): void {
  push(message, kind, timeout);
}
toast.info = (message: string, timeout?: number) => push(message, 'info', timeout);
toast.success = (message: string, timeout?: number) => push(message, 'success', timeout);
toast.error = (message: string, timeout?: number) => push(message, 'error', timeout);
