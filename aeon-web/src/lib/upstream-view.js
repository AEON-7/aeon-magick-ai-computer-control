// @ts-check
// Wording for the "Original weights" block on a model's detail view.
//
// The block mixes three kinds of statement, and they must never read alike:
//   1. what the provider published (a hash it gave, or one it did not give and
//      an Orb had to compute);
//   2. what THIS Orb checked itself, taken from its own history;
//   3. what other publishers signed. A signature proves who signed, not when,
//      so another publisher's date is always shown as their claim.
// Keeping the sentences here, as plain functions, is what lets them be tested.

/**
 * @typedef {{ path: string, size: number, sha256: string, hash_from: 'provider' | 'computed' }} UpstreamFile
 * @typedef {{ provider: string, repo: string, revision: string, files: UpstreamFile[] }} UpstreamRecord
 * @typedef {{ path: string, scope: string, what: string, was: string, now?: string }} Diff
 * @typedef {{ line?: number, at_ms: number, event: string, pinned?: string, head?: string, head_revision?: string, detail?: string, diff?: Diff[], by?: string, observed_ms?: number }} CheckEvent
 * @typedef {{ identities: number, others: number, mine: boolean, earliest_ms: number, earliest_other_ms: number, differing: number, list?: { key: string, fp: string, name: string, observed_ms: number, mine: boolean }[] }} Witnessed
 * @typedef {{ ok: boolean, lines: number, broken: number[] }} ChainState
 * @typedef {{ local: boolean, record: UpstreamRecord | null, recorded_here_ms?: number, confirmed_here_ms?: number }} UpstreamState
 * @typedef {'ok' | 'warn' | 'bad' | 'muted'} Tone
 */

const PROVIDERS = { huggingface: 'HuggingFace', ollama: 'the Ollama registry', civitai: 'Civitai' };

/** @param {string} provider */
export function providerName(provider) {
  return PROVIDERS[/** @type {keyof typeof PROVIDERS} */ (provider)] ?? (provider || 'the provider');
}

/**
 * The repository as a person would name it. Civitai's is a bare model number.
 * @param {{ provider: string, repo: string } | null | undefined} record
 */
export function repoLabel(record) {
  if (!record) return '';
  return record.provider === 'civitai' ? `model ${record.repo}` : record.repo;
}

/** What kind of thing the pinned revision is, per provider. @param {string} provider */
export function revisionLabel(provider) {
  return provider === 'huggingface' ? 'commit' : provider === 'ollama' ? 'manifest' : provider === 'civitai' ? 'version' : 'revision';
}

/** A revision short enough for one line; the full value belongs in a tooltip. @param {string} revision */
export function shortRevision(revision) {
  const r = String(revision ?? '');
  const hex = r.replace(/^sha256:/, '');
  return /^[0-9a-f]{20,}$/.test(hex) ? `${hex.slice(0, 12)}…` : r;
}

const WEIGHT_EXT = ['.safetensors', '.gguf', '.bin', '.onnx', '.pt', '.pth', '.ot', '.hef'];

/** @param {string} path */
export function isWeightFile(path) {
  const p = String(path ?? '').toLowerCase();
  return WEIGHT_EXT.some((e) => p.endsWith(e));
}

/**
 * Weight files first; tokenizer and config files after. A record with nothing
 * that looks like weights shows everything as weights rather than hiding it.
 * @param {UpstreamRecord | null | undefined} record
 */
export function splitFiles(record) {
  const files = record?.files ?? [];
  const weights = files.filter((f) => isWeightFile(f.path));
  return weights.length ? { weights, others: files.filter((f) => !isWeightFile(f.path)) } : { weights: files, others: [] };
}

/**
 * Where one file's SHA-256 came from. `recordedHere` is true only when this
 * Orb imported the model itself; otherwise a computed hash was computed by
 * someone else and must not be called this Orb's.
 * @param {UpstreamFile} file @param {string} provider @param {boolean} recordedHere
 * @returns {{ label: string, title: string, published: boolean }}
 */
export function hashSource(file, provider, recordedHere) {
  const p = providerName(provider);
  if (file.hash_from === 'provider') {
    return {
      published: true,
      label: `published by ${p}`,
      title: `${p} itself published this SHA-256 for the file, and the bytes that were downloaded matched it.`,
    };
  }
  return recordedHere
    ? {
        published: false,
        label: 'computed on this Orb',
        title: `${p} publishes no SHA-256 for this file, so this Orb hashed the bytes it downloaded. It is this Orb's own measurement, not ${p}'s statement.`,
      }
    : {
        published: false,
        label: 'computed by the importing Orb',
        title: `${p} publishes no SHA-256 for this file. The Orb that imported it hashed what it received: its measurement, not ${p}'s statement and not this Orb's.`,
      };
}

/**
 * Who read the provider, in this Orb's own words about itself.
 * @param {UpstreamState} u @param {(ms: number) => string} fmt
 * @returns {{ tone: Tone, text: string }[]}
 */
export function whoRecorded(u, fmt) {
  const p = providerName(u.record?.provider ?? '');
  if (u.recorded_here_ms) {
    return [{ tone: 'ok', text: `This Orb read these from ${p} itself on ${fmt(u.recorded_here_ms)}.` }];
  }
  /** @type {{ tone: Tone, text: string }[]} */
  const lines = [{ tone: 'muted', text: `This Orb did not import this model from ${p}. The record is its publisher's statement.` }];
  if (u.confirmed_here_ms) {
    lines.push({ tone: 'ok', text: `This Orb asked ${p} itself on ${fmt(u.confirmed_here_ms)} and found the same hashes.` });
  } else if (u.local) {
    lines.push({ tone: 'muted', text: `This Orb has not asked ${p} itself yet. “Check again” does.` });
  } else {
    lines.push({ tone: 'muted', text: `Once it is downloaded here, this Orb can ask ${p} itself.` });
  }
  return lines;
}

/** @param {string | undefined} verdict */
function scopeText(verdict) {
  switch (verdict) {
    case 'confirmed': return 'same hashes';
    case 'changed': return 'different hashes';
    case 'removed': return 'gone or no longer public';
    case 'unreachable': return 'no answer';
    default: return 'not checked';
  }
}

/**
 * The last re-check, as a headline and the two scopes behind it.
 * @param {CheckEvent | null | undefined} check @param {string} provider
 * @returns {{ tone: Tone, headline: string, lines: string[] }}
 */
export function checkSummary(check, provider) {
  const p = providerName(provider);
  if (!check) return { tone: 'muted', headline: 'Not re-checked yet', lines: [] };
  const lines = [`pinned ${revisionLabel(provider)}: ${scopeText(check.pinned)}`, `${headLabel(provider)}: ${scopeText(check.head)}`];
  const kept = 'The copy on this network has not changed: it is still exactly the files recorded here.';
  switch (check.event) {
    case 'confirmed':
      return { tone: 'ok', headline: `${capital(p)} still publishes the same hashes`, lines };
    case 'changed':
      return { tone: 'bad', headline: `${capital(p)} now publishes different hashes`, lines: [...lines, kept] };
    case 'removed':
      return { tone: 'warn', headline: `${capital(p)} no longer offers all of this`, lines: [...lines, kept] };
    case 'unreachable':
      return { tone: 'muted', headline: `${capital(p)} could not be reached`, lines: [...lines, 'No answer is not evidence of anything about the model.'] };
    default:
      return { tone: 'muted', headline: check.event, lines };
  }
}

/** What "the default branch" is called for each provider. @param {string} provider */
export function headLabel(provider) {
  return provider === 'ollama' ? 'where the tag points now' : provider === 'civitai' ? "the model's version list" : 'default branch';
}

/** @param {string} s */
function capital(s) {
  return s ? s[0].toUpperCase() + s.slice(1) : s;
}

/** @param {number} n @param {string} one */
function count(n, one) {
  return `${n} ${one}${n === 1 ? '' : 's'}`;
}

/**
 * What signed statements say. Other publishers' dates are their claims.
 * @param {Witnessed | null | undefined} w @param {(ms: number) => string} fmt
 * @returns {{ tone: Tone, headline: string, note: string, warning: string }}
 */
export function witnessSummary(w, fmt) {
  const warning = w && w.differing > 0
    ? `${count(w.differing, 'publisher')} signed a different hash for this same revision. One pinned revision cannot honestly have two: someone is wrong or lying.`
    : '';
  if (!w || w.identities === 0) {
    return { tone: 'muted', headline: 'Nobody has signed that they saw these hashes published', note: '', warning };
  }
  if (w.others === 0) {
    return { tone: 'muted', headline: 'No other publisher has recorded these hashes yet', note: 'Only this Orb has signed that it saw them published.', warning };
  }
  const also = w.mine ? ', besides this Orb' : '';
  return {
    tone: 'ok',
    headline: `${count(w.others, 'other publisher')} recorded the same hashes${also}`,
    note: `The earliest of them says ${fmt(w.earliest_other_ms)}. That date is their claim: a signature proves who signed, not when.`,
    warning,
  };
}

/** @param {ChainState | null | undefined} chain @returns {{ tone: Tone, text: string }} */
export function chainSummary(chain) {
  if (!chain || chain.lines === 0) return { tone: 'muted', text: 'This Orb has no history yet.' };
  if (chain.ok) {
    return { tone: 'ok', text: `This Orb's history is intact: ${count(chain.lines, 'line')}, each carrying the hash of the one before.` };
  }
  const where = chain.broken.length === 1 ? `line ${chain.broken[0]}` : `lines ${chain.broken.join(', ')}`;
  return { tone: 'bad', text: `This Orb's history file was altered or damaged at ${where}. It is shown as found and has not been repaired.` };
}

/**
 * One line of the history list.
 * @param {CheckEvent} e @param {string} provider
 * @returns {{ tone: Tone, label: string, text: string }}
 */
export function historyLine(e, provider) {
  const p = providerName(provider);
  const who = e.by === 'timer' ? 'weekly check' : e.by === 'operator' ? 'checked by you' : e.by === 'import' ? 'import' : (e.by ?? '');
  if (e.event === 'recorded') {
    return { tone: 'ok', label: 'recorded', text: `this Orb read the hashes from ${p} · ${who}` };
  }
  const s = checkSummary(e, provider);
  const detail = e.detail ? ` · ${e.detail}` : '';
  return { tone: s.tone, label: e.event, text: `${who}${detail}` };
}
