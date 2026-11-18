// node --test aeon-web/tests/
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  chainSummary, checkSummary, hashSource, headLabel, historyLine, isWeightFile, providerName, repoLabel, revisionLabel,
  shortRevision, splitFiles, whoRecorded, witnessSummary,
} from '../src/lib/upstream-view.js';

const fmt = (ms) => `day-${ms}`;
const record = {
  provider: 'huggingface', repo: 'org/model', revision: '0123456789abcdef0123456789abcdef01234567',
  files: [
    { path: 'config.json', size: 12, sha256: 'cc'.repeat(32), hash_from: 'computed' },
    { path: 'model.safetensors', size: 1024, sha256: 'aa'.repeat(32), hash_from: 'provider' },
  ],
};

test('a hash the provider did not publish is never called the provider\'s', () => {
  const published = hashSource(record.files[1], 'huggingface', true);
  assert.equal(published.published, true);
  assert.equal(published.label, 'published by HuggingFace');

  const mine = hashSource(record.files[0], 'huggingface', true);
  assert.equal(mine.published, false);
  assert.equal(mine.label, 'computed on this Orb');
  assert.doesNotMatch(mine.label, /published/);
  assert.match(mine.title, /publishes no SHA-256/);

  // On a mirror the computing was done by someone else, and it says so.
  const theirs = hashSource(record.files[0], 'huggingface', false);
  assert.equal(theirs.label, 'computed by the importing Orb');
  assert.match(theirs.title, /not this Orb's/);
});

test('what this Orb did itself is kept apart from what a publisher states', () => {
  // Imported here: one plain sentence in the first person.
  const own = whoRecorded({ local: true, record, recorded_here_ms: 5 }, fmt);
  assert.deepEqual(own.map((l) => l.text), ['This Orb read these from HuggingFace itself on day-5.']);

  // A mirror that never asked: the record is someone's statement.
  const mirror = whoRecorded({ local: true, record, recorded_here_ms: 0, confirmed_here_ms: 0 }, fmt);
  assert.match(mirror[0].text, /did not import this model from HuggingFace/);
  assert.match(mirror[0].text, /publisher's statement/);
  assert.match(mirror[1].text, /has not asked HuggingFace itself yet/);
  assert.ok(mirror.every((l) => !/This Orb read these/.test(l.text)));

  // A mirror that asked and got the same answer may say that much, and no more.
  const asked = whoRecorded({ local: true, record, recorded_here_ms: 0, confirmed_here_ms: 9 }, fmt);
  assert.equal(asked[1].text, 'This Orb asked HuggingFace itself on day-9 and found the same hashes.');
  // Not downloaded: nothing to ask about yet.
  assert.match(whoRecorded({ local: false, record }, fmt)[1].text, /Once it is downloaded here/);
});

test('another publisher\'s date is shown as their claim', () => {
  const none = witnessSummary({ identities: 0, others: 0, mine: false, earliest_ms: 0, earliest_other_ms: 0, differing: 0 }, fmt);
  assert.match(none.headline, /Nobody has signed/);
  assert.equal(witnessSummary(null, fmt).headline, none.headline);

  const alone = witnessSummary({ identities: 1, others: 0, mine: true, earliest_ms: 3, earliest_other_ms: 0, differing: 0 }, fmt);
  assert.equal(alone.headline, 'No other publisher has recorded these hashes yet');
  assert.doesNotMatch(alone.note, /day-/);

  const one = witnessSummary({ identities: 2, others: 1, mine: true, earliest_ms: 2, earliest_other_ms: 2, differing: 0 }, fmt);
  assert.equal(one.headline, '1 other publisher recorded the same hashes, besides this Orb');
  assert.match(one.note, /says day-2/);
  assert.match(one.note, /their claim/);
  assert.match(one.note, /proves who signed, not when/);
  assert.equal(one.warning, '');

  // The count is of OTHER publishers, and this Orb is not said to be among
  // them when it has signed nothing.
  const many = witnessSummary({ identities: 3, others: 3, mine: false, earliest_ms: 1, earliest_other_ms: 1, differing: 2 }, fmt);
  assert.equal(many.headline, '3 other publishers recorded the same hashes');
  assert.match(many.warning, /2 publishers signed a different hash for this same revision/);
});

test('a re-check says which scope moved, and that no answer proves nothing', () => {
  assert.deepEqual(checkSummary(null, 'huggingface'), { tone: 'muted', headline: 'Not re-checked yet', lines: [] });

  const ok = checkSummary({ at_ms: 1, event: 'confirmed', pinned: 'confirmed', head: 'confirmed' }, 'huggingface');
  assert.equal(ok.tone, 'ok');
  assert.equal(ok.headline, 'HuggingFace still publishes the same hashes');
  assert.deepEqual(ok.lines, ['pinned commit: same hashes', 'default branch: same hashes']);

  const moved = checkSummary({ at_ms: 1, event: 'changed', pinned: 'confirmed', head: 'changed' }, 'huggingface');
  assert.equal(moved.tone, 'bad');
  assert.deepEqual(moved.lines.slice(0, 2), ['pinned commit: same hashes', 'default branch: different hashes']);
  assert.match(moved.lines[2], /The copy on this network has not changed/);

  const gone = checkSummary({ at_ms: 1, event: 'removed', pinned: 'removed', head: 'removed' }, 'civitai');
  assert.equal(gone.tone, 'warn');
  assert.equal(gone.headline, 'Civitai no longer offers all of this');
  assert.equal(gone.lines[0], 'pinned version: gone or no longer public');
  assert.equal(gone.lines[1], "the model's version list: gone or no longer public");

  const silent = checkSummary({ at_ms: 1, event: 'unreachable', pinned: 'unreachable', head: 'unreachable' }, 'ollama');
  assert.equal(silent.tone, 'muted'); // not a warning
  assert.equal(silent.headline, 'The Ollama registry could not be reached');
  assert.match(silent.lines.at(-1), /not evidence of anything/);
  assert.equal(silent.lines[1], 'where the tag points now: no answer');
});

test('a broken history is reported as found, not as repaired', () => {
  assert.equal(chainSummary(null).tone, 'muted');
  assert.equal(chainSummary({ ok: true, lines: 0, broken: [] }).text, 'This Orb has no history yet.');
  assert.match(chainSummary({ ok: true, lines: 1, broken: [] }).text, /intact: 1 line,/);
  const bad = chainSummary({ ok: false, lines: 9, broken: [4, 5] });
  assert.equal(bad.tone, 'bad');
  assert.match(bad.text, /altered or damaged at lines 4, 5/);
  assert.match(bad.text, /shown as found and has not been repaired/);
  assert.match(chainSummary({ ok: false, lines: 9, broken: [4] }).text, /at line 4\./);
});

test('history lines name who asked and what came back', () => {
  const rec = historyLine({ at_ms: 1, event: 'recorded', by: 'import' }, 'huggingface');
  assert.deepEqual([rec.label, rec.tone], ['recorded', 'ok']);
  assert.match(rec.text, /this Orb read the hashes from HuggingFace/);
  const timer = historyLine({ at_ms: 2, event: 'unreachable', pinned: 'unreachable', head: 'unreachable', by: 'timer', detail: 'timed out' }, 'huggingface');
  assert.deepEqual([timer.label, timer.tone, timer.text], ['unreachable', 'muted', 'weekly check · timed out']);
  assert.equal(historyLine({ at_ms: 3, event: 'changed', by: 'operator' }, 'huggingface').text, 'checked by you');
});

test('names, revisions and the split of weights from the rest', () => {
  assert.equal(providerName('huggingface'), 'HuggingFace');
  assert.equal(providerName('somewhere-new'), 'somewhere-new');
  assert.equal(providerName(''), 'the provider');
  assert.deepEqual(['huggingface', 'ollama', 'civitai', 'x'].map(revisionLabel), ['commit', 'manifest', 'version', 'revision']);
  assert.equal(headLabel('huggingface'), 'default branch');
  assert.equal(repoLabel(record), 'org/model');
  assert.equal(repoLabel({ provider: 'civitai', repo: '4384' }), 'model 4384');
  assert.equal(repoLabel(null), '');
  assert.equal(shortRevision(record.revision), '0123456789ab…');
  assert.equal(shortRevision(`sha256:${'ab'.repeat(32)}`), 'abababababab…');
  assert.equal(shortRevision('128713'), '128713');
  assert.ok(isWeightFile('unet/Model.SafeTensors') && isWeightFile('a.gguf') && !isWeightFile('tokenizer.json'));
  const split = splitFiles(record);
  assert.deepEqual([split.weights.map((f) => f.path), split.others.map((f) => f.path)], [['model.safetensors'], ['config.json']]);
  // Nothing that looks like weights: show everything rather than nothing.
  assert.equal(splitFiles({ ...record, files: [record.files[0]] }).weights.length, 1);
  assert.deepEqual(splitFiles(null), { weights: [], others: [] });
});
