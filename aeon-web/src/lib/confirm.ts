// In-app modal dialogs — the replacement for native confirm() and prompt().
// Promise-based so call sites stay drop-in:
//
//   if (!(await confirmRite({ title: 'Switch persona?', body: '…' }))) return;
//
//   // Destructive actions gate on a typed phrase (replaces the old
//   // confirm() + prompt('Type REBOOT') double-dialog):
//   if (!(await confirmRite({ title: 'Reboot target', body: '…', danger: true, phrase: 'REBOOT' }))) return;
//
//   const name = await askRite({ title: 'Rename recording', initial: old });
//   if (name === null) return; // cancelled
//
// Rendered by <ConfirmHost/> in the root layout.
import { writable } from 'svelte/store';

export interface ConfirmOptions {
  title: string;
  /** Supports \n for paragraphs. */
  body?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  /** Red styling for destructive actions. */
  danger?: boolean;
  /** Require typing this exact phrase before confirm unlocks. */
  phrase?: string;
}

export interface AskOptions {
  title: string;
  body?: string;
  label?: string;
  placeholder?: string;
  initial?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
}

export type ModalRequest =
  | { kind: 'confirm'; opts: ConfirmOptions; resolve: (ok: boolean) => void }
  | { kind: 'ask'; opts: AskOptions; resolve: (value: string | null) => void };

export const modal = writable<ModalRequest | null>(null);

// One dialog at a time; later requests wait for the current one.
const queue: ModalRequest[] = [];
let active = false;

function enqueue(req: ModalRequest): void {
  if (active) {
    queue.push(req);
    return;
  }
  active = true;
  modal.set(req);
}

/** Called by ConfirmHost after resolving the current request. */
export function settle(): void {
  const next = queue.shift();
  if (next) {
    modal.set(next);
  } else {
    active = false;
    modal.set(null);
  }
}

export function confirmRite(opts: ConfirmOptions): Promise<boolean> {
  return new Promise((resolve) => enqueue({ kind: 'confirm', opts, resolve }));
}

export function askRite(opts: AskOptions): Promise<string | null> {
  return new Promise((resolve) => enqueue({ kind: 'ask', opts, resolve }));
}
