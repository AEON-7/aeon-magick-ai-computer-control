<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import StorageManager from '$lib/components/StorageManager.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import DownloadProgress from '$lib/components/DownloadProgress.svelte';
  import { toast } from '$lib/toast';

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
  type Host = { label: string; is_self: boolean; online: boolean; peer_id?: string; addrs?: string[] };
  type Sig = { ok: boolean; name: string; fp: string };
  type Row = { entry: Entry; hosts: Host[]; local: boolean; host_count?: number; star_count?: number; starred?: boolean; signature?: Sig | null };
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

  // ── Publisher identity — an ed25519 keypair that signs the models you share ──
  let pubAccounts: { pubkey: string; username: string; fingerprint: string }[] = [];
  let pubUnlocked = ''; // pubkey of the currently-unlocked identity (signs shares)
  $: pubName = pubAccounts.find((a) => a.pubkey === pubUnlocked)?.username ?? '';
  $: pubFp = pubUnlocked.slice(0, 12);
  let showPub = false;
  let pubMode: 'create' | 'unlock' | 'restore' = 'create';
  let pubUser = '';
  let pubPass = '';
  let pubSel = '';
  let seedWords: string[] = []; // shown ONCE after create — the user must write it down
  let seedInput = ''; // 24-word phrase pasted in when restoring on a new Orb

  async function loadPublisher() {
    try {
      const r = await papi('/accounts');
      if (r?.ok) { pubAccounts = r.accounts ?? []; pubUnlocked = r.unlocked ?? ''; }
    } catch {}
  }
  function openPublisher() {
    pubMode = pubAccounts.length ? 'unlock' : 'create';
    pubSel = pubAccounts[0]?.pubkey ?? '';
    pubUser = ''; pubPass = ''; seedWords = []; seedInput = ''; showPub = true;
  }
  async function pubCreate() {
    if (!pubUser.trim() || pubPass.length < 6) { toast('Pick a username and a password of at least 6 characters.', 'error'); return; }
    const pw = pubPass;
    const r = await papi('/accounts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: pubUser.trim(), password: pw }) });
    if (!r?.ok) { toast('Create failed: ' + (r?.err ?? 'error'), 'error'); return; }
    seedWords = String(r.seed_phrase ?? '').split(/\s+/).filter(Boolean);
    await papi('/unlock', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ pubkey: r.pubkey, password: pw }) });
    await loadPublisher();
    pubPass = ''; // modal stays open to display the seed
  }
  // Restore an existing identity from its 24-word seed on a fresh Orb. The key is
  // DERIVED from the phrase, so the pubkey (and reputation) come back identical;
  // the password just re-encrypts it locally and can differ from the original.
  async function pubRestore() {
    const words = seedInput.trim().split(/\s+/).filter(Boolean);
    if (!pubUser.trim() || pubPass.length < 6) { toast('Enter a display name and a password of at least 6 characters.', 'error'); return; }
    if (words.length !== 24) { toast(`Enter your 24-word recovery phrase (you have ${words.length} word${words.length === 1 ? '' : 's'}).`, 'error'); return; }
    const pw = pubPass;
    const r = await papi('/accounts', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: pubUser.trim(), password: pw, seed_phrase: words.join(' ') }) });
    if (!r?.ok) { toast('Restore failed — ' + (r?.err ?? 'check the recovery phrase'), 'error'); return; }
    await papi('/unlock', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ pubkey: r.pubkey, password: pw }) });
    await loadPublisher();
    showPub = false; pubPass = ''; seedInput = '';
    toast(`Restored @${pubUser.trim()} · ${String(r.pubkey).slice(0, 12)} — signing as your original identity.`, 'success');
  }
  async function pubUnlock() {
    const r = await papi('/unlock', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ pubkey: pubSel, password: pubPass }) });
    if (!r?.ok) { toast('Unlock failed — ' + (r?.err ?? 'wrong password'), 'error'); return; }
    await loadPublisher();
    showPub = false; pubPass = '';
    toast('Identity unlocked — the models you share are now signed.', 'success');
  }
  async function pubLock() { await papi('/lock', { method: 'POST' }); await loadPublisher(); }

  async function load() {
    try {
      node = await api('/status');
      if (node?.daemon === 'active') {
        const r = await api('/models/registry');
        if (r?.ok) {
          rows = r.models ?? [];
          tasks = r.tasks ?? {};
          peerCount = r.peer_count ?? 0;
          selfPeer = r.self_peer_id ?? '';
          myStarCount = r.my_star_count ?? 0;
          err = '';
        }
      }
    } catch (e: any) {
      err = e?.message ?? 'failed to load';
    }
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
        <span class="text-ink-500 text-xs font-mono">{node.peers} swarm peers · {fmtBytes(node.repo_bytes)} /
          <a href="/orbnet/ipfs" class="underline decoration-dotted hover:text-cursed-300"
             title="This is your IPFS storage allocation, not an upload cap — click to adjust how much storage this Orb shares with the network">{node.storage_max}</a></span>
      {/if}
      <div class="flex-1"></div>
      <!-- on/off switch -->
      <button
        class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors {node?.enabled ? 'bg-cursed-600' : 'bg-ink-700'} disabled:opacity-50"
        on:click={toggleIpfs} disabled={toggling} title={node?.enabled ? 'Disable IPFS (leave Model Share)' : 'Enable IPFS (join Model Share)'}>
        <span class="inline-block h-5 w-5 transform rounded-full bg-white transition-transform {node?.enabled ? 'translate-x-5' : 'translate-x-0.5'}"></span>
      </button>
      <span class="text-xs font-mono text-ink-400 w-16">{toggling ? '…' : node?.enabled ? 'enabled' : 'disabled'}</span>
    </div>

    {#if node?.enabled}
      <!-- Storage allocation, external drives + LAN sharing — the same controls
           as the IPFS page, collapsible so the model grid stays front and centre. -->
      <details class="panel/60 group">
        <summary class="cursor-pointer select-none px-4 py-2.5 font-mono text-sm text-cursed-300 flex items-center gap-2 list-none">
          <span class="text-ink-500 transition-transform group-open:rotate-90">▸</span>
          Storage, drives &amp; LAN sharing
        </summary>
        <div class="p-3 pt-0"><StorageManager /></div>
      </details>

      <!-- publisher identity — signs the models you share -->
      <div class="flex items-center gap-2 text-[12px] font-mono">
        {#if pubUnlocked}
          <span class="inline-flex items-center gap-1 text-emerald-300" title="Models you share are signed with this ed25519 identity — the network can verify you published them">🔏 signing as @{pubName}·<span class="text-emerald-400/70">{pubFp}</span></span>
          <button class="text-ink-500 hover:text-red-400" on:click={pubLock}>lock</button>
        {:else}
          <span class="text-ink-500">Shared models are <span class="text-amber-300/80">unsigned</span>.</span>
          <button class="text-cursed-300 hover:text-cursed-200" on:click={openPublisher}>{pubAccounts.length ? '🔑 Unlock a publisher identity' : '＋ Create a publisher identity'} →</button>
        {/if}
      </div>

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
                  {#if row.entry.verified}<span class="shrink-0 text-cursed-300" title="Verified — every file's SHA-256 matches the hash HuggingFace published">✓</span>{/if}
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
              {#if row.signature}
                <span class="{row.signature.ok ? 'text-emerald-300' : 'text-red-400'}"
                      title={row.signature.ok ? `Signed by @${row.signature.name || 'anon'} (key ${row.signature.fp}) — signature verified against this model` : 'Signature does NOT verify — the claimed author is not proven'}>
                  {row.signature.ok ? '🔏' : '⚠'} {row.signature.name || 'anon'}·{row.signature.fp}
                </span>
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
        <button class="btn-primary text-sm px-4 py-1.5 rounded disabled:opacity-50" on:click={submitShare} disabled={saving || uploadPct >= 0}>
          {editingCid ? (saving ? 'Saving…' : 'Save changes') : 'Publish to network'}
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
              <span class="shrink-0 text-[10px] font-mono px-1.5 py-0.5 rounded bg-cursed-500/15 text-cursed-300 border border-cursed-500/40" title="Every file's SHA-256 matches the hash the source published">✓ verified</span>
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

<!-- ── Publisher identity: create / unlock + one-time seed display ────────── -->
{#if showPub}
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
  <div class="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4" on:click={() => (showPub = false)}>
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
    <div class="bg-ink-900 border border-cursed-700/50 rounded-sm w-full max-w-md p-5 space-y-4" on:click|stopPropagation>
      {#if seedWords.length}
        <h2 class="font-mono text-cursed-300 flex items-center gap-2">🔏 Write down your recovery phrase</h2>
        <p class="text-[12px] text-ink-300 leading-relaxed">These 24 words are the <strong>only</strong> way to recover your publisher identity — shown once, never stored in plain text. Write them down and keep them safe (they're also included in the Orb's encrypted config backup).</p>
        <div class="grid grid-cols-3 gap-1.5 text-[12px] font-mono bg-ink-950 border border-ink-800 rounded p-3">
          {#each seedWords as w, i}
            <div class="flex gap-1"><span class="text-ink-600 w-5 text-right">{i + 1}</span><span class="text-ink-100">{w}</span></div>
          {/each}
        </div>
        <div class="flex justify-end">
          <button class="btn-primary text-sm px-4 py-2 rounded" on:click={() => { seedWords = []; showPub = false; }}>I've written it down</button>
        </div>
      {:else if pubMode === 'create'}
        <h2 class="font-mono text-cursed-300">Create a publisher identity</h2>
        <p class="text-[12px] text-ink-400 leading-relaxed">An anonymous <span class="text-cursed-300">ed25519</span> keypair — no email, no PII. The public key <em>is</em> your identity; a 24-word seed is your only backup. Models you share while it's unlocked get signed, so the network can verify you published them.</p>
        <label class="block space-y-1"><span class="text-[11px] text-ink-500 font-mono">Display name (a petname)</span>
          <input bind:value={pubUser} placeholder="e.g. mageworks" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
        <label class="block space-y-1"><span class="text-[11px] text-ink-500 font-mono">Password — encrypts the key (6+ chars)</span>
          <input bind:value={pubPass} type="password" autocomplete="new-password" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
        <div class="flex items-center justify-between pt-1">
          <div class="flex items-center gap-3">
            <button class="text-[12px] text-cursed-400 hover:text-cursed-300 font-mono" on:click={() => (pubMode = 'restore')}>↩ restore a backup</button>
            {#if pubAccounts.length}<button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => (pubMode = 'unlock')}>unlock →</button>{/if}
          </div>
          <button class="btn-primary text-sm px-4 py-2 rounded" on:click={pubCreate}>Create identity</button>
        </div>
      {:else if pubMode === 'restore'}
        <h2 class="font-mono text-cursed-300">Restore signing identity</h2>
        <p class="text-[12px] text-ink-400 leading-relaxed">Recover a publisher identity from its <span class="text-cursed-300">24-word recovery phrase</span> — your public key and reputation come back <em>exactly</em> as before (the key is derived from the phrase). Set a password to encrypt it on this Orb; it can differ from the original.</p>
        <label class="block space-y-1"><span class="text-[11px] text-ink-500 font-mono">Display name (a petname)</span>
          <input bind:value={pubUser} placeholder="e.g. mageworks" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
        <label class="block space-y-1"><span class="text-[11px] text-ink-500 font-mono">Recovery phrase — 24 words</span>
          <textarea bind:value={seedInput} rows="3" spellcheck="false" autocomplete="off" placeholder="word1 word2 word3 … word24"
                    class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none resize-none"></textarea>
          <span class="text-[10px] font-mono {seedInput.trim().split(/\s+/).filter(Boolean).length === 24 ? 'text-emerald-400' : 'text-ink-600'}">{seedInput.trim() ? seedInput.trim().split(/\s+/).filter(Boolean).length : 0}/24 words</span></label>
        <label class="block space-y-1"><span class="text-[11px] text-ink-500 font-mono">New password — encrypts the key here (6+ chars)</span>
          <input bind:value={pubPass} type="password" autocomplete="new-password" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
        <div class="flex items-center justify-between pt-1">
          <div class="flex items-center gap-3">
            <button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => (pubMode = 'create')}>＋ new instead</button>
            {#if pubAccounts.length}<button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => (pubMode = 'unlock')}>unlock →</button>{/if}
          </div>
          <button class="btn-primary text-sm px-4 py-2 rounded" on:click={pubRestore}>Restore identity</button>
        </div>
      {:else}
        <h2 class="font-mono text-cursed-300">Unlock publisher identity</h2>
        <label class="block space-y-1"><span class="text-[11px] text-ink-500 font-mono">Identity</span>
          <select bind:value={pubSel} class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none">
            {#each pubAccounts as a}<option value={a.pubkey}>@{a.username} · {a.fingerprint}</option>{/each}
          </select></label>
        <label class="block space-y-1"><span class="text-[11px] text-ink-500 font-mono">Password</span>
          <input bind:value={pubPass} type="password" autocomplete="current-password" class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-cursed-500 outline-none" /></label>
        <div class="flex items-center justify-between pt-1">
          <div class="flex items-center gap-3">
            <button class="text-[12px] text-ink-500 hover:text-ink-300 font-mono" on:click={() => { pubMode = 'create'; pubUser = ''; pubPass = ''; }}>＋ new</button>
            <button class="text-[12px] text-cursed-400 hover:text-cursed-300 font-mono" on:click={() => { pubMode = 'restore'; pubUser = ''; pubPass = ''; seedInput = ''; }}>↩ restore</button>
          </div>
          <button class="btn-primary text-sm px-4 py-2 rounded" on:click={pubUnlock}>Unlock</button>
        </div>
      {/if}
    </div>
  </div>
{/if}
