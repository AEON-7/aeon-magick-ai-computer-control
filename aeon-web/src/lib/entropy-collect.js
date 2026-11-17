// @ts-check
// Collecting a person's own randomness for a publisher identity.
//
// The Orb never relies on this: whatever is gathered here is hashed together
// with the system generator (and the hardware chip, where there is one), so it
// can only add unpredictability. What this module is for is making sure the
// person's contribution is REAL — measuring whether a scribble actually varies,
// and noticing dice input that no pair of dice would produce.

/** What one roll of a fair six-sided die is worth: log2(6) ≈ 2.585 bits. */
export const BITS_PER_ROLL = Math.log2(6);

/** Most rolls sent to the Orb (600 rolls ≈ 1,550 bits — far past the key's 256). */
const MAX_ROLLS = 600;

/**
 * Read typed dice results. Only the digits 1–6 count. Spaces, commas and line
 * breaks are separators; anything else is counted as ignored so the person can
 * see their typing was not all used.
 * @param {string} text
 * @returns {{ rolls: number[], ignored: number, bits: number, warning: string }}
 */
export function readDice(text) {
  /** @type {number[]} */
  const rolls = [];
  let ignored = 0;
  for (const ch of String(text ?? '')) {
    if (ch >= '1' && ch <= '6') rolls.push(ch.charCodeAt(0) - 48);
    else if (!/[\s,;.\-]/.test(ch)) ignored++;
  }
  return { rolls, ignored, bits: Math.floor(rolls.length * BITS_PER_ROLL), warning: diceWarning(rolls) };
}

/**
 * Say so when the rolls are not what real dice produce. This cannot prove dice
 * were used — it only catches the obvious: a key held down, a made-up pattern.
 * @param {number[]} rolls
 * @returns {string} empty when nothing looks wrong
 */
export function diceWarning(rolls) {
  if (rolls.length < 12) return '';
  const counts = [0, 0, 0, 0, 0, 0, 0];
  for (const r of rolls) counts[r]++;
  const distinct = counts.filter((c) => c > 0).length;
  if (distinct <= 2) return 'Only one or two different numbers appear. Real dice show all six — type each roll as it lands.';
  for (let period = 1; period <= 6; period++) {
    if (rolls.every((r, i) => r === rolls[i % period])) {
      return 'This repeats the same short sequence over and over. Type the dice as they actually land.';
    }
  }
  if (Math.max(...counts) > rolls.length * 0.6) {
    return 'One number comes up far more often than chance allows. Check the die, or your typing.';
  }
  return '';
}

/**
 * Measures how much VARIED movement a scribble contains.
 *
 * Every sample is kept and hashed, varied or not. Progress, though, only
 * advances for movement that is new: a step is described by where it is on the
 * pad, which way it heads and how long it is, and what counts is each
 * step-to-step change not seen before. Going round the same loop produces the
 * same changes again and adds nothing — which is exactly the motion a person
 * falls into, and exactly what must not be mistaken for randomness.
 *
 * @param {{ cols?: number, rows?: number, target?: number }} [opts]
 */
export function createScribble(opts = {}) {
  const cols = opts.cols ?? 12;
  const rows = opts.rows ?? 8;
  const target = opts.target ?? 160;
  /** @type {number[]} */
  const samples = [];
  /** @type {Set<string>} */
  const seen = new Set();
  /** @type {number[]} */
  const recent = [];
  let lastKey = '';
  let lastX = 0;
  let lastY = 0;
  let started = false;

  return {
    /**
     * Record one pointer position.
     * @param {number} x @param {number} y pixels within the pad
     * @param {number} t a high-resolution timestamp (performance.now())
     * @param {number} w @param {number} h the pad's size in pixels
     */
    add(x, y, t, w, h) {
      if (samples.length < 60000) samples.push(x, y, t);
      if (!started) {
        started = true;
        lastX = x;
        lastY = y;
        return;
      }
      const dx = x - lastX;
      const dy = y - lastY;
      const dist = Math.hypot(dx, dy);
      if (dist < 4) return; // too small a step to have a direction
      const dir = Math.round(Math.atan2(dy, dx) / (Math.PI / 4)) & 7; // 8 compass directions
      const len = dist < 14 ? 0 : dist < 40 ? 1 : 2; // short / medium / long: size and speed
      const cx = Math.min(cols - 1, Math.max(0, Math.floor((x / Math.max(1, w)) * cols)));
      const cy = Math.min(rows - 1, Math.max(0, Math.floor((y / Math.max(1, h)) * rows)));
      const key = `${cx + cols * cy}:${dir}:${len}`;
      const change = `${lastKey}>${key}`;
      const fresh = !seen.has(change);
      if (fresh) seen.add(change);
      recent.push(fresh ? 1 : 0);
      if (recent.length > 50) recent.shift();
      lastKey = key;
      lastX = x;
      lastY = y;
    },
    /** Number of positions recorded. */
    get count() {
      return samples.length / 3;
    },
    /** Distinct movement changes so far. */
    get varied() {
      return seen.size;
    },
    /** 0..1 — how close the scribble is to "plenty". */
    get progress() {
      return Math.min(1, seen.size / target);
    },
    /** True while the latest movement is just more of the same. */
    get repetitive() {
      return seen.size < target && recent.length >= 50 && recent.reduce((a, b) => a + b, 0) <= 4;
    },
    /** Every sample (positions and timings), for hashing. */
    bytes() {
      return new Uint8Array(new Float64Array(samples).buffer);
    },
  };
}

/**
 * SHA-256 of some bytes, as hex. Uses the browser's own implementation.
 * @param {Uint8Array} bytes
 * @returns {Promise<string>} '' if the browser offers no WebCrypto here
 */
export async function sha256Hex(bytes) {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) return '';
  const digest = await subtle.digest('SHA-256', /** @type {BufferSource} */ (bytes));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/**
 * The text handed to the Orb to be mixed into the key.
 * @param {string} scribbleHex digest of the scribble, or ''
 * @param {number[]} rolls
 * @returns {string}
 */
export function personalEntropy(scribbleHex, rolls) {
  const parts = [];
  if (scribbleHex) parts.push(`scribble:${scribbleHex}`);
  if (rolls.length) parts.push(`dice:${rolls.slice(0, MAX_ROLLS).join('')}`);
  return parts.join(';');
}
