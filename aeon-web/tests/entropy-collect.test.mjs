// node --test aeon-web/tests/
import test from 'node:test';
import assert from 'node:assert/strict';
import { BITS_PER_ROLL, createScribble, diceWarning, personalEntropy, readDice, sha256Hex } from '../src/lib/entropy-collect.js';

const W = 480, H = 160;

test('dice: only 1–6 count, and each roll is worth log2(6) bits', () => {
  const d = readDice('3 5, 1\n6-2 4 7 0 x 9');
  assert.deepEqual(d.rolls, [3, 5, 1, 6, 2, 4]);
  assert.equal(d.ignored, 4); // 7, 0, x, 9
  assert.equal(d.bits, Math.floor(6 * BITS_PER_ROLL));
  assert.equal(readDice('').rolls.length, 0);
  // 50 real rolls reach 128 bits; 99 reach the key's full 256.
  assert.ok(readDice('1'.repeat(49) + '2').bits >= 128);
  assert.ok(Math.floor(99 * BITS_PER_ROLL) >= 255);
});

test('dice: input no real dice would produce is called out', () => {
  assert.match(diceWarning(Array(20).fill(4)), /one or two different numbers/);
  assert.match(diceWarning([...'121212121212121212'].map(Number)), /one or two different numbers/);
  assert.match(diceWarning([...'123456123456123456'].map(Number)), /repeats the same short sequence/);
  assert.match(diceWarning([...'135135135135135'].map(Number)), /repeats the same short sequence/);
  assert.match(diceWarning([...'6666666616662666366646665'].map(Number)), /far more often/);
  // An ordinary run of real rolls raises nothing, and short input is not judged.
  assert.equal(diceWarning([...'3516242653114625341265'].map(Number)), '');
  assert.equal(diceWarning([1, 1, 1]), '');
});

test('scribble: going round the same loop stops counting', () => {
  const s = createScribble();
  let t = 0;
  const loop = () => { for (let a = 0; a < Math.PI * 2; a += 0.12) s.add(240 + 60 * Math.cos(a), 80 + 40 * Math.sin(a), (t += 8), W, H); };
  loop();
  const afterOne = s.varied;
  for (let i = 0; i < 25; i++) loop();
  // Twenty-five more identical laps add almost nothing…
  assert.ok(s.varied <= afterOne + 6, `${afterOne} -> ${s.varied}`);
  assert.ok(s.progress < 1);
  // …and the pad says so.
  assert.equal(s.repetitive, true);
  // Every sample is still recorded for hashing.
  assert.equal(s.count, 26 * Math.ceil((Math.PI * 2) / 0.12));
});

test('scribble: varied direction and size fills the meter', () => {
  const s = createScribble();
  // A deterministic but wide-ranging path: changing radius, centre and step.
  let t = 0, seed = 7;
  const rnd = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
  let x = 240, y = 80;
  for (let i = 0; i < 4000 && s.progress < 1; i++) {
    const step = 5 + rnd() * 55, ang = rnd() * Math.PI * 2;
    x = Math.min(W - 1, Math.max(0, x + step * Math.cos(ang)));
    y = Math.min(H - 1, Math.max(0, y + step * Math.sin(ang)));
    s.add(x, y, (t += 6 + rnd() * 9), W, H);
  }
  assert.equal(s.progress, 1);
  assert.equal(s.repetitive, false);
});

test('scribble: tiny jitters on one spot do not count as movement', () => {
  const s = createScribble();
  for (let i = 0; i < 500; i++) s.add(100 + (i % 2), 60 + ((i >> 1) % 2), i * 5, W, H);
  assert.equal(s.varied, 0);
  assert.equal(s.count, 500);
});

test('what is sent to the Orb', async () => {
  assert.equal(personalEntropy('', []), '');
  assert.equal(personalEntropy('ab12', []), 'scribble:ab12');
  assert.equal(personalEntropy('', [3, 5, 1]), 'dice:351');
  assert.equal(personalEntropy('ab12', [3, 5, 1]), 'scribble:ab12;dice:351');
  assert.equal(personalEntropy('', Array(900).fill(2)).length, 'dice:'.length + 600); // capped
  // Different scribbles hash differently; the same one hashes the same.
  const a = createScribble(), b = createScribble();
  a.add(1, 2, 3, W, H); a.add(50, 60, 9, W, H);
  b.add(1, 2, 3, W, H); b.add(50, 61, 9, W, H);
  const [ha, hb, ha2] = await Promise.all([sha256Hex(a.bytes()), sha256Hex(b.bytes()), sha256Hex(a.bytes())]);
  assert.match(ha, /^[0-9a-f]{64}$/);
  assert.notEqual(ha, hb);
  assert.equal(ha, ha2);
});
