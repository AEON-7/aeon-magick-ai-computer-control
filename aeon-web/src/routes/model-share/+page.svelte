<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import StorageManager from '$lib/components/StorageManager.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import DownloadProgress from '$lib/components/DownloadProgress.svelte';
  import { toast } from '$lib/toast';
  import { createScribble, personalEntropy, readDice, sha256Hex } from '$lib/entropy-collect.js';

  // Intergalactic Model Share — a decentralized, censorship-resistant network of
  // AI models shared over IPFS (the InterPlanetary File System — we think
  // bigger). Every Orb auto-enrolls at boot and gossips its catalog on a pubsub
  // topic, so this console shows models from every Orb on the network with no
  // token or fleet enrollment. Upload a model (with a model card), browse what
  // others share, and download (pin) any of them to host it here too — every
  // holder helps serve it, so popular models download faster.

  type Card = {
    kind?: string; base_model?: string; params?: string; quant?: string;
    license?: string; description?: string; intended_use?: string;
    tags?: string[]; format?: string; image?: string; readme?: boolean;
    nsfw?: boolean; gallery?: string[];
  };
  type Entry = {
    cid: string; name: string; file?: string; size_bytes: number;
    sha256?: string; card?: Card; added_at_ms?: number;
    origin_id?: string; origin_label?: string;
    verified?: boolean; source?: string;
  };
  type Host = { label: string; is_self: boolean; online: boolean; peer_id?: string; addrs?: string[]; is_origin?: boolean };
  type Sig = { ok: boolean; name: string; fp: string };
  // Who a model is credited to, and how much this Orb has reason to trust them.
  // `standing`: unsigned | new | proven (a download from them verified here) |
  // trusted / blocked (your decision) | failed (their bytes failed verification here).
  type Origin = {
    key: string; fp: string; name: string; signed: boolean; delegated: boolean;
    standing: 'unsigned' | 'new' | 'proven' | 'trusted' | 'blocked' | 'failed' | 'mirror';
    reputation: number; seeder_online: boolean;
    upstream?: string; // set on an unmodified mirror of a vendor release (e.g. the Hailo model zoo)
    content?: { manifest: boolean; files_ok: boolean; origin_ok: boolean; upstream?: string; checked_ms: number } | null;
    earlier_claim?: { key: string; fp: string; first_seen_ms: number } | null;
  };
  type Row = { entry: Entry; hosts: Host[]; local: boolean; host_count?: number; star_count?: number; starred?: boolean; signature?: Sig | null; origin?: Origin; blocked?: boolean };
  type Karma = { served_bytes: number; downloaded_bytes: number; ratio: number };
  type NodeStatus = {
    enabled: boolean; daemon: string; peers: number; gateway_port: number;
    repo_bytes: number; storage_max: string;
  };

  let node: NodeStatus | null = null;
  let rows: Row[] = [];
  let karma: Karma | null = null;
  let myStarCount = 0;
  $: karmaPct = karma ? Math.round((karma.served_bytes / Math.max(1, karma.served_bytes + karma.downloaded_bytes)) * 100) : 0;
  // An in-flight upload/download/fetch. `pct` (0–100) + bytes are present only
  // for downloads whose total size the Orb knows; otherwise it's a plain phase.
  type Task = { phase: string; pct?: number | null; done_bytes?: number; total_bytes?: number };
  let tasks: Record<string, Task> = {};
  let peerCount = 0;
  let net: {
    orbs_online: number; models: number; shared_bytes: number; local_bytes: number; local_models?: number; seeding: boolean;
    our_pin?: { models: number; bytes: number };
    our_files?: { count: number; bytes: number };
  } = {
    orbs_online: 0, models: 0, shared_bytes: 0, local_bytes: 0, seeding: true,
  };
  let selfPeer = '';
  let err = '';
  let poll: ReturnType<typeof setTimeout>;
  // Any non-error task means work is happening → poll faster for a smooth bar.
  $: busyNow = Object.values(tasks).some((t) => !String(t?.phase ?? '').startsWith('error'));

  // filters
  let query = '';
  let kindFilter = 'all';
  let onDevice = false;        // show only models this Orb has downloaded
  let activeTags = new Set<string>();
  // mature-content opt-in (18+ attestation)
  let matureOk = false;
  let showAgeGate = false;
  let ageAttest18 = false;
  let ageAccept = false;
  const KINDS = ['all', 'llm', 'vlm', 'vision', 'stt', 'tts', 'embedding', 'other'];
  const KIND_ICON: Record<string, string> = {
    llm: '💬', vlm: '👁️', vision: '🖼️', stt: '🎙️', tts: '🔊', embedding: '🧭', other: '📦',
  };

  // upload / share form
  let fileInput: HTMLInputElement;
  let imageInput: HTMLInputElement;
  let pendingFile: File | null = null;      // weights (null when editing metadata only)
  let editingCid: string | null = null;      // set when editing an existing model
  let form = { name: '', kind: 'llm', base_model: '', params: '', quant: '', format: '', license: '', description: '', intended_use: '', tags: '', readme: '' };
  let imageB64 = '';   // data URL of a newly picked image (share or replace)
  let imageExt = '';
  let existingImageUrl = '';  // gateway URL of the current image when editing
  let removeImage = false;
  let uploadPct = -1;
  let uploadName = '';
  let showShare = false;
  let saving = false;

  // detail modal
  let detail: Row | null = null;
  let detailReadme = '';       // fetched README text for the open detail
  let detailReadmeLoaded = '';  // cid whose readme we've fetched

  // IPFS toggle + HuggingFace import
  let toggling = false;
  let hfUrl = '';
  let hfBusy = false;
  let ollamaRef = '';
  let ollamaBusy = false;
  let civitaiRef = '';
  let civitaiBusy = false;
  let tokens: { huggingface: boolean; civitai: boolean; ollama: boolean } = { huggingface: false, civitai: false, ollama: false };
  let showTokens = false;
  let tokenInput: { huggingface: string; civitai: string; ollama: string } = { huggingface: '', civitai: '', ollama: '' };
  const tokenRows = [
    { src: 'huggingface' as const, label: 'HuggingFace', ph: 'hf_xxx…' },
    { src: 'civitai' as const, label: 'Civitai', ph: 'Civitai API key' },
    { src: 'ollama' as const, label: 'Ollama', ph: 'ollama token' },
  ];

  // ── Push a model to a connected system (Agent Dashboard: DGX / gateways) ──
  let systems: Array<{ id: string; label: string; address: string; roles?: string[]; status?: string }> = [];
  let pushSel = '';
  let pushMsg = '';
  let pushActive = false;

  // Push dialog + destination
  let pushEntry: Entry | null = null;        // the model being pushed (opens the dialog)
  let pushMode: 'home' | 'custom' = 'home';  // default home folder vs a browsed folder
  let pushDest = '';                          // chosen target directory (custom mode)

  // Remote folder browser (lists directories on the target over SSH)
  let browsePath = '';
  let browseParent = '';
  let browseDirs: string[] = [];
  let browseLoading = false;
  let browseErr = '';

  async function loadSystems() {
    try {
      const r = await fetch('/api/agent/systems', { credentials: 'same-origin' }).then((x) => x.json());
      systems = r?.systems ?? [];
      if (!pushSel && systems[0]) pushSel = systems[0].id;
    } catch { /* not admin / none registered */ }
  }

  function slugify(name: string): string {
    const s = name.trim().toLowerCase().replace(/[^a-z0-9.-]+/g, '-').replace(/^-+|-+$/g, '');
    return (s || 'model').slice(0, 80);
  }

  function openPush(entry: Entry) {
    pushEntry = entry;
    pushMode = 'home'; pushDest = '';
    browseErr = ''; browseDirs = [];
    pushMsg = ''; pushActive = false;
    loadSystems();
  }
  function closePush() { pushEntry = null; }

  function doPush() {
    if (!pushEntry || !pushSel) return;
    // home → backend defaults to ~/aeon-models/<slug>; custom → the browsed
    // folder + a per-model subdir so pushes don't collide.
    const dest = pushMode === 'custom' && pushDest ? `${pushDest.replace(/\/+$/, '')}/${slugify(pushEntry.name)}` : '';
    pushToSystem(pushEntry, pushSel, dest);
  }

  async function pushToSystem(entry: Entry, systemId: string, dest: string) {
    pushActive = true; pushMsg = 'starting…';
    try {
      const r = await fetch(`/api/agent/systems/${encodeURIComponent(systemId)}/models/push`, {
        method: 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid: entry.cid, name: entry.name, dest }),
      }).then((x) => x.json());
      if (!r?.ok) { pushMsg = 'error: ' + (r?.err ?? 'push failed'); pushActive = false; return; }
      const slug = String(r.task ?? '').split(':').slice(1).join(':');
      pollPush(systemId, slug);
    } catch (e: any) { pushMsg = 'error: ' + (e?.message ?? 'push failed'); pushActive = false; }
  }

  async function browseTo(path: string) {
    if (!pushSel) return;
    browseLoading = true; browseErr = '';
    try {
      const r = await fetch(`/api/agent/systems/${encodeURIComponent(pushSel)}/browse?path=${encodeURIComponent(path)}`, { credentials: 'same-origin' }).then((x) => x.json());
      if (!r?.ok) { browseErr = r?.err ?? 'cannot open folder'; }
      else { browsePath = r.path; browseParent = r.parent ?? ''; browseDirs = r.dirs ?? []; pushMode = 'custom'; pushDest = r.path; }
    } catch (e: any) { browseErr = e?.message ?? 'browse failed'; }
    browseLoading = false;
  }
  function startBrowse() { browseTo(''); }

  async function pollPush(sysId: string, slug: string) {
    try {
      const r = await fetch(`/api/agent/systems/${encodeURIComponent(sysId)}/models/push/status`, { credentials: 'same-origin' }).then((x) => x.json());
      const st = r?.pushes?.[slug];
      if (st) {
        pushMsg =
          st.phase === 'transferring' ? `transferring ${st.pct ?? 0}%`
          : st.phase === 'pulling' ? 'pulling to Orb…'
          : st.phase === 'queued' ? 'queued…'
          : st.phase === 'done' ? '✓ pushed to ' + (st.system ?? 'system') + ' → ' + (st.path ?? '')
          : st.phase === 'error' ? 'error: ' + (st.err ?? 'failed')
          : String(st.phase ?? '');
        if (st.done) { pushActive = false; return; }
      }
    } catch { /* transient */ }
    setTimeout(() => pollPush(sysId, slug), 1500);
  }

  async function toggleIpfs() {
    if (!node) return;
    toggling = true; err = '';
    try {
      await api(node.enabled ? '/disable' : '/enable', { method: 'POST' });
    } catch (e: any) { err = e?.message ?? 'toggle failed'; }
    // enabling installs kubo on first run — give it a moment, then reload.
    setTimeout(async () => { toggling = false; await load(); }, 1500);
  }

  async function importHf() {
    const url = hfUrl.trim();
    if (!url) return;
    hfBusy = true; err = '';
    try {
      const r = await api('/models/import-hf', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ url }),
      });
      if (r && r.ok === false) err = r.err || 'import failed';
      else hfUrl = '';
    } catch (e: any) { err = e?.message ?? 'import failed'; }
    hfBusy = false; await load();
  }

  async function importOllama() {
    const reference = ollamaRef.trim();
    if (!reference) return;
    ollamaBusy = true; err = '';
    try {
      const r = await api('/models/import-ollama', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ reference }),
      });
      if (r && r.ok === false) err = r.err || 'import failed';
      else ollamaRef = '';
    } catch (e: any) { err = e?.message ?? 'import failed'; }
    ollamaBusy = false; await load();
  }

  async function importCivitai() {
    const reference = civitaiRef.trim();
    if (!reference) return;
    civitaiBusy = true; err = '';
    try {
      const r = await api('/models/import-civitai', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ reference }),
      });
      if (r && r.ok === false) err = r.err || 'import failed';
      else civitaiRef = '';
    } catch (e: any) { err = e?.message ?? 'import failed'; }
    civitaiBusy = false; await load();
  }

  async function loadTokens() {
    try { const r = await api('/models/tokens'); if (r?.ok) tokens = { huggingface: !!r.huggingface, civitai: !!r.civitai, ollama: !!r.ollama }; } catch {}
  }

  async function saveToken(source: 'huggingface' | 'civitai' | 'ollama') {
    try {
      await api('/models/tokens', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ source, token: tokenInput[source] }),
      });
      tokenInput[source] = '';
      await loadTokens();
    } catch {}
  }

  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/ipfs${path}`, { credentials: 'same-origin', ...opts }).then((r) => r.json());
  // Publisher identity lives at /api/publisher (not /api/ipfs).
  const papi = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/publisher${path}`, { credentials: 'same-origin', ...opts }).then((r) => r.json());

  // ── Publisher identity — the name you sign your models with ───────────────
  // An identity is an ed25519 key made from a 24-word recovery phrase. Signing
  // in ONCE on an Orb authorises that Orb to sign in your name from then on —
  // across restarts — until you sign out. `pubUnlocked` is the identity this
  // Orb is currently signed in as ('' = none).
  let pubAccounts: { pubkey: string; username: string; fingerprint: string; protection_current?: boolean }[] = [];
  let pubUnlocked = '';
  let pubSignedSince = 0;
  let pubOrbFp = '';
  // What a new key would be made from right now (system generator, hardware chip).
  let pubEntropy: { verdict: string; summary: string; sources: string[]; hardware_ok: boolean; recommend_personal?: boolean } | null = null;
  // Optional randomness the person adds themselves: a scribble and/or real dice.
  let showExtra = false;
  let entropyTab: 'scribble' | 'dice' = 'scribble';
  let scribble = createScribble();
  let scribbleCanvas: HTMLCanvasElement | undefined;
  let scribbleTick = 0;         // bumped on every sample so the meter redraws
  let scribbleDown = false;
  let scribbleLast: { x: number; y: number } | null = null;
  let diceText = '';
  $: dice = readDice(diceText);
  // The scribble tracker is a plain object, so Svelte cannot see it change;
  // reading the tick alongside it is what makes these recompute.
  const onTick = <T,>(_tick: number, value: T): T => value;
  $: scribbleProgress = onTick(scribbleTick, scribble.progress);
  $: scribbleStale = onTick(scribbleTick, scribble.repetitive);
  $: scribbleUsed = onTick(scribbleTick, scribble.count > 20);
  $: hasPersonal = scribbleUsed || dice.rolls.length > 0;
  function scribblePoint(e: PointerEvent, start: boolean) {
    const c = scribbleCanvas;
    if (!c) return;
    const r = c.getBoundingClientRect();
    // Coalesced events give every position the pointer reported, not just one per frame.
    const events = !start && typeof e.getCoalescedEvents === 'function' ? e.getCoalescedEvents() : [];
    for (const ev of events.length ? events : [e]) {
      const x = ev.clientX - r.left, y = ev.clientY - r.top;
      scribble.add(x, y, performance.now(), r.width, r.height);
      const ctx = c.getContext('2d');
      if (ctx) {
        const sx = c.width / r.width, sy = c.height / r.height;
        ctx.lineWidth = 2 * sx; ctx.lineCap = 'round'; ctx.strokeStyle = '#a78bfa';
        ctx.beginPath();
        if (start || !scribbleLast) ctx.moveTo(x * sx, y * sy); else ctx.moveTo(scribbleLast.x * sx, scribbleLast.y * sy);
        ctx.lineTo(x * sx, y * sy);
        ctx.stroke();
      }
      scribbleLast = { x, y };
      start = false;
    }
    scribbleTick++;
  }
  function scribbleStart(e: PointerEvent) { scribbleDown = true; scribbleLast = null; scribbleCanvas?.setPointerCapture?.(e.pointerId); scribblePoint(e, true); }
  function scribbleMove(e: PointerEvent) { if (scribbleDown) scribblePoint(e, false); }
  function scribbleEnd() { scribbleDown = false; scribbleLast = null; }
  function scribbleClear() {
    scribble = createScribble(); scribbleLast = null; scribbleTick++;
    const c = scribbleCanvas; c?.getContext('2d')?.clearRect(0, 0, c.width, c.height);
  }
  async function buildPersonalEntropy(): Promise<string> {
    const hex = scribble.count > 20 ? await sha256Hex(scribble.bytes()) : '';
    return personalEntropy(hex, dice.rolls);
  }
  let madeFrom: string[] = [];  // what the identity just created was actually mixed from
  const SOURCE_NAMES: Record<string, string> = { kernel: "the Orb's system generator", hardware: 'its hardware random-number chip', personal: 'the randomness you added' };
  const nameSources = (list: string[]) => list.map((x) => SOURCE_NAMES[x] ?? x).join(' + ');
  $: pubSigning = !!pubUnlocked;
  $: pubName = pubAccounts.find((a) => a.pubkey === pubUnlocked)?.username ?? '';
  $: pubFp = pubUnlocked.slice(0, 12);
  let showPub = false;
  let pubMode: 'create' | 'unlock' | 'restore' = 'create';
  // create wizard: fill in → write the words down → prove you did
  let pubStep: 'form' | 'words' | 'check' = 'form';
  let pubUser = '';
  let pubPass = '';
  let pubPass2 = '';
  let pubSel = '';
  let pubBusy = false;
  let seedWords: string[] = []; // shown ONCE after create — the user must write it down
  let seedAck = false;
  let seedCheckIdx: number[] = [];      // which word positions we ask back
  let seedCheckVals: string[] = [];
  let seedInput = ''; // 24-word phrase pasted in when restoring on a new Orb
  // per-upload choice: put my name on this one (default) or the Orb's key only
  let signAsMe = true;
  // quick sign-in (publisher card + inside the share dialog)
  let quickPass = '';
  const PASS_MIN = 8;
  const postJson = (body: unknown): RequestInit => ({ method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });

  async function loadPublisher() {
    try {
      const r = await papi('/accounts');
      if (r?.ok) {
        pubAccounts = r.accounts ?? [];
        pubUnlocked = r.unlocked ?? '';
        pubSignedSince = r.signed_in_since_ms ?? 0;
        pubOrbFp = r.orb_fingerprint ?? '';
        pubEntropy = r.entropy ?? null;
        if (!pubSel || !pubAccounts.some((a) => a.pubkey === pubSel)) pubSel = pubAccounts[0]?.pubkey ?? '';
      }
    } catch {}
  }
  function openPublisher(mode?: 'create' | 'unlock' | 'restore') {
    pubMode = mode ?? (pubAccounts.length ? 'unlock' : 'create');
    pubStep = 'form';
    pubSel = pubAccounts[0]?.pubkey ?? '';
    pubUser = ''; pubPass = ''; pubPass2 = ''; seedWords = []; seedAck = false; seedInput = ''; showPub = true;
    scribble = createScribble(); scribbleLast = null; scribbleTick++; diceText = ''; madeFrom = [];
    // Where the Orb has no second source of its own, offer this up front.
    showExtra = !!pubEntropy?.recommend_personal; entropyTab = 'scribble';
  }
  function closePublisher() {
    // The recovery phrase is on screen exactly once. Don't let a stray click lose it.
    if (seedWords.length && !confirm('Close without finishing? Your recovery phrase will not be shown again — make sure it is written down.')) return;
    seedWords = []; showPub = false;
  }
  async function pubCreate() {
    if (!pubUser.trim()) { toast('Pick a name to publish under.', 'error'); return; }
    if (pubPass.length < PASS_MIN) { toast(`Use a password of at least ${PASS_MIN} characters.`, 'error'); return; }
    if (pubPass !== pubPass2) { toast('The two passwords do not match.', 'error'); return; }
    pubBusy = true;
    try {
      const pw = pubPass;
      const r = await papi('/accounts', postJson({ username: pubUser.trim(), password: pw, extra_entropy: await buildPersonalEntropy() }));
      if (!r?.ok) { toast('Could not create the identity: ' + (r?.err ?? 'error'), 'error'); return; }
      seedWords = String(r.seed_phrase ?? '').split(/\s+/).filter(Boolean);
      madeFrom = r.entropy_sources ?? [];
      scribble = createScribble(); diceText = '';
      // Older supervisors needed a separate unlock to start signing.
      if (!r.signed_in) await papi('/unlock', postJson({ pubkey: r.pubkey, password: pw }));
      await loadPublisher();
      pubPass = ''; pubPass2 = ''; seedAck = false; pubStep = 'words';
    } finally { pubBusy = false; }
  }
  function startSeedCheck() {
    // Ask for three words at random positions, in order.
    const picks = new Set<number>();
    while (picks.size < 3) picks.add(Math.floor(Math.random() * seedWords.length));
    seedCheckIdx = [...picks].sort((a, b) => a - b);
    seedCheckVals = seedCheckIdx.map(() => '');
    pubStep = 'check';
  }
  $: seedCheckOk = seedCheckIdx.length === 3 && seedCheckIdx.every((i, n) => (seedCheckVals[n] ?? '').trim().toLowerCase() === seedWords[i]);
  function finishCreate() {
    if (!seedCheckOk) { toast('Those words do not match — check what you wrote down.', 'error'); return; }
    seedWords = []; showPub = false;
    toast(`You're signed in as @${pubName}. Everything you share from this Orb now carries your signature.`, 'success');
  }
  // Restore an existing identity from its 24-word seed on a fresh Orb. The key is
  // DERIVED from the phrase, so the pubkey (and reputation) come back identical;
  // the password just re-encrypts it locally and can differ from the original.
  async function pubRestore() {
    const words = seedInput.trim().split(/\s+/).filter(Boolean);
    if (!pubUser.trim() || pubPass.length < PASS_MIN) { toast(`Enter a name and a password of at least ${PASS_MIN} characters.`, 'error'); return; }
    if (words.length !== 24) { toast(`Enter your 24-word recovery phrase (you have ${words.length} word${words.length === 1 ? '' : 's'}).`, 'error'); return; }
    pubBusy = true;
    try {
      const pw = pubPass;
      const r = await papi('/accounts', postJson({ username: pubUser.trim(), password: pw, seed_phrase: words.join(' ') }));
      if (!r?.ok) { toast('Restore failed — ' + (r?.err ?? 'check the recovery phrase'), 'error'); return; }
      if (!r.signed_in) await papi('/unlock', postJson({ pubkey: r.pubkey, password: pw }));
      await loadPublisher();
      showPub = false; pubPass = ''; seedInput = '';
      toast(`Restored @${pubUser.trim()} · ${String(r.pubkey).slice(0, 12)} — signed in as your original identity.`, 'success');
    } finally { pubBusy = false; }
  }
  // Sign in: authorises this Orb to sign as the identity from now on.
  async function pubSignIn(pubkey: string, password: string): Promise<boolean> {
    if (!pubkey || !password) { toast('Enter your identity password.', 'error'); return false; }
    pubBusy = true;
    try {
      const r = await papi('/unlock', postJson({ pubkey, password }));
      if (!r?.ok) { toast('Could not sign in — ' + (r?.err ?? 'wrong password'), 'error'); return false; }
      await loadPublisher();
      toast(`Signed in as @${r.username ?? pubName}. This Orb will keep signing as you, including after restarts, until you sign out.`, 'success');
      return true;
    } finally { pubBusy = false; }
  }
  async function pubUnlock() { if (await pubSignIn(pubSel, pubPass)) { showPub = false; pubPass = ''; } }
  async function quickSignIn() { if (await pubSignIn(pubSel || pubAccounts[0]?.pubkey, quickPass)) quickPass = ''; }
  async function pubLock() {
    if (!confirm(`Sign out @${pubName} on this Orb? Models you share afterwards will be signed by the Orb's own key, without your name, until you sign in again.`)) return;
    await papi('/lock', { method: 'POST' });
    await loadPublisher();
    toast('Signed out. Your identity is still stored on this Orb, encrypted with your password.', 'info');
  }
  async function copyText(text: string, what: string) {
    try { await navigator.clipboard.writeText(text); toast(`${what} copied.`, 'success'); }
    catch { toast(`Could not copy — select the ${what.toLowerCase()} and copy it by hand.`, 'error'); }
  }

  // ── Proofs: tie the key to your accounts elsewhere ────────────────────────
  // A proof is a signed line saying "this key is <account>". Posted ON that
  // account, it lets anyone check that the two belong to the same person.
  const PROOF_SITES = [
    { v: 'huggingface.co', label: 'Hugging Face', where: 'your profile README or a model card' },
    { v: 'github.com', label: 'GitHub', where: 'your profile README or a public gist' },
    { v: 'x.com', label: 'X / Twitter', where: 'a pinned post (it fits in a reply thread or an image alt text)' },
    { v: 'civitai.com', label: 'Civitai', where: 'your profile bio or a model description' },
    { v: '', label: 'Website or other', where: 'a page you control' },
  ];
  let showProof = false;
  let proofTab: 'make' | 'check' = 'make';
  let proofSite = 'huggingface.co';
  let proofHandle = '';
  let proofPass = '';
  let proofText = '';
  let proofCheckText = '';
  let proofResult: { valid: boolean; why?: string; key?: string; fingerprint?: string; name?: string; claim?: string; issued_ms?: number; is_mine?: boolean } | null = null;
  $: proofClaim = proofSite ? `${proofSite}/${proofHandle.trim().replace(/^@/, '')}` : proofHandle.trim();
  $: proofWhere = PROOF_SITES.find((x) => x.v === proofSite)?.where ?? 'a page you control';
  function openProof(tab: 'make' | 'check' = 'make') {
    proofTab = tab; proofText = ''; proofPass = ''; proofResult = null; proofCheckText = ''; showProof = true;
  }
  async function makeProof() {
    if (!proofHandle.trim()) { toast('Enter the account name (or address) you want to prove.', 'error'); return; }
    pubBusy = true;
    try {
      const r = await papi('/proof', postJson({ pubkey: pubUnlocked || pubSel, password: proofPass, claim: proofClaim }));
      if (!r?.ok) { toast(r?.needs_password ? 'Enter your identity password — a proof is signed with your own key.' : (r?.err ?? 'Could not create the proof'), 'error'); return; }
      proofText = r.proof; proofPass = '';
    } finally { pubBusy = false; }
  }
  async function checkProof() {
    proofResult = null;
    if (!proofCheckText.trim()) return;
    const r = await papi('/verify', postJson({ proof: proofCheckText }));
    proofResult = r?.ok ? r : { valid: false, why: r?.err ?? 'could not check that' };
  }

  async function load() {
    try {
      node = await api('/status');
      if (node?.daemon === 'active') {
        const r = await api('/models/registry');
        if (r?.ok) {
          rows = r.models ?? [];
          if (detail) detail = rows.find((x) => x.entry.cid === detail?.entry.cid) ?? detail;
          tasks = r.tasks ?? {};
          peerCount = r.peer_count ?? 0;
          if (r.network) net = { ...net, ...r.network, seeding: r.network.seeding !== false };
          selfPeer = r.self_peer_id ?? '';
          myStarCount = r.my_star_count ?? 0;
          err = '';
        }
      }
    } catch (e: any) {
      err = e?.message ?? 'failed to load';
    }
  }

  async function toggleSeed() {
    const next = !net.seeding;
    try {
      const r = await api('/models/seed', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ seed: next }),
      });
      if (r?.ok) net = { ...net, seeding: !!r.seed };
    } catch {}
  }

  async function loadKarma() {
    try {
      const r = await api('/models/karma');
      if (r?.ok) karma = r.karma;
    } catch {}
  }

  async function toggleStar(cid: string) {
    // optimistic: flip locally, then persist
    rows = rows.map((r) => r.entry.cid === cid
      ? { ...r, starred: !r.starred, star_count: (r.star_count ?? 0) + (r.starred ? -1 : 1) }
      : r);
    try {
      await api('/models/star', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid }),
      });
    } catch {}
    await load();
  }

  function fmtBytes(n: number): string {
    if (!n) return '—';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0, x = n;
    while (x >= 1024 && i < u.length - 1) { x /= 1024; i++; }
    return `${x.toFixed(x < 10 && i > 0 ? 1 : 0)} ${u[i]}`;
  }

  // Card images are served THROUGH the supervisor (same-origin HTTPS), not the
  // plain-HTTP :8080 gateway — otherwise the browser blocks them as mixed
  // content on the HTTPS console and they silently fail to render.
  const imageSrc = (cid: string, name: string) =>
    `/api/ipfs/models/image?cid=${cid}&name=${encodeURIComponent(name)}`;

  $: gatewayBase =
    typeof location !== 'undefined' ? `http://${location.hostname}:${node?.gateway_port ?? 8080}` : '';
  function downloadUrl(e: Entry) {
    return `${gatewayBase}/ipfs/${e.cid}${e.file ? '/' + encodeURIComponent(e.file) : ''}?download=true&filename=${encodeURIComponent(e.name)}`;
  }

  $: filtered = rows.filter((r) => {
    const c = r.entry.card;
    if (c?.nsfw && !matureOk) return false;                 // mature hidden unless opted-in
    if (onDevice && !r.local) return false;                 // On Device
    if (kindFilter !== 'all' && (c?.kind || 'other') !== kindFilter) return false;
    if (activeTags.size && !(c?.tags ?? []).some((t) => activeTags.has(t))) return false;
    if (!query.trim()) return true;
    const q = query.toLowerCase();
    return [r.entry.name, c?.base_model, c?.description, c?.params, c?.quant, ...(c?.tags ?? [])]
      .filter(Boolean).join(' ').toLowerCase().includes(q);
  });

  // Most common tags across the (mature-filtered) library — for quick filtering.
  $: commonTags = (() => {
    const counts = new Map<string, number>();
    for (const r of rows) {
      if (r.entry.card?.nsfw && !matureOk) continue;
      for (const t of r.entry.card?.tags ?? []) counts.set(t, (counts.get(t) ?? 0) + 1);
    }
    return [...counts.entries()].sort((a, b) => b[1] - a[1]).slice(0, 14).map(([t]) => t);
  })();

  function resetForm() {
    pendingFile = null; editingCid = null; imageB64 = ''; imageExt = '';
    existingImageUrl = ''; removeImage = false; saving = false;
    form = { name: '', kind: 'llm', base_model: '', params: '', quant: '', format: '', license: '', description: '', intended_use: '', tags: '', readme: '' };
  }

  // ── Share (new model) ──
  function pickFile() { resetForm(); fileInput?.click(); }
  function onFile() {
    const f = fileInput?.files?.[0];
    if (!f) return;
    pendingFile = f;
    if (!form.name) form.name = f.name.replace(/\.(gguf|safetensors|onnx|hef|bin|pt|pth)$/i, '');
    const ext = (f.name.split('.').pop() || '').toLowerCase();
    if (!form.format && ['gguf', 'safetensors', 'onnx', 'hef', 'bin', 'pt', 'pth'].includes(ext)) form.format = ext;
    showShare = true;
    if (fileInput) fileInput.value = '';
  }

  // ── Edit (existing model — metadata / image / README only) ──
  async function editModel(row: Row) {
    resetForm();
    editingCid = row.entry.cid;
    const c = row.entry.card ?? {};
    form = {
      name: row.entry.name, kind: c.kind || 'other', base_model: c.base_model || '',
      params: c.params || '', quant: c.quant || '', format: c.format || '',
      license: c.license || '', description: c.description || '', intended_use: c.intended_use || '',
      tags: (c.tags || []).join(', '), readme: '',
    };
    if (c.image) existingImageUrl = imageSrc(row.entry.cid, c.image);
    showShare = true;
    detail = null;
    // pull the existing README so the editor is prefilled
    if (c.readme) {
      try {
        const r = await api(`/models/file?cid=${row.entry.cid}&name=README.md`);
        if (r?.ok) form.readme = r.text ?? '';
      } catch {}
    }
  }

  function onImage() {
    const f = imageInput?.files?.[0];
    if (!f) return;
    if (f.size > 4 * 1024 * 1024) { err = 'image too large (max 4 MB)'; return; }
    imageExt = (f.name.split('.').pop() || 'png').toLowerCase();
    removeImage = false;
    const reader = new FileReader();
    reader.onload = () => { imageB64 = String(reader.result || ''); };
    reader.readAsDataURL(f);
    if (imageInput) imageInput.value = '';
  }
  function clearImage() { imageB64 = ''; existingImageUrl = ''; removeImage = true; }

  function cardFromForm(): Card {
    return {
      kind: form.kind, base_model: form.base_model.trim(), params: form.params.trim(),
      quant: form.quant.trim(), format: form.format.trim(), license: form.license.trim(),
      description: form.description.trim(), intended_use: form.intended_use.trim(),
      tags: form.tags.split(',').map((t) => t.trim()).filter(Boolean),
    };
  }

  // Publish a new model: stream the weights (progress), then finalize with the
  // card + README + image as JSON.
  function submitShare() {
    if (editingCid) return submitEdit();
    if (!pendingFile) return;
    const f = pendingFile;
    uploadName = form.name || f.name;
    uploadPct = 0; err = '';
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/api/ipfs/models/upload-weights');
    xhr.setRequestHeader('Content-Disposition', `attachment; filename="${f.name}"`);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.withCredentials = true;
    xhr.upload.addEventListener('progress', (e) => {
      if (e.lengthComputable) uploadPct = Math.floor((e.loaded / e.total) * 100);
    });
    xhr.addEventListener('load', async () => {
      let draft: any = {};
      try { draft = JSON.parse(xhr.responseText); } catch {}
      if (!draft.ok) { uploadPct = -1; err = draft.err || `HTTP ${xhr.status}`; return; }
      uploadPct = 100;
      const r = await api('/models/publish', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          draft_id: draft.draft_id, name: form.name.trim() || f.name, card: cardFromForm(),
          readme: form.readme, image_b64: imageB64, image_ext: imageExt,
          // Put my name on it — only meaningful while signed in.
          sign_identity: signAsMe && pubSigning,
        }),
      });
      uploadPct = -1;
      if (r && r.ok === false) err = r.err || 'publish failed';
      showShare = false; resetForm(); await load();
    });
    xhr.addEventListener('error', () => { uploadPct = -1; err = 'upload failed (network)'; });
    xhr.send(f);
  }

  // Edit an existing model's metadata/image/README — rebuilds the IPFS
  // directory on the Orb reusing the weights (no re-upload).
  async function submitEdit() {
    if (!editingCid) return;
    saving = true; err = '';
    const body: any = { cid: editingCid, name: form.name.trim(), card: cardFromForm(), readme: form.readme };
    if (imageB64) { body.image_b64 = imageB64; body.image_ext = imageExt; }
    else if (removeImage) body.remove_image = true;
    try {
      const r = await api('/models/edit', {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
      });
      if (r && r.ok === false) err = r.err || 'edit failed';
    } catch (e: any) { err = e?.message ?? 'edit failed'; }
    saving = false; showShare = false; resetForm(); await load();
  }

  // ── Origin + publisher trust ───────────────────────────────────────────────
  const STANDING: Record<string, { icon: string; cls: string; label: string }> = {
    trusted:  { icon: '🛡', cls: 'text-emerald-300', label: 'trusted by you' },
    proven:   { icon: '🔏', cls: 'text-emerald-300/80', label: 'a download from this publisher verified on this Orb' },
    new:      { icon: '🔏', cls: 'text-ink-300', label: 'signed, but this Orb has not verified anything from this publisher yet' },
    failed:   { icon: '⚠', cls: 'text-red-400', label: 'a download from this publisher FAILED verification on this Orb' },
    blocked:  { icon: '⛔', cls: 'text-red-400', label: 'blocked by you' },
    unsigned: { icon: '·', cls: 'text-ink-600', label: 'unsigned — nobody has put a key behind this model' },
    mirror:   { icon: '⧉', cls: 'text-sky-300', label: 'an unmodified mirror of a vendor release — every Orb that downloads the same file seeds this same copy' },
  };
  const standing = (o?: Origin) => STANDING[o?.standing ?? 'unsigned'] ?? STANDING.unsigned;
  function originTitle(row: Row): string {
    const o = row.origin;
    if (o?.upstream) return `Mirror of ${o.upstream}: ${STANDING.mirror.label}. Seeded by ${row.host_count ?? row.hosts.length} Orb(s).`;
    if (!o?.signed) return STANDING.unsigned.label;
    const bits = [`Published by ${o.name || 'anon'} (key ${o.fp}${o.delegated ? ', signing through an Orb' : ''}) — ${standing(o).label}.`];
    bits.push(`Reputation ${o.reputation}/100.`);
    if (o.seeder_online) bits.push('The original seeder is online now.');
    if (o.content?.origin_ok) bits.push('Its files matched the signed manifest when this Orb downloaded it.');
    if (o.earlier_claim) bits.push(`The same weights were first seen here from a different key (${o.earlier_claim.fp}) — this may be a re-publication.`);
    return bits.join(' ');
  }
  // Put this Orb's key behind a model it shared before signing was automatic.
  async function claimModel(cid: string) {
    try {
      const r = await api('/models/claim', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ cid }) });
      if (r?.ok === false) { toast(r.err || 'Could not sign that model', 'error'); return; }
      toast(`Signed with this Orb's key (${r.signed_by}).`, 'success');
      await load();
    } catch (e: any) { toast(e?.message ?? 'Could not sign that model', 'error'); }
  }
  async function setTrust(key: string, patch: { trusted?: boolean; blocked?: boolean }) {
    try {
      const r = await api('/models/trust', {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ key, ...patch }),
      });
      if (r?.ok === false) { toast(r.err || 'Could not save that', 'error'); return; }
      await load();
    } catch (e: any) { toast(e?.message ?? 'Could not save that', 'error'); }
  }

  // Minimal, XSS-safe markdown → HTML for READMEs from untrusted Orbs: escape
  // ALL html first, then re-introduce only a fixed, safe tag set. Links are
  // autolinked http(s) only.
  function renderMd(src: string): string {
    let s = src.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
    s = s.replace(/```([\s\S]*?)```/g, (_m, c) => `<pre class="bg-ink-950 border border-ink-800 rounded p-2 overflow-x-auto text-[11px]">${c.trim()}</pre>`);
    s = s.replace(/`([^`]+)`/g, '<code class="text-cursed-300">$1</code>');
    s = s.replace(/^###\s+(.*)$/gm, '<h3 class="font-mono text-ink-100 mt-3 mb-1">$1</h3>');
    s = s.replace(/^##\s+(.*)$/gm, '<h2 class="font-mono text-ink-100 text-base mt-3 mb-1">$1</h2>');
    s = s.replace(/^#\s+(.*)$/gm, '<h1 class="font-mono text-ink-100 text-lg mt-3 mb-1">$1</h1>');
    s = s.replace(/\*\*([^*]+)\*\*/g, '<strong class="text-ink-100">$1</strong>');
    s = s.replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>');
    s = s.replace(/^[-*]\s+(.*)$/gm, '<li class="ml-4 list-disc">$1</li>');
    s = s.replace(/\bhttps?:\/\/[^\s<)]+/g, (u) => `<a href="${u}" target="_blank" rel="noreferrer" class="text-cursed-300 hover:underline">${u}</a>`);
    return s.replace(/\n{2,}/g, '<br><br>').replace(/\n/g, '<br>');
  }

  // Lazy-load the README when a detail modal opens for a model that has one.
  $: if (detail && detail.entry.card?.readme && detailReadmeLoaded !== detail.entry.cid) {
    const cid = detail.entry.cid;
    detailReadmeLoaded = cid;
    detailReadme = '';
    api(`/models/file?cid=${cid}&name=README.md`).then((r) => { if (r?.ok && detail?.entry.cid === cid) detailReadme = r.text ?? ''; }).catch(() => {});
  }

  async function download(row: Row) {
    const src = row.hosts.find((h) => !h.is_self && h.online && h.peer_id) ?? row.hosts[0];
    err = '';
    try {
      const r = await api('/models/fetch', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...row.entry, addrs: src?.addrs ?? [], peer_id: src?.peer_id ?? '' }),
      });
      if (r && r.ok === false) err = r.err || 'download failed';
    } catch (e: any) { err = e?.message ?? 'download failed'; }
    await load();
    scheduleTick(); // pick up the fast cadence now that a fetch is in flight
  }

  // Purge a FAILED download: reclaim its partial (orphaned) IPFS blocks + clear
  // the error, so the card resets to a clean "host" state. (Retry, by contrast,
  // re-runs the download and RESUMES from whatever's already cached.)
  async function purgeDownload(cid: string) {
    err = '';
    try {
      const p = await api('/models/purge', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid }),
      });
      if (p && p.ok === false) { err = p.err || 'purge failed'; return; }
      // Drop the local task at once (the server already cleared it) so the card
      // flips back to the download button without waiting for the next poll.
      const { [cid]: _drop, ...rest } = tasks;
      tasks = rest;
      toast(p?.message || 'Purged the failed download.', 'info');
    } catch (e: any) {
      err = e?.message ?? 'purge failed';
    }
    await load();
  }

  let confirmRemove = '';
  async function unshare(cid: string) {
    if (confirmRemove !== cid) {
      confirmRemove = cid;
      setTimeout(() => { if (confirmRemove === cid) confirmRemove = ''; }, 3000);
      return;
    }
    confirmRemove = '';
    try {
      await api('/models/remove', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cid }),
      });
    } catch {}
    if (detail?.entry.cid === cid) detail = null;
    await load();
  }

  // Self-adjusting poll: idle at 5 s, but while a download/upload is in flight
  // tick every 1.2 s so the progress bar advances smoothly.
  function scheduleTick() {
    clearTimeout(poll);
    poll = setTimeout(async () => {
      await load(); loadKarma();
      scheduleTick();
    }, busyNow ? 1200 : 5000);
  }
  onMount(() => {
    load(); loadSystems(); loadKarma(); loadTokens(); loadView(); loadPublisher();
    scheduleTick();
  });

  async function loadView() {
    try { const r = await api('/models/view'); if (r?.ok) matureOk = !!r.mature_ok; } catch {}
  }
  function toggleMature() {
    if (matureOk) { setMature(false); }            // turning OFF needs no gate
    else { ageAttest18 = false; ageAccept = false; showAgeGate = true; }
  }
  async function setMature(on: boolean) {
    try {
      const r = await api('/models/view', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mature_ok: on, attest_18: on }),
      });
      if (r?.ok) matureOk = on; else if (r?.err) err = r.err;
    } catch {}
    showAgeGate = false;
  }
  function toggleTag(t: string) {
    if (activeTags.has(t)) activeTags.delete(t); else activeTags.add(t);
    activeTags = activeTags;
  }
  // Reset the push status line whenever a different model detail opens.
  $: if (detail) { pushMsg = ''; pushActive = false; }
  onDestroy(() => clearTimeout(poll));
</script>

<div class="page-void min-h-screen">
  <header class="flex items-center justify-between px-5 py-3 chrome-header">
    <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
    <h1 class="font-mono text-lg text-cursed-300 flex items-center gap-2"><Icon name="aether" class="w-5 h-5" /> Intergalactic <span class="text-ink-400">Model Share</span></h1>
    <div class="w-28 text-right">
      {#if node?.daemon === 'active'}
        <span class="text-[10px] font-mono uppercase tracking-wider px-2 py-1 rounded bg-emerald-900/50 text-emerald-300"
              title="This Orb is enrolled in the Intergalactic Model Share network (IPFS)">on-network</span>
      {:else}
        <span class="text-[10px] font-mono uppercase tracking-wider px-2 py-1 rounded bg-amber-900/50 text-amber-300">connecting…</span>
      {/if}
    </div>
  </header>

  <main class="max-w-5xl mx-auto px-5 py-6 space-y-5">
    <p class="text-ink-400 text-xs leading-relaxed -mt-1">
      Built on <span class="text-cursed-300">IPFS — the InterPlanetary File System</span>. We just think bigger.
    </p>
    <p class="text-ink-300 text-sm leading-relaxed">
      A decentralized, censorship-resistant network for AI models — no fleet, no accounts, no central server to
      take down. Every Aeon Orb auto-joins at boot and announces the models it hosts, so the library below is
      contributed by Orbs everywhere, and each model is served by everyone who holds it — the more popular a
      model, the faster it downloads. <span class="text-cursed-300">Share</span> a model to publish it with a
      model card; <span class="text-cursed-300">download</span> anyone's to run it locally (and help host it).
    </p>

    {#if karma && (karma.served_bytes > 0 || karma.downloaded_bytes > 0)}
      <div class="panel px-4 py-3 flex items-center gap-4 flex-wrap">
        <div class="flex items-center gap-2 shrink-0">
          <span class="text-lg">☯</span>
          <div>
            <div class="text-xs font-mono text-cursed-300">Model Karma</div>
            <div class="text-[10px] text-ink-500">given back vs pulled</div>
          </div>
        </div>
        <div class="flex-1 min-w-[12rem]">
          <div class="h-2 rounded-full bg-ink-800 overflow-hidden flex">
            <div class="h-full bg-emerald-500" style="width:{karmaPct}%"></div>
            <div class="h-full bg-cursed-600" style="width:{100 - karmaPct}%"></div>
          </div>
          <div class="flex justify-between text-[10px] font-mono mt-1">
            <span class="text-emerald-400">served {fmtBytes(karma.served_bytes)}</span>
            <span class="text-cursed-300">downloaded {fmtBytes(karma.downloaded_bytes)}</span>
          </div>
        </div>
        <div class="text-right shrink-0">
          <div class="font-mono text-sm {karma.ratio >= 1 ? 'text-emerald-300' : 'text-ink-300'}" title="Bytes served ÷ bytes downloaded — above 1.0× means you give more than you take">{karma.ratio.toFixed(2)}×</div>
          <div class="text-[10px] text-ink-500">give / take</div>
        </div>
        {#if myStarCount > 0}<div class="text-[10px] font-mono text-amber-300 shrink-0" title="Models you've starred">★ {myStarCount}</div>{/if}
      </div>
    {/if}

    {#if err}
      <div class="rounded border border-red-700 bg-red-950/40 text-red-300 px-3 py-2 text-sm">{err}</div>
    {/if}

    <!-- IPFS node status + on/off toggle -->
    <div class="panel p-3 flex items-center gap-3 flex-wrap">
      <span class="font-mono text-[10px] uppercase tracking-wider px-2 py-1 rounded {node?.daemon === 'active' ? 'bg-emerald-900/50 text-emerald-300' : node?.daemon === 'failed' ? 'bg-red-900/50 text-red-300' : node?.enabled ? 'bg-amber-900/50 text-amber-300' : 'bg-ink-800 text-ink-400'}">
        {node?.daemon === 'active' ? 'IPFS running' : node?.daemon === 'failed' ? 'IPFS failed' : node?.enabled ? 'IPFS starting' : 'IPFS off'}
      </span>
      {#if node?.daemon === 'active'}
        <span class="text-ink-500 text-xs font-mono" title="Orbs currently providing the shared beacon, and the bytes of models pinned on that mesh">
          {net.orbs_online || peerCount} orb{(net.orbs_online || peerCount) === 1 ? '' : 's'} online
          · {net.models || rows.length} model{(net.models || rows.length) === 1 ? '' : 's'}
          · {fmtBytes(net.shared_bytes)} on the beacon
        </span>
        <span class="text-ink-500 text-xs font-mono" title="Directories this Orb is pinning for the mesh, and the weight files inside them">
          Our pin · {net.our_pin?.models ?? net.local_models ?? 0} model{(net.our_pin?.models ?? 0) === 1 ? '' : 's'}
          · {fmtBytes(net.our_pin?.bytes || net.local_bytes)}
          · Our files · {net.our_files?.count ?? 0}
          · {fmtBytes(net.our_files?.bytes || 0)}
        </span>
      {/if}
      <div class="flex-1"></div>
      <!-- on/off switch -->
      <button
        class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors {node?.enabled ? 'bg-cursed-600' : 'bg-ink-700'} disabled:opacity-50"
        on:click={toggleIpfs} disabled={toggling} title={node?.enabled ? 'Disable IPFS (leave Model Share)' : 'Enable IPFS (join Model Share)'}>
        <span class="inline-block h-5 w-5 transform rounded-full bg-white transition-transform {node?.enabled ? 'translate-x-5' : 'translate-x-0.5'}"></span>
      </button>
      <span class="text-xs font-mono text-ink-400 w-16">{toggling ? '…' : node?.enabled ? 'enabled' : 'disabled'}</span>
      <button
        class="text-[11px] font-mono px-2 py-1 rounded border {net.seeding ? 'border-emerald-700 text-emerald-300' : 'border-steel-700 text-ink-400'}"
        on:click={toggleSeed}
        title="Downloads and imports stay pinned on IPFS so other Orbs can fetch them. Turn this off to keep a private copy instead.">
        {net.seeding ? 'Seeding on' : 'Seeding off'}
      </button>
    </div>

    {#if node?.enabled}
      <!-- publisher identity — who the network sees as the author of what you share -->
      <section class="rounded-md border p-4 {pubSigning ? 'border-emerald-500/40 bg-emerald-500/5' : 'border-amber-500/40 bg-amber-500/5'}">
        {#if pubSigning}
          <div class="flex items-start gap-3 flex-wrap">
            <div class="text-3xl leading-none shrink-0" aria-hidden="true">🔏</div>
            <div class="min-w-0 flex-1">
              <div class="font-mono text-base text-emerald-300">Signing as @{pubName || 'you'}</div>
              <div class="text-[12px] text-ink-300 mt-0.5">
                Everything you share from this Orb carries your signature, so anyone can check it came from you.
                This Orb stays signed in — including after restarts — until you sign out.
              </div>
              <div class="mt-2 flex items-center gap-2 flex-wrap text-[11px] font-mono">
                <span class="text-ink-500">public key</span>
                <code class="px-1.5 py-0.5 rounded bg-ink-950 border border-ink-800 text-ink-200 break-all">{pubUnlocked}</code>
              </div>
            </div>
            <div class="flex flex-col gap-1.5 shrink-0 w-full sm:w-auto">
              <button class="btn-primary text-sm px-3 py-1.5 rounded" on:click={() => copyText(pubUnlocked, 'Public key')}>Copy public key</button>
              <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => openProof('make')}>Prove an account…</button>
              <button class="text-[11px] font-mono text-ink-500 hover:text-red-400 text-right" on:click={pubLock}>sign out</button>
            </div>
          </div>
        {:else if pubAccounts.length}
          <div class="flex items-start gap-3 flex-wrap">
            <div class="text-3xl leading-none shrink-0" aria-hidden="true">🔑</div>
            <div class="min-w-0 flex-1">
              <div class="font-mono text-base text-amber-300">You are not signed in</div>
              <div class="text-[12px] text-ink-300 mt-0.5">
                Models you share right now are signed by this Orb's key only, without your name.
                Sign in once and this Orb keeps signing as you — no need to do it again after a restart.
                {#if pubAccounts.some((a) => a.protection_current === false)}
                  <span class="block text-ink-500 mt-1">Signing in will also re-lock your key on this Orb with stronger password protection.</span>
                {/if}
              </div>
              <form class="mt-3 flex items-center gap-2 flex-wrap" on:submit|preventDefault={quickSignIn}>
                {#if pubAccounts.length > 1}
                  <select bind:value={pubSel} class="bg-ink-950 border border-steel-700 rounded px-2 py-2 text-sm font-mono">
                    {#each pubAccounts as a}<option value={a.pubkey}>@{a.username} · {a.fingerprint}</option>{/each}
                  </select>
                {:else}
                  <span class="font-mono text-sm text-ink-100">@{pubAccounts[0].username}</span>
                {/if}
                <input bind:value={quickPass} type="password" autocomplete="current-password" placeholder="identity password"
                       class="flex-1 min-w-[10rem] bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" />
                <button type="submit" class="btn-primary text-sm px-4 py-2 rounded disabled:opacity-50" disabled={pubBusy || !quickPass}>Sign in</button>
              </form>
              <div class="mt-2 flex gap-3 text-[11px] font-mono">
                <button class="text-ink-500 hover:text-ink-300" on:click={() => openPublisher('restore')}>restore a different identity</button>
                <button class="text-ink-500 hover:text-ink-300" on:click={() => openPublisher('create')}>create a new one</button>
              </div>
            </div>
          </div>
        {:else}
          <div class="flex items-start gap-3 flex-wrap">
            <div class="text-3xl leading-none shrink-0" aria-hidden="true">✍️</div>
            <div class="min-w-0 flex-1">
              <div class="font-mono text-base text-amber-300">Publish under your own name</div>
              <div class="text-[12px] text-ink-300 mt-0.5">
                A publisher identity is a signing key that is yours alone — no email, no account on anyone's server.
                It puts a name people can verify on every model you share, and it builds a reputation that follows you to any Orb.
              </div>
            </div>
            <div class="flex flex-col gap-1.5 shrink-0 w-full sm:w-auto">
              <button class="btn-primary text-sm px-4 py-2 rounded" on:click={() => openPublisher('create')}>Create my publisher identity</button>
              <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => openPublisher('restore')}>I have a recovery phrase</button>
            </div>
          </div>
        {/if}
      </section>

      <!-- Storage drive + LAN share sit as always-open tiles (not a collapsed
           details) so they're as obvious as "+ Share a model". -->
      <StorageManager variant="spotlight" />

      <!-- toolbar -->
      <div class="flex items-center gap-3 flex-wrap">
        <button class="btn-primary text-sm px-4 py-2 rounded-md" on:click={pickFile} disabled={uploadPct >= 0}>+ Share a model</button>
        <input type="file" bind:this={fileInput} class="hidden" on:change={onFile} />
        <input class="flex-1 min-w-[12rem] bg-ink-800 border border-steel-700 rounded px-3 py-2 text-ink-100 text-sm"
               bind:value={query} placeholder="Search models, base model, tags…" />
        <select bind:value={kindFilter} class="bg-ink-800 border border-steel-700 rounded px-2 py-2 text-ink-100 text-sm font-mono">
          {#each KINDS as k}<option value={k}>{k}</option>{/each}
        </select>
        <button class="text-xs font-mono px-2.5 py-2 rounded border {onDevice ? 'border-cursed-600 bg-cursed-950/40 text-cursed-300' : 'border-steel-700 text-ink-400 hover:text-ink-200'}"
                on:click={() => (onDevice = !onDevice)} title="Show only models this Orb has downloaded">⬇ On Device</button>
        <button class="text-xs font-mono px-2.5 py-2 rounded border {matureOk ? 'border-red-700 bg-red-950/40 text-red-300' : 'border-steel-700 text-ink-400 hover:text-ink-200'}"
                on:click={toggleMature} title="Opt in to view mature content (18+)">{matureOk ? '🔞 Mature: on' : 'Mature: off'}</button>
        <span class="text-ink-500 text-xs font-mono whitespace-nowrap">{filtered.length} model{filtered.length === 1 ? '' : 's'} · {peerCount} peer{peerCount === 1 ? '' : 's'}</span>
      </div>

      {#if commonTags.length}
        <div class="flex items-center gap-1.5 flex-wrap -mt-1">
          <span class="text-[10px] font-mono text-ink-600">tags:</span>
          {#each commonTags as t}
            <button class="text-[10px] font-mono px-1.5 py-0.5 rounded {activeTags.has(t) ? 'bg-cursed-700 text-white' : 'bg-ink-800 text-ink-400 hover:text-ink-200'}"
                    on:click={() => toggleTag(t)}>{t}</button>
          {/each}
          {#if activeTags.size}<button class="text-[10px] font-mono text-ink-500 hover:text-ink-300 underline" on:click={() => { activeTags = new Set(); }}>clear</button>{/if}
        </div>
      {/if}

      <!-- import straight from HuggingFace -->
      <div class="flex items-center gap-2 flex-wrap">
        <span class="text-lg">🤗</span>
        <input class="flex-1 min-w-[14rem] bg-ink-800 border border-steel-700 rounded px-3 py-2 text-ink-100 text-sm font-mono"
               bind:value={hfUrl} placeholder="Import from HuggingFace — paste a model URL (huggingface.co/org/model)"
               on:keydown={(e) => e.key === 'Enter' && importHf()} />
        <button class="btn text-sm px-4 py-2 rounded-md" on:click={importHf} disabled={hfBusy || !hfUrl.trim()}>{hfBusy ? 'Starting…' : 'Import'}</button>
      </div>
      <p class="text-[11px] text-ink-600 -mt-2">Pulls the weights, the README + author image, and verifies each file's SHA-256 against the hash HuggingFace publishes.</p>

      <!-- import from Ollama -->
      <div class="flex items-center gap-2 flex-wrap">
        <span class="text-lg">🦙</span>
        <input class="flex-1 min-w-[14rem] bg-ink-800 border border-steel-700 rounded px-3 py-2 text-ink-100 text-sm font-mono"
               bind:value={ollamaRef} placeholder="Import from Ollama — a model tag (e.g. llama3.2:3b or user/model:tag)"
               on:keydown={(e) => e.key === 'Enter' && importOllama()} />
        <button class="btn text-sm px-4 py-2 rounded-md" on:click={importOllama} disabled={ollamaBusy || !ollamaRef.trim()}>{ollamaBusy ? 'Starting…' : 'Import'}</button>
      </div>
      <p class="text-[11px] text-ink-600 -mt-2">Pulls the GGUF weights straight from the Ollama registry and verifies them against the layer digest — no Ollama install needed.</p>

      <!-- import generative models from Civitai -->
      <div class="flex items-center gap-2 flex-wrap">
        <span class="text-lg">🎨</span>
        <input class="flex-1 min-w-[14rem] bg-ink-800 border border-steel-700 rounded px-3 py-2 text-ink-100 text-sm font-mono"
               bind:value={civitaiRef} placeholder="Import from Civitai — a model URL (civitai.com/models/…) for checkpoints, LoRAs, VAEs"
               on:keydown={(e) => e.key === 'Enter' && importCivitai()} />
        <button class="btn text-sm px-4 py-2 rounded-md" on:click={importCivitai} disabled={civitaiBusy || !civitaiRef.trim()}>{civitaiBusy ? 'Starting…' : 'Import'}</button>
      </div>
      <p class="text-[11px] text-ink-600 -mt-2">Generative models for ComfyUI / Stable Diffusion. Prefers the SafeTensor file (never a pickle) and verifies the SHA-256. Gated + mature (“red”) content needs a Civitai token below.</p>

      <!-- optional per-source auth tokens for gated / mature pulls -->
      <div class="text-[11px]">
        <button class="text-cursed-400 hover:underline font-mono" on:click={() => (showTokens = !showTokens)}>
          {showTokens ? '▾' : '▸'} Auth tokens for gated models
          <span class="text-ink-600">({[tokens.huggingface && 'HF', tokens.civitai && 'Civitai', tokens.ollama && 'Ollama'].filter(Boolean).join(' · ') || 'none set'})</span>
        </button>
        {#if showTokens}
          <div class="mt-2 space-y-2 panel p-3">
            <p class="text-ink-500">Optional. A token lets the importer pull gated repos (HuggingFace), mature/“red” content (Civitai), or private models (Ollama). Stored encrypted-at-rest on this Orb (0600), never shared or gossiped.</p>
            {#each tokenRows as { src, label, ph }}
              <div class="flex items-center gap-2">
                <span class="w-24 text-ink-400 font-mono">{label}</span>
                <span class="text-[10px] px-1.5 py-0.5 rounded {tokens[src] ? 'bg-emerald-900/50 text-emerald-300' : 'bg-ink-800 text-ink-500'}">{tokens[src] ? 'set' : 'not set'}</span>
                <input type="password" class="flex-1 bg-ink-800 border border-steel-700 rounded px-2 py-1 text-ink-100 font-mono" placeholder={ph} bind:value={tokenInput[src]} />
                <button class="btn px-2 py-1 rounded" on:click={() => saveToken(src)}>{tokenInput[src].trim() ? 'Save' : 'Clear'}</button>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/if}

    {#if uploadPct >= 0}
      <div class="rounded-sm border border-cursed-700/50 bg-ink-900 p-4 space-y-1">
        <div class="flex justify-between text-xs font-mono text-ink-300"><span class="truncate">Uploading {uploadName}…</span><span>{uploadPct}%</span></div>
        <div class="h-2 rounded-full bg-ink-800 overflow-hidden"><div class="h-full bg-cursed-500 transition-all duration-150" style="width:{uploadPct}%"></div></div>
        <div class="text-[11px] text-ink-500">Streaming to IPFS — after the upload it's added to the blockstore and announced to the network.</div>
      </div>
    {/if}

    <!-- in-flight fetch/add phases (real progress bar while downloading) -->
    {#each Object.entries(tasks) as [key, t] (key)}
      <div class="rounded-sm border border-ink-800 bg-ink-900/60 px-3 py-2">
        <DownloadProgress task={t} label={key} />
      </div>
    {/each}

    <!-- model grid -->
    {#if !node?.enabled}
      <div class="text-ink-500 text-sm text-center py-10">IPFS is off — flip the switch above to join Model Share.</div>
    {:else if node?.daemon === 'failed'}
      <div class="text-red-300 text-sm text-center py-10">IPFS daemon failed to start — SSH into the Orb and check <code class="font-mono">journalctl -u aeon-ipfs</code>.</div>
    {:else if node?.daemon !== 'active'}
      <div class="text-ink-500 text-sm text-center py-10">Connecting to the IPFS network… (first run downloads kubo, ~30 MB)</div>
    {:else if filtered.length === 0}
      <div class="text-ink-500 text-sm text-center py-10">
        {rows.length ? 'No models match your filter.' : 'No models shared on the network yet — be the first: + Share a model, or import one from HuggingFace.'}
      </div>
    {:else}
      <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
        {#each filtered as row (row.entry.cid)}
          {@const c = row.entry.card ?? {}}
          {@const busy = tasks[row.entry.cid]}
          <div class="rounded-sm border bg-ink-900 p-3.5 flex flex-col gap-2 transition {row.local ? 'border-emerald-500/40' : 'border-steel-700 hover:border-steel-600'}">
            <div class="flex items-start gap-2.5">
              <!-- circular avatar, HuggingFace-style -->
              <button class="shrink-0" on:click={() => (detail = row)}>
                {#if c.image}
                  <img src={imageSrc(row.entry.cid, c.image)} alt="" class="w-11 h-11 rounded-full object-cover border border-steel-700 bg-ink-950" loading="lazy" />
                {:else}
                  <div class="w-11 h-11 rounded-full bg-ink-800 border border-steel-700 flex items-center justify-center text-xl">{KIND_ICON[c.kind || 'other'] ?? '📦'}</div>
                {/if}
              </button>
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-1.5">
                  <button class="font-mono text-sm text-ink-100 truncate hover:text-cursed-300 text-left" on:click={() => (detail = row)} title={row.entry.name}>{row.entry.name}</button>
                  {#if row.entry.verified}<span class="shrink-0 text-cursed-300" title={row.local ? "Every file's SHA-256 matched the hash its source published when it was imported" : "The publisher attests every file matched the hash its source published. That claim is part of what they signed, and this Orb re-checks the signed file list when it downloads."}>✓</span>{/if}
                </div>
                <div class="text-[11px] text-ink-500 font-mono flex flex-wrap gap-x-2">
                  {#if c.params}<span>{c.params}</span>{/if}
                  {#if c.quant}<span>{c.quant}</span>{/if}
                  <span>{fmtBytes(row.entry.size_bytes)}</span>
                </div>
              </div>
              {#if c.kind}<span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-ink-800 text-cursed-300 shrink-0">{c.kind}</span>{/if}
            </div>

            {#if c.description}<div class="text-[11px] text-ink-400 line-clamp-2">{c.description}</div>{/if}
            {#if c.base_model}<div class="text-[10px] text-ink-500 font-mono truncate">base: {c.base_model}</div>{/if}

            <div class="flex flex-wrap gap-1">
              {#each row.hosts.slice(0, 3) as h}
                <span class="text-[10px] font-mono px-1.5 py-0.5 rounded {h.is_self ? 'bg-emerald-900/50 text-emerald-300' : 'bg-ink-800 text-ink-400'}"
                      title={h.is_self ? 'hosted on this Orb' : 'hosted by ' + h.label}>{h.is_self ? 'this orb' : h.label}</span>
              {/each}
              {#if row.hosts.length > 3}<span class="text-[10px] font-mono text-ink-600">+{row.hosts.length - 3}</span>{/if}
            </div>

            <!-- community signals: star (trust) + adoption (orbs hosting) -->
            <div class="flex items-center gap-3 text-[11px] font-mono">
              <button class="flex items-center gap-1 transition-colors {row.starred ? 'text-amber-300' : 'text-ink-500 hover:text-amber-300'}"
                      on:click|stopPropagation={() => toggleStar(row.entry.cid)}
                      title={row.starred ? 'Starred — click to unstar' : 'Star this model (community trust signal)'}>
                <span class="text-sm leading-none">{row.starred ? '★' : '☆'}</span>{row.star_count ?? 0}
              </button>
              <span class="text-ink-500" title="Orbs hosting this model across the network — wider adoption means faster, more resilient downloads">⬡ {row.host_count ?? row.hosts.length} {(row.host_count ?? row.hosts.length) === 1 ? 'orb' : 'orbs'}</span>
              {#if row.origin?.upstream}
                <span class="{STANDING.mirror.cls}" title={originTitle(row)}>{STANDING.mirror.icon} {row.origin.upstream}</span>
              {:else if row.origin?.signed}
                <span class="{standing(row.origin).cls}" title={originTitle(row)}>
                  {standing(row.origin).icon} {row.origin.name || 'anon'}·{row.origin.fp}{#if row.origin.seeder_online}<span class="text-emerald-400" aria-label="original seeder online"> ●</span>{/if}{#if row.origin.earlier_claim}<span class="text-amber-400"> ⚠</span>{/if}
                </span>
              {:else if row.signature && !row.signature.ok}
                <span class="text-red-400" title="Signature does NOT verify — the claimed author is not proven">⚠ {row.signature.name || 'anon'}·{row.signature.fp}</span>
              {:else}
                <span class="text-ink-600" title={STANDING.unsigned.label}>unsigned</span>
              {/if}
            </div>

            <div class="mt-auto pt-1 flex items-center gap-2 text-xs">
              {#if busy}
                {@const isErr = String(busy.phase ?? '').startsWith('error')}
                <div class="flex flex-col gap-1 w-full">
                  <DownloadProgress task={busy} />
                  {#if isErr}
                    <div class="flex items-center gap-3 text-[11px]">
                      <button class="text-cursed-300 hover:text-cursed-200" on:click={() => download(row)}
                              title="Retry — resumes from what's already downloaded">↻ retry</button>
                      <button class="text-ink-500 hover:text-red-400" on:click={() => purgeDownload(row.entry.cid)}
                              title="Purge the partial download, reclaim its disk space, and clear the error">✕ purge</button>
                    </div>
                  {/if}
                </div>
              {:else}
                <!-- Primary action on every model: push it to a connected system. -->
                <button class="btn-primary text-xs py-1 px-2.5 rounded inline-flex items-center gap-1"
                        on:click={() => openPush(row.entry)} title="Push this model to a connected system (DGX / gateway)">⇧ Push to server</button>
                {#if row.local}
                  <button class="ml-auto text-ink-400 hover:text-cursed-300" on:click={() => editModel(row)} title="Edit this model's card, image + README">edit</button>
                  <a href={downloadUrl(row.entry)} class="text-ink-400 hover:text-cursed-300" title="Download the file from your gateway">save</a>
                  <button class="{confirmRemove === row.entry.cid ? 'text-red-400' : 'text-ink-500 hover:text-red-400'}" on:click={() => unshare(row.entry.cid)}>{confirmRemove === row.entry.cid ? 'sure?' : 'unshare'}</button>
                {:else}
                  <button class="ml-auto text-ink-500 hover:text-cursed-300" on:click={() => download(row)} title="Download + host on this Orb">↓ host</button>
                  <button class="text-ink-500 hover:text-cursed-300" on:click={() => (detail = row)}>details</button>
                {/if}
              {/if}
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </main>
</div>

<!-- ── Share / Edit form modal ──────────────────────────────────────────── -->
{#if showShare && (pendingFile || editingCid)}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-40 bg-black/60 flex items-center justify-center p-4" on:click={() => (showShare = false)}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="w-full max-w-lg panel p-5 space-y-3 max-h-[90vh] overflow-y-auto" on:click|stopPropagation>
      <div class="flex items-center justify-between">
        <h2 class="font-mono text-cursed-300">{editingCid ? 'Edit model card' : `Share “${pendingFile?.name}”`}</h2>
        {#if pendingFile}<span class="text-xs text-ink-500 font-mono">{fmtBytes(pendingFile.size)}</span>{/if}
      </div>
      <p class="text-[11px] text-ink-500">
        {#if editingCid}Editing the card, image + README rebuilds the model's IPFS entry — the weights are reused, never re-uploaded.{:else}Fill the model card — it's published inside the IPFS directory alongside the weights so anyone can read it before downloading.{/if}
      </p>

      <!-- who is publishing this -->
      {#if !editingCid}
        <div class="rounded-md border-2 p-3 {pubSigning && signAsMe ? 'border-emerald-500/60 bg-emerald-500/5' : 'border-amber-500/60 bg-amber-500/5'}">
          {#if pubSigning}
            <label class="flex items-start gap-3 cursor-pointer">
              <input type="checkbox" bind:checked={signAsMe} class="mt-1 h-5 w-5 accent-emerald-500 shrink-0" />
              <span class="min-w-0">
                <span class="block font-mono text-base {signAsMe ? 'text-emerald-300' : 'text-ink-200'}">Sign with my publisher identity</span>
                {#if signAsMe}
                  <span class="block text-[12px] text-ink-300 mt-0.5">🔏 Published as <strong class="text-emerald-300">@{pubName}</strong> <span class="font-mono text-ink-500">· {pubFp}</span> — people who download it can verify it came from you.</span>
                {:else}
                  <span class="block text-[12px] text-amber-300 mt-0.5">Your name will NOT be on this model. It will be signed by this Orb's key only ({pubOrbFp}), and cannot be re-attributed to you later.</span>
                {/if}
              </span>
            </label>
          {:else if pubAccounts.length}
            <div class="font-mono text-base text-amber-300">Not signed in — this model will not carry your name</div>
            <div class="text-[12px] text-ink-300 mt-0.5">Sign in to publish it as @{pubAccounts.find((a) => a.pubkey === pubSel)?.username ?? pubAccounts[0].username}. You only do this once on this Orb.</div>
            <form class="mt-2 flex items-center gap-2" on:submit|preventDefault={quickSignIn}>
              <input bind:value={quickPass} type="password" autocomplete="current-password" placeholder="identity password"
                     class="flex-1 min-w-0 bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" />
              <button type="submit" class="btn-primary text-sm px-4 py-2 rounded disabled:opacity-50" disabled={pubBusy || !quickPass}>Sign in</button>
            </form>
          {:else}
            <div class="font-mono text-base text-amber-300">This model will be published without your name</div>
            <div class="text-[12px] text-ink-300 mt-0.5">Create a publisher identity to sign what you share — it takes a minute, and you can do it right now without losing this form.</div>
            <button class="btn-primary text-sm px-4 py-2 rounded mt-2" on:click={() => openPublisher('create')}>Create my publisher identity</button>
          {/if}
        </div>
      {/if}

      <!-- image + name row -->
      <div class="flex gap-3">
        <div class="shrink-0">
          <div class="w-24 h-24 rounded-full border border-steel-700 bg-ink-950 overflow-hidden flex items-center justify-center">
            {#if imageB64}<img src={imageB64} alt="" class="w-full h-full object-cover" />
            {:else if existingImageUrl}<img src={existingImageUrl} alt="" class="w-full h-full object-cover" />
            {:else}<span class="text-ink-600 text-3xl">🖼️</span>{/if}
          </div>
          <div class="flex items-center justify-center gap-2 mt-1">
            <button class="text-[11px] text-cursed-300 hover:underline" on:click={() => imageInput?.click()}>{imageB64 || existingImageUrl ? 'replace' : 'add image'}</button>
            {#if imageB64 || existingImageUrl}<button class="text-[11px] text-ink-500 hover:text-red-400" on:click={clearImage}>remove</button>{/if}
          </div>
          <input type="file" accept="image/*" bind:this={imageInput} class="hidden" on:change={onImage} />
        </div>
        <label class="flex-1 block text-xs font-mono text-ink-400">Name
          <input class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.name} placeholder="Qwen3-VL 8B Instruct" />
          <span class="block mt-2">Base model</span>
          <input class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.base_model} placeholder="Qwen/Qwen3-VL-8B" />
        </label>
      </div>

      <div class="grid grid-cols-2 gap-2">
        <label class="block text-xs font-mono text-ink-400">Kind
          <select class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.kind}>
            {#each KINDS.slice(1) as k}<option value={k}>{k}</option>{/each}
          </select></label>
        <label class="block text-xs font-mono text-ink-400">Format
          <input class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.format} placeholder="gguf / safetensors / hef" /></label>
        <label class="block text-xs font-mono text-ink-400">Parameters
          <input class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.params} placeholder="8B" /></label>
        <label class="block text-xs font-mono text-ink-400">Quantization
          <input class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.quant} placeholder="int4 / q4_k_m" /></label>
      </div>
      <label class="block text-xs font-mono text-ink-400">Description
        <textarea rows="2" class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.description} placeholder="What it is, how it was trained/tuned, notable strengths."></textarea></label>
      <label class="block text-xs font-mono text-ink-400">Intended use
        <input class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.intended_use} placeholder="GUI grounding for agent computer-use" /></label>
      <div class="grid grid-cols-2 gap-2">
        <label class="block text-xs font-mono text-ink-400">License
          <input class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.license} placeholder="apache-2.0" /></label>
        <label class="block text-xs font-mono text-ink-400">Tags (comma-sep)
          <input class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm" bind:value={form.tags} placeholder="grounding, agent, vision" /></label>
      </div>
      <label class="block text-xs font-mono text-ink-400">README (markdown)
        <textarea rows="5" class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm font-mono" bind:value={form.readme} placeholder="# Usage&#10;How to run it, prompt format, benchmarks, credits…"></textarea></label>

      <div class="flex justify-end gap-2 pt-1">
        <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => (showShare = false)}>Cancel</button>
        <button class="btn-primary text-sm px-5 py-2 rounded disabled:opacity-50" on:click={submitShare} disabled={saving || uploadPct >= 0}>
          {#if editingCid}{saving ? 'Saving…' : 'Save changes'}
          {:else if pubSigning && signAsMe}🔏 Publish as @{pubName}
          {:else}Publish without my name{/if}
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- ── Detail / model-card modal ────────────────────────────────────────── -->
{#if detail}
  {@const c = detail.entry.card ?? {}}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-40 bg-black/60 flex items-center justify-center p-4" on:click={() => (detail = null)}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="w-full max-w-lg panel p-5 space-y-3 max-h-[90vh] overflow-y-auto" on:click|stopPropagation>
      <div class="flex items-start gap-4">
        {#if c.image}
          <img src={imageSrc(detail.entry.cid, c.image)} alt="" class="w-20 h-20 rounded-full object-cover border border-steel-700 bg-ink-950 shrink-0" />
        {:else}
          <div class="w-20 h-20 rounded-full bg-ink-800 border border-steel-700 flex items-center justify-center text-3xl shrink-0">{KIND_ICON[c.kind || 'other'] ?? '📦'}</div>
        {/if}
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <div class="font-mono text-lg text-ink-100 truncate">{detail.entry.name}</div>
            {#if detail.entry.verified}
              <span class="shrink-0 text-[10px] font-mono px-1.5 py-0.5 rounded bg-cursed-500/15 text-cursed-300 border border-cursed-500/40" title={detail.local ? "Every file's SHA-256 matched the hash the source published" : "The publisher attests every file matched the hash the source published"}>✓ {detail.local ? 'verified' : 'attested'}</span>
            {/if}
            {#if c.nsfw}
              <span class="shrink-0 text-[10px] font-mono px-1.5 py-0.5 rounded bg-red-900/50 text-red-300 border border-red-700/50">18+</span>
            {/if}
          </div>
          <div class="text-xs text-ink-500 font-mono">{fmtBytes(detail.entry.size_bytes)} · {c.kind || 'model'}{c.format ? ' · ' + c.format : ''}</div>
          {#if detail.entry.source}
            <a href={detail.entry.source} target="_blank" rel="noreferrer" class="text-xs text-cursed-300 hover:underline font-mono break-all">{detail.entry.source.replace('https://', '')}</a>
          {/if}
        </div>
        {#if detail.local}<button class="text-xs text-cursed-300 hover:underline shrink-0" on:click={() => detail && editModel(detail)}>edit</button>{/if}
      </div>
      {#if c.description}<p class="text-sm text-ink-300 leading-relaxed">{c.description}</p>{/if}
      {#if c.gallery?.length && (!c.nsfw || matureOk)}
        <div class="flex gap-2 overflow-x-auto pb-1 -mx-1 px-1">
          {#each c.gallery as g}
            <img src={imageSrc(detail.entry.cid, g)} alt="example generation" loading="lazy" class="h-32 rounded-sm border border-steel-700 object-cover shrink-0" />
          {/each}
        </div>
      {/if}
      <div class="grid grid-cols-2 gap-x-4 gap-y-1.5 text-xs font-mono">
        {#if c.base_model}<div class="text-ink-500">base model</div><div class="text-ink-200 text-right truncate">{c.base_model}</div>{/if}
        {#if c.params}<div class="text-ink-500">parameters</div><div class="text-ink-200 text-right">{c.params}</div>{/if}
        {#if c.quant}<div class="text-ink-500">quantization</div><div class="text-ink-200 text-right">{c.quant}</div>{/if}
        {#if c.license}<div class="text-ink-500">license</div><div class="text-ink-200 text-right">{c.license}</div>{/if}
        {#if c.intended_use}<div class="text-ink-500">intended use</div><div class="text-ink-200 text-right">{c.intended_use}</div>{/if}
        {#if detail.entry.origin_label}<div class="text-ink-500">shared by</div><div class="text-ink-200 text-right truncate">{detail.entry.origin_label}</div>{/if}
        <div class="text-ink-500">sha256</div><div class="text-ink-200 text-right truncate" title={detail.entry.sha256}>{(detail.entry.sha256 ?? '').slice(0, 16)}…</div>
      </div>
      <!-- origin: who first published this, and what this Orb knows about them -->
      {#if detail.origin}
        {@const o = detail.origin}
        <div class="rounded border border-ink-800 bg-ink-950/60 p-3 space-y-2 text-[12px] font-mono">
          <div class="flex items-center justify-between gap-3">
            <div class="min-w-0">
              <div class="text-ink-500 text-[10px] uppercase tracking-wide">origin</div>
              {#if o.upstream}
                <div class="{STANDING.mirror.cls} truncate">{STANDING.mirror.icon} {o.upstream}<span class="text-ink-500"> · vendor release, mirrored</span></div>
              {:else if o.signed}
                <div class="{standing(o).cls} truncate">{standing(o).icon} {o.name || 'anon'}<span class="text-ink-500"> · {o.fp}</span></div>
              {:else}
                <div class="text-ink-400">unsigned</div>
              {/if}
            </div>
            {#if !o.signed && detail.local}
              <button class="shrink-0 px-2 py-1 rounded border border-cursed-500/40 text-cursed-300 hover:bg-cursed-500/10"
                      title="If you shared this model from this Orb, sign it so the network can tell who published it"
                      on:click={() => detail && claimModel(detail.entry.cid)}>Sign as mine</button>
            {/if}
            {#if o.signed && !o.upstream}
              <div class="shrink-0 text-right" title="Worked out on this Orb. Your own trust decision counts most, then downloads this Orb verified itself, then how many Orbs mirror the publisher's models; stars count least because they are the easiest to fake.">
                <div class="text-ink-500 text-[10px] uppercase tracking-wide">reputation</div>
                <div class="text-ink-100">{o.reputation}<span class="text-ink-600">/100</span></div>
              </div>
            {/if}
          </div>
          <ul class="text-ink-400 space-y-0.5 leading-relaxed">
            <li>{standing(o).label}</li>
            {#if o.signed && !o.upstream}
              <li>{o.seeder_online ? '● the original seeder is online now' : '○ the original seeder is not online — served by mirrors'}</li>
              {#if o.delegated}<li>signed by an Orb on this publisher's behalf</li>{/if}
            {/if}
            {#if o.content}
              <li class={o.content.files_ok ? 'text-emerald-300/80' : 'text-ink-400'}>
                {#if o.content.origin_ok}✓ files matched the publisher's signed manifest on this Orb
                {:else if o.content.upstream}✓ files match the manifest · unmodified mirror of {o.content.upstream}
                {:else if o.content.files_ok}✓ file hash matched what was advertised (older card, no signed manifest)
                {:else}not yet checked against a manifest{/if}
              </li>
            {:else if !detail.local}
              <li>not downloaded here yet — the files are checked against the signed manifest when you download</li>
            {/if}
            {#if o.earlier_claim}
              <li class="text-amber-300">⚠ the same weights were first seen on this Orb from a different key ({o.earlier_claim.fp}) — this may be a re-publication</li>
            {/if}
          </ul>
          {#if o.signed && !o.upstream}
            <div class="flex flex-wrap gap-2 pt-1">
              {#if o.standing === 'trusted'}
                <button class="px-2 py-1 rounded border border-ink-700 text-ink-300 hover:text-ink-100" on:click={() => setTrust(o.key, { trusted: false })}>Stop trusting</button>
              {:else}
                <button class="px-2 py-1 rounded border border-emerald-500/40 text-emerald-300 hover:bg-emerald-500/10" on:click={() => setTrust(o.key, { trusted: true })}>Trust this publisher</button>
              {/if}
              {#if o.standing === 'blocked'}
                <button class="px-2 py-1 rounded border border-ink-700 text-ink-300 hover:text-ink-100" on:click={() => setTrust(o.key, { blocked: false })}>Unblock</button>
              {:else}
                <button class="px-2 py-1 rounded border border-red-500/40 text-red-300 hover:bg-red-500/10" on:click={() => setTrust(o.key, { blocked: true })}>Block</button>
              {/if}
            </div>
          {/if}
        </div>
      {/if}
      {#if c.tags?.length}
        <div class="flex flex-wrap gap-1">
          {#each c.tags as t}<span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-ink-800 text-ink-300">{t}</span>{/each}
        </div>
      {/if}
      {#if c.readme}
        <div class="border-t border-ink-800 pt-3">
          <div class="text-[11px] font-mono text-ink-500 uppercase tracking-wider mb-1">README</div>
          {#if detailReadme}
            <div class="text-sm text-ink-300 leading-relaxed">{@html renderMd(detailReadme)}</div>
          {:else}
            <div class="text-xs text-ink-600">loading…</div>
          {/if}
        </div>
      {/if}
      <div class="text-[11px] text-ink-500 font-mono">
        hosts: {detail.hosts.map((h) => (h.is_self ? 'this orb' : h.label)).join(', ')}
      </div>
      <div class="text-[10px] text-ink-600 font-mono break-all">CID {detail.entry.cid}</div>

      <div class="flex justify-end gap-2 pt-1 border-t border-ink-800">
        <button class="btn-primary text-sm px-3 py-1.5 rounded mr-auto inline-flex items-center gap-1"
                on:click={() => detail && openPush(detail.entry)}>⇧ Push to server</button>
        <a href={downloadUrl(detail.entry)} class="btn text-sm px-3 py-1.5 rounded">Save file</a>
        {#if detail.local}
          <span class="self-center text-emerald-400 font-mono text-xs">✓ hosted here</span>
        {:else if tasks[detail.entry.cid]}
          <div class="flex-1 min-w-0 self-center"><DownloadProgress task={tasks[detail.entry.cid]} /></div>
        {:else}
          <button class="btn-primary text-sm px-4 py-1.5 rounded" on:click={() => detail && download(detail)}>↓ Download + host</button>
        {/if}
      </div>
    </div>
  </div>
{/if}

<!-- ── Mature-content age gate (18+ attestation before showing NSFW models) ── -->
{#if showAgeGate}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4" on:click={() => (showAgeGate = false)}>
    <div class="bg-ink-900 border border-red-800/60 rounded-sm w-full max-w-md p-5 space-y-4" on:click|stopPropagation>
      <h2 class="font-mono text-red-300 flex items-center gap-2">🔞 View mature content</h2>
      <p class="text-sm text-ink-300 leading-relaxed">Some models on the network (e.g. Civitai content flagged mature/“red”) and their example images are adult in nature. To view them you must confirm the following.</p>
      <label class="flex items-start gap-2 text-sm text-ink-200">
        <input type="checkbox" class="mt-0.5" bind:checked={ageAttest18} />
        <span>I am <span class="text-red-300">18 years of age or older</span>, and it is legal for me to view adult content where I live.</span>
      </label>
      <label class="flex items-start gap-2 text-sm text-ink-200">
        <input type="checkbox" class="mt-0.5" bind:checked={ageAccept} />
        <span>I understand this content is contributed by third parties, is not moderated by this device, and I view it at my own discretion and responsibility.</span>
      </label>
      <div class="flex justify-end gap-2 pt-1">
        <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => (showAgeGate = false)}>Cancel</button>
        <button class="text-sm px-4 py-1.5 rounded-sm font-mono {ageAttest18 && ageAccept ? 'bg-red-700 hover:bg-red-600 text-white' : 'bg-ink-800 text-ink-500 cursor-not-allowed'}"
                disabled={!(ageAttest18 && ageAccept)} on:click={() => setMature(true)}>Proceed</button>
      </div>
    </div>
  </div>
{/if}

<!-- ── Push-to-server dialog: pick a connected system + destination folder ── -->
{#if pushEntry}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-50 bg-black/60 flex items-center justify-center p-4" on:click={closePush}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="w-full max-w-md panel p-5 space-y-4 max-h-[90vh] overflow-y-auto" on:click|stopPropagation>
      <div class="flex items-center justify-between gap-2">
        <h2 class="font-mono text-cursed-300 text-sm truncate">⇧ Push “{pushEntry.name}”</h2>
        <button class="text-ink-500 hover:text-ink-200 shrink-0" on:click={closePush}>✕</button>
      </div>

      {#if !systems.length}
        <div class="text-center py-4 space-y-3">
          <p class="text-sm text-ink-300">Pushing a model needs a server to push it to — a DGX, an agent
            gateway, or any box you've SSH-linked. You don't have any connected yet.</p>
          <a href="/agent" class="inline-block bg-cursed-600 hover:bg-cursed-500 text-white font-mono text-sm px-4 py-2 rounded-sm transition-colors">↳ Connect your first system</a>
          <p class="text-xs text-ink-500">Set it up once in the Agent Dashboard, then come back and push.</p>
        </div>
        <div class="flex justify-end"><button class="btn text-sm px-3 py-1.5 rounded" on:click={closePush}>Close</button></div>
      {:else}
        <!-- 1. target system -->
        <label class="block text-xs font-mono text-ink-400">Target system
          <select bind:value={pushSel} class="mt-1 w-full bg-ink-800 border border-steel-600 rounded px-2 py-1.5 text-ink-100 text-sm">
            {#each systems as s}
              <option value={s.id}>{s.label || s.address}{s.roles?.length ? ' · ' + s.roles.join('/') : ''}</option>
            {/each}
          </select>
        </label>

        <!-- 2. destination -->
        <div class="space-y-1.5">
          <div class="text-xs font-mono text-ink-400">Destination folder</div>
          <label class="flex items-center gap-2 text-sm text-ink-200 cursor-pointer">
            <input type="radio" bind:group={pushMode} value="home" /> Home folder <code class="text-ink-500 text-xs">~/aeon-models/</code>
          </label>
          <label class="flex items-center gap-2 text-sm text-ink-200 cursor-pointer">
            <input type="radio" bind:group={pushMode} value="custom" on:change={startBrowse} /> Browse the target…
          </label>
        </div>

        <!-- remote folder browser -->
        {#if pushMode === 'custom'}
          <div class="rounded border border-steel-700 bg-ink-950/50 p-2 space-y-1.5">
            <div class="flex items-center gap-2 text-[11px] font-mono">
              <button class="text-cursed-300 hover:text-cursed-200 disabled:opacity-40" on:click={() => browseTo(browseParent)} disabled={!browseParent || browseLoading} title="Up one level">⬆</button>
              <span class="truncate flex-1 text-ink-300" title={browsePath}>{browsePath || '…'}</span>
              {#if browseLoading}<span class="text-ink-500">…</span>{/if}
            </div>
            {#if browseErr}<div class="text-[11px] text-red-400 font-mono">{browseErr}</div>{/if}
            <div class="max-h-40 overflow-y-auto space-y-0.5">
              {#each browseDirs as d}
                <button class="w-full text-left text-xs font-mono text-ink-300 hover:text-cursed-300 hover:bg-ink-800 rounded px-2 py-1 truncate"
                        on:click={() => browseTo(browsePath.replace(/\/$/, '') + '/' + d)}>📁 {d}</button>
              {:else}
                {#if !browseLoading && !browseErr}<div class="text-[11px] text-ink-600 px-2 py-1">no sub-folders here</div>{/if}
              {/each}
            </div>
            {#if browsePath}
              <div class="text-[10px] text-ink-600 leading-snug break-all">Lands in <code class="text-ink-500">{browsePath.replace(/\/$/, '')}/{slugify(pushEntry.name)}</code></div>
            {/if}
          </div>
        {/if}

        {#if pushMsg}
          <div class="text-xs font-mono {pushMsg.startsWith('error') ? 'text-red-400' : pushMsg.startsWith('✓') ? 'text-emerald-400' : 'text-amber-300'} break-all">{pushMsg}</div>
        {/if}

        <div class="flex justify-end gap-2 pt-1">
          <button class="btn text-sm px-3 py-1.5 rounded" on:click={closePush}>Close</button>
          <button class="btn-primary text-sm px-4 py-1.5 rounded disabled:opacity-50"
                  on:click={doPush} disabled={pushActive || !pushSel || (pushMode === 'custom' && !browsePath)}>
            {pushActive ? 'pushing…' : '⇧ Push'}
          </button>
        </div>
        <p class="text-[10px] text-ink-600 leading-snug">Pulls the model down to this Orb (if it isn't already), then rsyncs it to the chosen system over its SSH key.</p>
      {/if}
    </div>
  </div>
{/if}

<!-- ── Publisher identity: guided create, restore, sign in ─────────────────── -->
{#if showPub}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4" on:click={closePublisher}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="bg-ink-900 border border-cursed-700/50 rounded-sm w-full max-w-lg p-5 space-y-4 max-h-[92vh] overflow-y-auto" on:click|stopPropagation>
      {#if pubMode === 'create'}
        <!-- progress -->
        <ol class="flex items-center gap-2 text-[11px] font-mono">
          {#each [['form', '1 · Name it'], ['words', '2 · Write down 24 words'], ['check', '3 · Confirm']] as [k, label], i}
            <li class="px-2 py-1 rounded {pubStep === k ? 'bg-cursed-500/20 text-cursed-200 border border-cursed-500/50' : 'text-ink-500 border border-ink-800'}">{label}</li>
          {/each}
        </ol>

        {#if pubStep === 'form'}
          <h2 class="font-mono text-lg text-cursed-300">Create your publisher identity</h2>
          <p class="text-[13px] text-ink-300 leading-relaxed">
            This makes a signing key that belongs only to you. There is no email and no account on a server:
            the key <em>is</em> the identity, and a 24-word recovery phrase is the only backup of it.
          </p>
          <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Name to publish under</span>
            <input bind:value={pubUser} placeholder="e.g. mageworks" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" />
            <span class="block text-[11px] text-ink-500">Shown next to your models. Others can use the same name — your key is what makes it yours.</span></label>
          <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Password ({PASS_MIN} characters or more)</span>
            <input bind:value={pubPass} type="password" autocomplete="new-password" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" />
            <span class="block text-[11px] text-ink-500">Locks the key on this Orb. You need it to sign in here and to create proofs. A long phrase is best.</span></label>
          <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Password again</span>
            <input bind:value={pubPass2} type="password" autocomplete="new-password" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" />
            {#if pubPass2 && pubPass !== pubPass2}<span class="block text-[11px] text-red-400">does not match</span>{/if}</label>

          <!-- where the key's randomness comes from -->
          <div class="rounded border border-ink-800 bg-ink-950/60 p-3 text-[12px] space-y-2">
            {#if pubEntropy?.verdict === 'not_ready'}
              <div class="text-red-300">⚠ {pubEntropy.summary}</div>
            {:else}
              <div class="text-ink-300">
                <span class="{pubEntropy?.hardware_ok || hasPersonal ? 'text-emerald-300' : 'text-amber-300'}">●</span>
                Your key will be made from {nameSources(pubEntropy?.sources ?? ['kernel'])}{hasPersonal ? ' + the randomness you add below' : ''}, mixed together.
              </div>
              {#if pubEntropy?.recommend_personal && !hasPersonal}
                <div class="text-amber-300/90">This computer gives the Orb one source of randomness, not two. Adding your own below is recommended.</div>
              {/if}
            {/if}

            {#if !showExtra}
              <button class="text-[11px] font-mono text-cursed-400 hover:text-cursed-300" on:click={() => (showExtra = true)}>＋ add my own randomness — scribble or roll dice (optional)</button>
            {:else}
              <div class="flex items-center gap-2 text-[11px] font-mono pt-1">
                <button class="px-2.5 py-1 rounded border {entropyTab === 'scribble' ? 'border-cursed-500/60 bg-cursed-500/15 text-cursed-200' : 'border-ink-800 text-ink-400'}" on:click={() => (entropyTab = 'scribble')}>✍️ Scribble{scribbleUsed ? ' ✓' : ''}</button>
                <button class="px-2.5 py-1 rounded border {entropyTab === 'dice' ? 'border-cursed-500/60 bg-cursed-500/15 text-cursed-200' : 'border-ink-800 text-ink-400'}" on:click={() => (entropyTab = 'dice')}>🎲 Roll dice{dice.rolls.length ? ` ✓ ${dice.rolls.length}` : ''}</button>
                <span class="text-ink-600">use either, or both</span>
              </div>

              {#if entropyTab === 'scribble'}
                <p class="text-ink-300 leading-relaxed">
                  Scribble in the box with your mouse or finger. <strong class="text-ink-100">Keep changing it</strong> — the direction,
                  the size and the speed: big sweeps, small tight turns, zigzags, crossing over.
                  <strong class="text-ink-100">Do not repeat the same motion.</strong> Going round the same circle or back and forth
                  on the same line is predictable, and the meter will stop moving.
                </p>
                <!-- svelte-ignore a11y-no-static-element-interactions -->
                <canvas bind:this={scribbleCanvas} width="960" height="320"
                        class="w-full h-40 rounded border border-steel-700 bg-ink-950 cursor-crosshair touch-none select-none"
                        on:pointerdown|preventDefault={scribbleStart} on:pointermove={scribbleMove}
                        on:pointerup={scribbleEnd} on:pointercancel={scribbleEnd} on:pointerleave={scribbleEnd}></canvas>
                <div class="flex items-center gap-2">
                  <div class="flex-1 h-2 rounded-full bg-ink-800 overflow-hidden">
                    <div class="h-full transition-all duration-150 {scribbleProgress >= 1 ? 'bg-emerald-500' : scribbleStale ? 'bg-amber-500' : 'bg-cursed-500'}" style="width:{Math.round(scribbleProgress * 100)}%"></div>
                  </div>
                  <span class="w-40 text-right font-mono text-[11px] {scribbleProgress >= 1 ? 'text-emerald-300' : scribbleStale ? 'text-amber-300' : 'text-ink-400'}">
                    {scribbleProgress >= 1 ? 'plenty — thank you' : scribbleStale ? 'same motion — change it' : scribbleUsed ? 'keep varying it' : 'start scribbling'}
                  </span>
                  <button class="text-[11px] font-mono text-ink-500 hover:text-ink-300" on:click={scribbleClear}>clear</button>
                </div>
                {#if scribbleStale}
                  <p class="text-amber-300">That is the same movement again. Change direction, make it bigger or smaller, move to another part of the box.</p>
                {/if}
              {:else}
                <p class="text-ink-300 leading-relaxed">
                  Roll <strong class="text-ink-100">real, physical dice</strong> and type each result (1 to 6) as it lands.
                  <strong class="text-ink-100">Do not use a dice app, a website, a voice assistant, or numbers from your head</strong> —
                  a simulated roll comes from a computer's generator (the very thing you are adding to), and people choosing
                  "random" numbers are far more predictable than they feel.
                </p>
                <textarea bind:value={diceText} rows="3" inputmode="numeric" spellcheck="false" autocomplete="off"
                          placeholder="e.g. 3 5 1 6 2 4 4 1 …"
                          class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono tracking-widest focus:border-cursed-500 outline-none resize-none"></textarea>
                <div class="font-mono text-[12px] {dice.rolls.length >= 99 ? 'text-emerald-300' : dice.rolls.length >= 50 ? 'text-emerald-300/80' : 'text-ink-300'}">
                  {dice.rolls.length} roll{dice.rolls.length === 1 ? '' : 's'} ≈ {dice.bits} bits
                </div>
                <div class="font-mono text-[11px] text-ink-500">50 rolls ≈ 128 bits · 99 rolls ≈ 256 bits, the key's full strength</div>
                {#if dice.ignored}<p class="text-ink-500">{dice.ignored} character{dice.ignored === 1 ? '' : 's'} that are not 1–6 were ignored.</p>{/if}
                {#if dice.warning}<p class="text-amber-300">⚠ {dice.warning}</p>{/if}
              {/if}
              <p class="text-[11px] text-ink-500">
                Whatever you add is mixed with the Orb's own sources. It can only make the key harder to guess — it never replaces
                them, any amount helps, and you do not need to remember or keep it.
              </p>
            {/if}
          </div>

          <div class="flex items-center justify-between pt-1">
            <button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => openPublisher('restore')}>I already have a recovery phrase</button>
            <button class="btn-primary text-sm px-5 py-2 rounded disabled:opacity-50" on:click={pubCreate} disabled={pubBusy || pubEntropy?.verdict === 'not_ready'}>Create identity</button>
          </div>

        {:else if pubStep === 'words'}
          <h2 class="font-mono text-lg text-cursed-300">Write down your recovery phrase</h2>
          <p class="text-[13px] text-ink-200 leading-relaxed">
            These 24 words <strong>are</strong> your identity. Anyone who has them can publish as you, and if you lose them
            and this Orb, nobody — not us, not anyone — can get your identity back. <strong>They are shown this once.</strong>
          </p>
          {#if madeFrom.length}
            <p class="text-[11px] font-mono text-ink-500">Made from {nameSources(madeFrom)}, mixed together.</p>
          {/if}
          <div class="grid grid-cols-3 gap-1.5 text-[13px] font-mono bg-ink-950 border border-cursed-700/40 rounded p-3 select-all">
            {#each seedWords as w, i}
              <div class="flex gap-1.5"><span class="text-ink-400 w-5 text-right select-none">{i + 1}</span><span class="text-ink-100">{w}</span></div>
            {/each}
          </div>
          <div class="grid sm:grid-cols-2 gap-3 text-[12px] leading-relaxed">
            <div class="rounded border border-emerald-500/30 bg-emerald-500/5 p-3">
              <div class="font-mono text-emerald-300 mb-1">Do</div>
              <ul class="list-disc ml-4 space-y-1 text-ink-200">
                <li>Write them on paper, in order, with their numbers.</li>
                <li>Make two copies and keep them in different places you trust.</li>
                <li>A password manager's secure note is acceptable, if you already rely on one.</li>
                <li>Check your writing against the screen before you continue.</li>
              </ul>
            </div>
            <div class="rounded border border-red-500/30 bg-red-500/5 p-3">
              <div class="font-mono text-red-300 mb-1">Do not</div>
              <ul class="list-disc ml-4 space-y-1 text-ink-200">
                <li>Take a screenshot or a photo of this screen.</li>
                <li>Paste them into chat, email, notes or a cloud document.</li>
                <li>Type them into any website or app other than an Orb's restore screen.</li>
                <li>Give them to anyone who says they need them to "verify" you.</li>
              </ul>
            </div>
          </div>
          <label class="flex items-start gap-2 text-[13px] text-ink-200 cursor-pointer">
            <input type="checkbox" bind:checked={seedAck} class="mt-0.5 h-4 w-4 accent-cursed-500" />
            <span>I have written all 24 words down, in order, and checked them.</span>
          </label>
          <div class="flex justify-end">
            <button class="btn-primary text-sm px-5 py-2 rounded disabled:opacity-40" on:click={startSeedCheck} disabled={!seedAck}>Continue</button>
          </div>

        {:else}
          <h2 class="font-mono text-lg text-cursed-300">Confirm your backup</h2>
          <p class="text-[13px] text-ink-300 leading-relaxed">Type these three words from what you wrote down. This is the only check that your backup works.</p>
          <div class="space-y-2">
            {#each seedCheckIdx as idx, n}
              <label class="flex items-center gap-3"><span class="w-20 text-right text-[12px] font-mono text-ink-400">word {idx + 1}</span>
                <input bind:value={seedCheckVals[n]} autocomplete="off" autocapitalize="none" spellcheck="false"
                       class="flex-1 bg-ink-950 border rounded px-2.5 py-2 text-sm font-mono outline-none
                              {(seedCheckVals[n] ?? '').trim().toLowerCase() === seedWords[idx] ? 'border-emerald-500/60' : 'border-steel-700 focus:border-cursed-500'}" />
              </label>
            {/each}
          </div>
          <div class="flex items-center justify-between pt-1">
            <button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => (pubStep = 'words')}>← show the words again</button>
            <button class="btn-primary text-sm px-5 py-2 rounded disabled:opacity-40" on:click={finishCreate} disabled={!seedCheckOk}>Finish — start signing</button>
          </div>
        {/if}

      {:else if pubMode === 'restore'}
        <h2 class="font-mono text-lg text-cursed-300">Restore your identity on this Orb</h2>
        <p class="text-[13px] text-ink-300 leading-relaxed">Enter the <span class="text-cursed-300">24-word recovery phrase</span>. Your public key and reputation come back exactly as before — the key is made from the phrase. The password you set here locks it on this Orb and can differ from the one you used elsewhere.</p>
        <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Name to publish under</span>
          <input bind:value={pubUser} placeholder="e.g. mageworks" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
        <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Recovery phrase — 24 words</span>
          <textarea bind:value={seedInput} rows="3" spellcheck="false" autocomplete="off" placeholder="word1 word2 word3 … word24"
                    class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none resize-none"></textarea>
          <span class="text-[11px] font-mono {seedInput.trim().split(/\s+/).filter(Boolean).length === 24 ? 'text-emerald-400' : 'text-ink-600'}">{seedInput.trim() ? seedInput.trim().split(/\s+/).filter(Boolean).length : 0}/24 words</span></label>
        <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">New password for this Orb ({PASS_MIN} characters or more)</span>
          <input bind:value={pubPass} type="password" autocomplete="new-password" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
        <div class="flex items-center justify-between pt-1">
          <div class="flex items-center gap-3">
            <button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => openPublisher('create')}>create a new one instead</button>
            {#if pubAccounts.length}<button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => openPublisher('unlock')}>sign in →</button>{/if}
          </div>
          <button class="btn-primary text-sm px-5 py-2 rounded disabled:opacity-50" on:click={pubRestore} disabled={pubBusy}>Restore and sign in</button>
        </div>

      {:else}
        <h2 class="font-mono text-lg text-cursed-300">Sign in to your publisher identity</h2>
        <p class="text-[13px] text-ink-300 leading-relaxed">You do this once. This Orb then keeps signing as you — including after restarts — until you sign out.</p>
        <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Identity</span>
          <select bind:value={pubSel} class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none">
            {#each pubAccounts as a}<option value={a.pubkey}>@{a.username} · {a.fingerprint}</option>{/each}
          </select></label>
        <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Password</span>
          <input bind:value={pubPass} type="password" autocomplete="current-password" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
        <div class="flex items-center justify-between pt-1">
          <div class="flex items-center gap-3">
            <button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => openPublisher('create')}>create a new one</button>
            <button class="text-[12px] text-cursed-400 hover:text-cursed-300 font-mono" on:click={() => openPublisher('restore')}>restore from phrase</button>
          </div>
          <button class="btn-primary text-sm px-5 py-2 rounded disabled:opacity-50" on:click={pubUnlock} disabled={pubBusy}>Sign in</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

<!-- ── Proofs: link the publisher key to accounts on other platforms ────────── -->
{#if showProof}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4" on:click={() => (showProof = false)}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="bg-ink-900 border border-cursed-700/50 rounded-sm w-full max-w-lg p-5 space-y-4 max-h-[92vh] overflow-y-auto" on:click|stopPropagation>
      <div class="flex items-center gap-2 text-[12px] font-mono">
        <button class="px-3 py-1.5 rounded border {proofTab === 'make' ? 'border-cursed-500/60 bg-cursed-500/15 text-cursed-200' : 'border-ink-800 text-ink-400'}" on:click={() => (proofTab = 'make')}>Prove an account is mine</button>
        <button class="px-3 py-1.5 rounded border {proofTab === 'check' ? 'border-cursed-500/60 bg-cursed-500/15 text-cursed-200' : 'border-ink-800 text-ink-400'}" on:click={() => (proofTab = 'check')}>Check someone's proof</button>
      </div>

      {#if proofTab === 'make'}
        <p class="text-[13px] text-ink-300 leading-relaxed">
          Your public key can be shared freely, but a key alone does not show it belongs to the person behind an account.
          A proof is a short statement signed with your key. Post it <em>on</em> the account, and anyone can check both: the
          signature shows the key holder wrote it, and where it is posted shows the account owner did.
        </p>
        {#if !proofText}
          <div class="grid grid-cols-5 gap-2">
            <label class="col-span-2 block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Where</span>
              <select bind:value={proofSite} class="w-full bg-ink-950 border border-steel-700 rounded px-2 py-2 text-sm font-mono">
                {#each PROOF_SITES as x}<option value={x.v}>{x.label}</option>{/each}
              </select></label>
            <label class="col-span-3 block space-y-1"><span class="text-[12px] text-ink-400 font-mono">{proofSite ? 'Your account name there' : 'Address you control'}</span>
              <input bind:value={proofHandle} placeholder={proofSite ? 'your-name' : 'example.org/about'} autocapitalize="none" spellcheck="false"
                     class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
          </div>
          {#if proofHandle.trim()}<div class="text-[12px] font-mono text-ink-400">The proof will say: <span class="text-ink-100">@{pubName} is {proofClaim}</span></div>{/if}
          <label class="block space-y-1"><span class="text-[12px] text-ink-400 font-mono">Identity password</span>
            <input bind:value={proofPass} type="password" autocomplete="current-password" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" />
            <span class="block text-[11px] text-ink-500">A proof is signed with your own key, not the Orb's, so it needs your password.</span></label>
          <div class="flex justify-end gap-2">
            <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => (showProof = false)}>Cancel</button>
            <button class="btn-primary text-sm px-5 py-2 rounded disabled:opacity-50" on:click={makeProof} disabled={pubBusy}>Create proof</button>
          </div>
        {:else}
          <div class="text-[13px] text-emerald-300 font-mono">Proof created. Now post it.</div>
          <pre class="text-[11px] font-mono bg-ink-950 border border-ink-800 rounded p-3 whitespace-pre-wrap break-all select-all">{proofText}</pre>
          <ol class="list-decimal ml-5 text-[12px] text-ink-200 space-y-1 leading-relaxed">
            <li>Copy the whole block, including the BEGIN and END lines.</li>
            <li>Paste it into {proofWhere}.</li>
            <li>Leave it there. Anyone can copy it into "Check someone's proof" on any Orb to verify it.</li>
          </ol>
          <div class="flex justify-end gap-2">
            <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => { proofText = ''; }}>Make another</button>
            <button class="btn-primary text-sm px-5 py-2 rounded" on:click={() => copyText(proofText, 'Proof')}>Copy proof</button>
          </div>
        {/if}
      {:else}
        <p class="text-[13px] text-ink-300 leading-relaxed">Paste a proof you found on someone's profile or page. This checks the signature; you check that it really is posted on the account it names.</p>
        <textarea bind:value={proofCheckText} rows="7" spellcheck="false" placeholder="-----BEGIN AEON IDENTITY PROOF-----&#10;…&#10;-----END AEON IDENTITY PROOF-----"
                  class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-[11px] font-mono focus:border-cursed-500 outline-none"></textarea>
        {#if proofResult}
          {#if proofResult.valid}
            <div class="rounded border border-emerald-500/40 bg-emerald-500/5 p-3 text-[13px] space-y-1">
              <div class="text-emerald-300 font-mono">✓ Genuine — signed by the key it names</div>
              <div class="text-ink-200">The holder of key <code class="font-mono text-ink-100">{proofResult.fingerprint}</code>{proofResult.name ? ` (@${proofResult.name})` : ''} claims <strong class="text-ink-100">{proofResult.claim}</strong>.</div>
              <div class="text-[12px] text-ink-400">{proofResult.is_mine ? 'This is your own identity.' : 'It is only meaningful if you found it posted on that account.'}</div>
              <div class="text-[11px] font-mono text-ink-500 break-all">full key: {proofResult.key}</div>
            </div>
          {:else}
            <div class="rounded border border-red-500/40 bg-red-500/5 p-3 text-[13px] text-red-300">✗ Not valid — {proofResult.why}</div>
          {/if}
        {/if}
        <div class="flex justify-end gap-2">
          <button class="btn text-sm px-3 py-1.5 rounded" on:click={() => (showProof = false)}>Close</button>
          <button class="btn-primary text-sm px-5 py-2 rounded" on:click={checkProof}>Check</button>
        </div>
      {/if}
    </div>
  </div>
{/if}
