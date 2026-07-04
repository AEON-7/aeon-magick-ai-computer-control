// True while the control page owns the keyboard (pointer-lock capture or
// the mobile soft-keyboard bridge). Global hotkeys — the Cmd+K palette in
// particular — must stand down so every keystroke reaches the target.
import { writable } from 'svelte/store';

export const inputCaptured = writable(false);
