<script lang="ts">
  // OrbNet — anonymous Matrix federation over Tor between Aeon Magick Orbs.
  // Activate the homeserver, auto-join the community, chat in interest rooms,
  // spin off groups + DMs, set your own moderation filter. Talks to the
  // supervisor's admin-only /api/orbnet/* surface.
  import { onMount, onDestroy } from 'svelte';
  import qrcode from 'qrcode-generator';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { toast } from '$lib/toast';
  import { confirmRite, askRite } from '$lib/confirm';

  type Status = {
    ok: boolean; enabled: boolean; onion: string; homeserver_up: boolean;
    tor: string; conduit: string; owner: string | null;
    display_name: string; handle: string; auto_join_community: boolean;
    moderation_keywords: string[];
  };
  type Room = { room_id: string; name: string; last_ts: number; members?: number; last_sender?: string; last_body?: string };
  type Msg = { sender: string; body: string; ts: number; event_id: string };

  let status: Status | null = null;
  let rooms: Room[] = [];
  let selected: Room | null = null;
  let messages: Msg[] = [];
  let stats: { rooms: number; members: number; active: number } | null = null;
  let draft = '';
  let err = '';
  let busy = '';
  let enabling = false;
  let enablingTries = 0;
  let copied = '';
  let showConnect = false;
  let clientPw = '';
  let pwBusy = false;
  let pwMsg = '';
  let poll: ReturnType<typeof setInterval>;

  // activate form
  let handle = '';
  let displayName = '';

  // moderation
  let modText = '';
  let showMod = false;
  let showManage = false;
  let peers: string[] = [];
  let personasList: any[] = [];

  const j = (r: Response) => r.json();
  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/orbnet${path}`, { credentials: 'same-origin', ...opts }).then(j);

  async function loadStatus() {
    try {
      status = await api('/status');
      if (status?.moderation_keywords) modText = status.moderation_keywords.join('\n');
      err = '';
      // Provisioning runs in the background after activate(); clear the banner
      // once the owner account appears, or give up after ~6 min of polling.
      if (enabling) {
        if (status?.owner) {
          enabling = false; busy = ''; enablingTries = 0;
        } else if (++enablingTries > 60) {
          enabling = false; busy = '';
          err = 'Bring-up is taking unusually long — Tor may be struggling to bootstrap. It keeps trying in the background; reload in a few minutes.';
        }
      }
      if (status?.enabled && status?.homeserver_up) await loadRooms();
    } catch (e: any) {
      err = e?.message ?? 'failed to load';
    }
  }
  async function loadRooms() {
    try {
      const r = await api('/rooms');
      if (r.ok) {
        rooms = (r.rooms ?? []).sort((a: Room, b: Room) => b.last_ts - a.last_ts);
        stats = r.stats ?? null;
        if (!selected && rooms.length) selectRoom(rooms[0]);
      }
    } catch { /* */ }
  }
  async function selectRoom(room: Room) {
    selected = room;
    await loadMessages();
  }
  async function loadMessages() {
    if (!selected) return;
    try {
      const r = await api(`/rooms/${encodeURIComponent(selected.room_id)}/messages`);
      if (r.ok) messages = (r.messages ?? []).slice().reverse(); // oldest→newest
    } catch { /* */ }
  }
  async function send() {
    if (!selected || !draft.trim()) return;
    const body = draft.trim();
    draft = '';
    await api(`/rooms/${encodeURIComponent(selected.room_id)}/send`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ body }),
    });
    await loadMessages();
  }
  async function activate() {
    err = '';
    enabling = true;
    enablingTries = 0;
    busy = 'Activating OrbNet — bringing up Tor + the homeserver in the background. This can take 1–2 min (longer on a cold start); the page updates automatically when it’s ready.';
    try {
      const r = await api('/enable', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ handle, display_name: displayName }),
      });
      // enable now returns immediately ({ ok, started }); only a config-write
      // failure reports ok:false. The slow bring-up runs server-side and the
      // poll below (loadStatus) clears the banner once the owner appears.
      if (r && r.ok === false) {
        err = typeof r.err === 'string' ? r.err : JSON.stringify(r.err);
        enabling = false; busy = '';
      }
    } catch (e: any) {
      // The request itself shouldn't block now, but even if it drops the
      // bring-up continues server-side — keep polling rather than erroring.
    }
    await loadStatus();
  }
  async function deactivate() {
    if (!(await confirmRite({
      title: 'Take OrbNet offline',
      body: 'Take OrbNet offline? Your account + rooms persist for re-enable.',
      danger: true,
      confirmLabel: 'take offline',
    }))) return;
    busy = 'Stopping…';
    await api('/disable', { method: 'POST' });
    busy = '';
    await loadStatus();
  }
  async function saveModeration() {
    const kws = modText.split('\n').map((s) => s.trim()).filter(Boolean);
    const r = await api('/moderation', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ moderation_keywords: kws }),
    });
    if (r.ok && status) status.moderation_keywords = r.moderation_keywords;
    showMod = false;
  }
  async function createGroup() {
    const name = await askRite({ title: 'New group', label: 'group name', placeholder: 'e.g. ops-council' });
    if (!name) return;
    const r = await api('/group', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, invite: [] }),
    });
    if (r.ok) await loadRooms();
    else toast.error('Could not create group: ' + JSON.stringify(r.err));
  }
  async function startDm() {
    const uid = await askRite({
      title: 'Direct message',
      label: 'Orb user',
      placeholder: '@nova:abc…onion',
    });
    if (!uid) return;
    const r = await api('/dm', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ user_id: uid }),
    });
    if (r.ok) await loadRooms();
    else toast.error('Could not start DM: ' + JSON.stringify(r.err));
  }
  async function peerOrb() {
    const onion = await askRite({
      title: 'Connect another Orb',
      body: 'You both federate + join each other’s community rooms.',
      label: 'OrbNet onion address',
      placeholder: 'xxxxx.onion',
    });
    if (!onion) return;
    busy = 'Peering over Tor…';
    const r = await api('/peer', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ onion }),
    });
    busy = '';
    if (r.ok) { await loadRooms(); toast.success(`Peered — joined ${r.joined_rooms} of its rooms.`); }
    else toast.error('Peer failed: ' + JSON.stringify(r.err));
  }

  // ── manage: federation peers + personas ──
  async function loadManage() {
    try {
      const [p, pr] = await Promise.all([api('/peers'), api('/personas')]);
      peers = p.peers ?? [];
      personasList = pr.personas ?? [];
    } catch { /* */ }
  }
  function toggleManage() {
    showManage = !showManage;
    if (showManage) loadManage();
  }
  async function unpeerOrb(onion: string) {
    if (!(await confirmRite({
      title: 'Stop federating',
      body: `Stop federating with ${onion.slice(0, 14)}… ? You'll leave its rooms.`,
      danger: true,
      confirmLabel: 'un-peer',
    }))) return;
    const r = await api('/peer/remove', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ onion }) });
    if (r.ok) { await loadManage(); await loadRooms(); }
    else toast.error('Could not un-peer: ' + JSON.stringify(r.err));
  }
  async function removePersona(user_id: string, name: string) {
    if (!(await confirmRite({
      title: 'Retire persona',
      body: `Retire persona "${name}"? It leaves all its rooms and stops responding.`,
      danger: true,
      confirmLabel: 'retire',
    }))) return;
    const r = await api('/persona/remove', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ user_id }) });
    if (r.ok) { await loadManage(); await loadRooms(); }
    else toast.error('Could not retire: ' + JSON.stringify(r.err));
  }
  async function leaveRoom() {
    if (!selected) return;
    if (!(await confirmRite({
      title: 'Leave room',
      body: `Leave "${selected.name}"?`,
      confirmLabel: 'leave',
    }))) return;
    const r = await api(`/rooms/${encodeURIComponent(selected.room_id)}/leave`, { method: 'POST' });
    if (r.ok) { selected = null; messages = []; await loadRooms(); }
    else toast.error('Could not leave: ' + JSON.stringify(r.err));
  }
  async function kickMember() {
    if (!selected) return;
    const uid = await askRite({
      title: 'Remove member',
      label: 'Matrix ID',
      placeholder: '@nova:abc…onion',
    });
    if (!uid) return;
    // Step 1 is the ABORT gate. A two-option dialog can't express "never mind":
    // confirmRite resolves false for the cancel button, Escape AND a backdrop
    // click, so mapping false onto "just kick" meant dismissing the dialog still
    // removed the member. Confirm the removal first, then pick the severity —
    // and let the dismissal of step 2 fall to the *less* destructive option.
    const remove = await confirmRite({
      title: `Remove ${uid.trim()}?`,
      body: 'They lose access to this room. You choose kick or ban next.',
      danger: true,
      confirmLabel: 'remove',
      cancelLabel: 'cancel',
    });
    if (!remove) return;
    const ban = await confirmRite({
      title: 'Ban as well?',
      body: `Ban blocks ${uid.trim()} from ever rejoining; a kick lets them back in later.`,
      danger: true,
      confirmLabel: 'ban',
      cancelLabel: 'kick only',
    });
    const r = await api(`/rooms/${encodeURIComponent(selected.room_id)}/kick`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ user_id: uid.trim(), ban }) });
    if (r.ok) toast.success(`${ban ? 'Banned' : 'Kicked'} ${uid}`);
    else toast.error('Could not remove member (do you have the power level?): ' + JSON.stringify(r.err));
  }

  // personas — human-placed only
  let showPersona = false;
  let pName = '', pPrompt = '', pLlm = '', pModel = '';
  let templates: any[] = [];      // pantheon agents (start-from-template)
  let llmSources: any[] = [];     // models running/cached across connected systems
  let pTemplate = '';
  let pModelSel = '';
  // persona edit (manage panel)
  let editingUser = '';
  let eModelSel = '', ePrompt = '';

  async function loadLlmSources() {
    try { const r = await api('/llm-sources'); llmSources = r.sources ?? []; } catch { llmSources = []; }
  }
  async function loadTemplates() {
    try {
      const sys = await fetch('/api/agent/systems', { credentials: 'same-origin' }).then(j);
      const systems = (sys.systems ?? sys ?? []).filter((s: any) => (s.roles ?? []).includes('openclaw'));
      const all: any[] = [];
      for (const s of systems) {
        try {
          const a = await fetch(`/api/agent/systems/${s.id}/agents`, { credentials: 'same-origin' }).then(j);
          for (const ag of (a.agents ?? [])) all.push({ key: `${s.id}::${ag.id}`, system_id: s.id, agent_id: ag.id, name: ag.name ?? ag.id, model: ag.model });
        } catch { /* */ }
      }
      templates = all;
    } catch { templates = []; }
  }
  function openPersona() {
    showPersona = !showPersona;
    if (showPersona) { loadLlmSources(); loadTemplates(); }
  }
  function modelOf(sel: string) { return llmSources.find((s: any) => `${s.system_id}::${s.model}` === sel); }
  function applyModel() {
    const m = modelOf(pModelSel);
    if (m) { pModel = m.model; pLlm = m.running ? m.endpoint : ''; }
  }
  async function applyTemplate() {
    const t = templates.find((x: any) => x.key === pTemplate);
    if (!t) return;
    pName = t.name || '';
    try {
      const soul = await fetch(`/api/agent/systems/${t.system_id}/agents/${encodeURIComponent(t.agent_id)}/persona-file?which=soul`, { credentials: 'same-origin' }).then(j);
      const ident = await fetch(`/api/agent/systems/${t.system_id}/agents/${encodeURIComponent(t.agent_id)}/persona-file?which=identity`, { credentials: 'same-origin' }).then(j);
      pPrompt = [soul?.content, ident?.content].filter(Boolean).join('\n\n');
    } catch { /* */ }
    if (t.model) {
      const m = llmSources.find((s: any) => s.model === t.model && s.running);
      if (m) { pModelSel = `${m.system_id}::${m.model}`; applyModel(); }
    }
  }
  function startEdit(p: any) {
    if (editingUser === p.user_id) { editingUser = ''; return; }
    editingUser = p.user_id; eModelSel = ''; ePrompt = '';
    loadLlmSources();
  }
  async function saveEdit(user_id: string) {
    const body: any = { user_id };
    if (ePrompt.trim()) body.system_prompt = ePrompt;
    const m = modelOf(eModelSel);
    if (m && m.running) { body.llm_url = m.endpoint; body.model = m.model; }
    const r = await api('/persona/update', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    if (r.ok) { editingUser = ''; await loadManage(); } else toast.error('Update failed: ' + JSON.stringify(r.err));
  }
  async function placePersona() {
    if (!selected || !pName.trim()) return;
    busy = 'Placing persona…';
    const r = await api('/persona', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: pName.trim(), room_id: selected.room_id, system_prompt: pPrompt, llm_url: pLlm, model: pModel }),
    });
    busy = '';
    if (r.ok) { showPersona = false; pName = pPrompt = pLlm = pModel = pTemplate = pModelSel = ''; toast.success('Persona placed in ' + selected.name); }
    else toast.error('Could not place persona: ' + JSON.stringify(r.err));
  }

  // moderation: hide messages whose body matches any keyword (case-insensitive)
  function filtered(m: Msg): boolean {
    return bodyFiltered(m.body);
  }
  function bodyFiltered(body?: string): boolean {
    const kws = status?.moderation_keywords ?? [];
    if (!kws.length || !body) return false;
    const b = body.toLowerCase();
    return kws.some((k) => b.includes(k.toLowerCase()));
  }
  const shortOnion = (o: string) => (o ? o.slice(0, 8) + '…' + o.slice(-10) : '');
  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // fallback for non-secure contexts / blocked clipboard API
      const ta = document.createElement('textarea');
      ta.value = text; ta.style.position = 'fixed'; ta.style.opacity = '0';
      document.body.appendChild(ta); ta.focus(); ta.select();
      try { document.execCommand('copy'); } catch { /* */ }
      document.body.removeChild(ta);
    }
    copied = text;
    setTimeout(() => { if (copied === text) copied = ''; }, 1500);
  }
  function qrSvg(text: string): string {
    if (!text) return '';
    try {
      const qr = qrcode(0, 'M');
      qr.addData(text);
      qr.make();
      return qr.createSvgTag({ cellSize: 4, margin: 2 });
    } catch { return ''; }
  }
  async function setClientPassword() {
    if (clientPw.length < 8) { pwMsg = 'Use at least 8 characters.'; return; }
    pwBusy = true; pwMsg = '';
    try {
      const r = await api('/client-password', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: clientPw }),
      });
      if (r.ok) { pwMsg = '✓ Password set — sign in to Element with your Matrix ID + this password.'; clientPw = ''; }
      else { pwMsg = 'Failed: ' + (typeof r.err === 'string' ? r.err : JSON.stringify(r.err)); }
    } catch (e: any) {
      pwMsg = e?.message ?? 'failed';
    } finally {
      pwBusy = false;
    }
  }
  const shortUser = (u: string) => (u || '').replace(/:.*onion$/, ':…');
  const fmtTime = (ts: number) => (ts ? new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '');

  onMount(() => {
    loadStatus();
    poll = setInterval(() => {
      if (status?.enabled || enabling) { loadStatus(); if (selected) loadMessages(); }
    }, 6000);
  });
  onDestroy(() => clearInterval(poll));

  $: dotCls = (s: string) => (s === 'active' ? 'bg-live-400' : 'bg-red-500');
  $: homeserver = status?.onion ? 'https://' + status.onion + ':8448' : '';
</script>

<div class="page-void h-full flex flex-col">
  <PageHeader title="OrbNet chat" subtitle="Matrix · Conduit · onion" backHref="/orbnet" backLabel="ORBNET" index="03.1">
    {#if status?.enabled}
      <button class="text-[11px] font-mono text-red-300/80 hover:text-red-300" on:click={deactivate}>take offline ✕</button>
    {/if}
  </PageHeader>

  <main class="flex-1 overflow-auto">
    {#if err}<div class="m-4 p-3 rounded bg-red-900/20 border border-red-500/40 text-red-300 text-sm">{err}</div>{/if}
    {#if busy}<div class="m-4 p-3 rounded bg-cursed-500/10 border border-cursed-500/30 text-cursed-200 text-sm">⏳ {busy}</div>{/if}
    {#if status?.last_error && !status?.enabled}
      <div class="m-4 p-3 rounded bg-amber-900/20 border border-amber-500/40 text-amber-200 text-sm font-mono">
        Last bring-up failed (OrbNet auto-disabled to protect the Orb): {status.last_error}
      </div>
    {/if}

    {#if !status}
      <p class="p-5 text-zinc-500 text-sm">loading…</p>

    {:else if !status.enabled}
      <!-- ── OFF: description + activate ─────────────────────────────── -->
      <div class="p-5 max-w-2xl mx-auto w-full space-y-5">
        <section class="bg-ink-900 border border-cursed-500/30 rounded-sm p-6 space-y-3">
          <h1 class="font-mono text-lg text-cursed-300">🔮 Join OrbNet</h1>
          <p class="text-zinc-300 text-sm leading-relaxed">
            OrbNet is a private, <b class="text-cursed-200">anonymous</b> chat mesh between Aeon Magick Orbs.
            Activating runs a Matrix homeserver on this Orb reachable only as a <b>Tor onion service</b> —
            <b class="text-live-300">no port-forwarding, no DynDNS, your home IP never exposed</b>, works behind CGNAT.
            Every Orb authenticates by its onion address, so the network is fully decentralized.
          </p>
          <ul class="text-zinc-400 text-sm space-y-1 list-disc list-inside">
            <li>Auto-join the OrbNet <b>community</b> — interest rooms for tech, AI, makers, privacy &amp; more</li>
            <li>Spin off your own <b>group chats</b> and <b>end-to-end-encrypted DMs</b></li>
            <li>Set a personal <b>moderation filter</b> — hide content you'd rather not see</li>
            <li>Optionally place a <b>persona</b> into a room (you choose — never automatic)</li>
          </ul>
          <p class="text-[11px] text-zinc-500">
            Anonymity note: message content in DMs/private groups is E2E-encrypted; public community rooms are readable
            (that's how the mesh + activity feed work). Other servers can see room membership + timing — onion transport
            hides IPs, not all metadata.
          </p>
        </section>
        <section class="panel p-5 space-y-3">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-400">Activate</h2>
          <label class="block text-xs text-zinc-500">Handle <span class="text-zinc-600">(pseudonymous; @handle:your-onion)</span>
            <input bind:value={handle} placeholder="e.g. aurora — leave blank for a random one"
                   class="mt-1 w-full bg-ink-800 border border-steel-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
          </label>
          <label class="block text-xs text-zinc-500">Display name <span class="text-zinc-600">(shown in chats)</span>
            <input bind:value={displayName} placeholder="optional"
                   class="mt-1 w-full bg-ink-800 border border-steel-700 rounded px-3 py-1.5 text-sm text-zinc-200" />
          </label>
          <button class="btn bg-cursed-600/30 border-cursed-500/50 text-cursed-100 hover:bg-cursed-600/40"
                  disabled={!!busy} on:click={activate}>🔮 Activate OrbNet</button>
        </section>
      </div>

    {:else}
      <!-- ── ON: status + community + chat ───────────────────────────── -->
      <div class="p-4 space-y-3">
        <section class="panel p-4 flex flex-wrap items-center gap-x-6 gap-y-2 text-sm">
          <div class="flex items-center gap-2"><span class="w-2 h-2 rounded-full {dotCls(status.tor)}"></span><span class="text-zinc-400">Tor</span></div>
          <div class="flex items-center gap-2"><span class="w-2 h-2 rounded-full {dotCls(status.conduit)}"></span><span class="text-zinc-400">Homeserver</span></div>
          <button class="font-mono text-[11px] text-cursed-300 hover:text-cursed-200 inline-flex items-center gap-1.5" title="Copy full address: {status.onion}" on:click={() => copy(status.onion)}>{shortOnion(status.onion)}<span class="text-zinc-500">{copied === status.onion ? '✓' : '⧉'}</span></button>
          {#if status.owner}<button class="font-mono text-[11px] text-zinc-400 hover:text-zinc-200 inline-flex items-center gap-1.5" title="Copy your Matrix ID: {status.owner}" on:click={() => copy(status.owner)}>you: {shortUser(status.owner)}<span class="text-zinc-600">{copied === status.owner ? '✓' : '⧉'}</span></button>{/if}
          {#if stats}<div class="text-[11px] text-zinc-400" title="active = posted in the last hour (presence over Tor is unreliable)">{stats.members} member{stats.members === 1 ? '' : 's'} · {stats.active} active · {stats.rooms} rooms</div>{/if}
          <div class="ml-auto flex items-center gap-2">
            <button class="btn text-xs" on:click={createGroup}>+ group</button>
            <button class="btn text-xs" on:click={startDm}>+ DM</button>
            <button class="btn text-xs" on:click={peerOrb}>+ peer Orb</button>
            <button class="btn text-xs" class:active={showConnect} on:click={() => (showConnect = !showConnect)}>📱 connect</button>
            <button class="btn text-xs" class:active={showMod} on:click={() => (showMod = !showMod)}>moderation</button>
            <button class="btn text-xs" class:active={showManage} on:click={toggleManage}>manage</button>
          </div>
        </section>

        {#if showConnect}
          <section class="bg-ink-900 border border-cursed-500/30 rounded-sm p-4 space-y-4">
            <div class="flex items-center justify-between">
              <div class="text-xs font-mono uppercase tracking-wider text-cursed-300">📱 Connect a client (Element)</div>
              <button class="text-zinc-500 hover:text-zinc-300 text-xs" on:click={() => (showConnect = false)}>close ✕</button>
            </div>
            <div class="flex flex-wrap items-start gap-5">
              <div class="bg-white p-2 rounded inline-block shrink-0">{@html qrSvg(homeserver)}</div>
              <div class="space-y-2.5 text-[12px] min-w-[240px]">
                <div>
                  <div class="text-zinc-500">Homeserver URL</div>
                  <button class="font-mono text-cursed-200 hover:text-cursed-100 break-all text-left" on:click={() => copy(homeserver)}>{homeserver}<span class="text-zinc-500 ml-1">{copied === homeserver ? '✓' : '⧉'}</span></button>
                </div>
                <div>
                  <div class="text-zinc-500">Your Matrix ID</div>
                  <button class="font-mono text-zinc-300 hover:text-zinc-100 break-all text-left" on:click={() => copy(status.owner)}>{status.owner}<span class="text-zinc-500 ml-1">{copied === status.owner ? '✓' : '⧉'}</span></button>
                </div>
                <a class="inline-flex items-center gap-1 text-cursed-300 hover:text-cursed-200 underline" href="/api/orbnet/cert" download>⬇ Download this Orb's certificate</a>
              </div>
            </div>
            <div class="space-y-1.5">
              <div class="text-zinc-500 text-[12px]">Set a login password <span class="text-zinc-600">(your account's original password is random — choose one you'll type into Element)</span></div>
              <div class="flex gap-2 max-w-md">
                <input class="flex-1 bg-ink-800 border border-steel-700 rounded px-3 py-1.5 text-sm text-zinc-200" type="password" autocomplete="new-password" placeholder="new password (8+ chars)" bind:value={clientPw} on:keydown={(e) => e.key === 'Enter' && setClientPassword()} />
                <button class="btn text-sm" disabled={pwBusy} on:click={setClientPassword}>{pwBusy ? 'setting…' : 'set password'}</button>
              </div>
              {#if pwMsg}<div class="text-[12px] {pwMsg.startsWith('✓') ? 'text-live-300' : 'text-amber-300'}">{pwMsg}</div>{/if}
            </div>
            <details class="text-[12px] text-zinc-400">
              <summary class="cursor-pointer text-zinc-300 hover:text-zinc-100 select-none">How to connect (Tor required) ▾</summary>
              <ol class="list-decimal ml-5 mt-2 space-y-1.5">
                <li>On the phone, install <b class="text-zinc-200">Orbot</b> and enable VPN mode so apps can reach <code class="text-cursed-200">.onion</code> addresses.</li>
                <li><b class="text-zinc-200">Install the certificate</b> (button above). iOS: open the file → Install, then Settings ▸ General ▸ About ▸ Certificate Trust Settings ▸ turn it on. Android: Settings ▸ Security ▸ Install a certificate ▸ CA certificate.</li>
                <li><b class="text-zinc-200">Set a login password</b> above.</li>
                <li>Open <b class="text-zinc-200">Element</b> → Sign in → tap <i>Edit</i> / “Other homeserver” → paste the <b>Homeserver URL</b>.</li>
                <li>Sign in with your <b>Matrix ID</b> and the password you set.</li>
              </ol>
              <p class="mt-2 text-zinc-500">The cert lets Element trust your Orb's self-signed TLS (its SAN = your onion) — most reliable on iOS. If Android Element ignores user certs, use Element Web in Tor Browser instead: open the Homeserver URL once to accept the certificate, then sign in.</p>
            </details>
          </section>
        {/if}

        {#if showManage}
          <section class="bg-ink-900 border border-cursed-500/30 rounded-sm p-4 space-y-4">
            <div class="flex items-center justify-between">
              <div class="text-xs font-mono uppercase tracking-wider text-cursed-300">⚙ Manage — peers &amp; personas</div>
              <button class="text-zinc-500 hover:text-zinc-300 text-xs" on:click={() => (showManage = false)}>close ✕</button>
            </div>
            <div class="space-y-1.5">
              <div class="text-[11px] uppercase tracking-wide text-zinc-500">Federated Orbs ({peers.length})</div>
              {#if peers.length === 0}
                <p class="text-[12px] text-zinc-600">No peers. Use <b class="text-zinc-400">+ peer Orb</b> to federate with a friend's Orb by its onion.</p>
              {:else}
                {#each peers as onion}
                  <div class="flex items-center gap-2 text-[12px]">
                    <span class="font-mono text-zinc-300 break-all flex-1" title={onion}>{shortOnion(onion)}</span>
                    <button class="text-red-400/80 hover:text-red-300 shrink-0" on:click={() => unpeerOrb(onion)}>un-peer ✕</button>
                  </div>
                {/each}
              {/if}
            </div>
            <div class="space-y-1.5 border-t border-ink-800 pt-3">
              <div class="text-[11px] uppercase tracking-wide text-zinc-500">Placed personas ({personasList.length})</div>
              {#if personasList.length === 0}
                <p class="text-[12px] text-zinc-600">No personas placed. Open a room → <b class="text-zinc-400">🎭 + persona</b>.</p>
              {:else}
                {#each personasList as p}
                  <div class="text-[12px] space-y-1">
                    <div class="flex items-center gap-2">
                      <span class="text-cursed-200 flex-1 truncate">🎭 {p.name} <span class="text-zinc-600">· {p.rooms.length} room{p.rooms.length === 1 ? '' : 's'}{p.has_llm ? '' : ' · ⚠ no model'}</span></span>
                      <button class="text-zinc-400 hover:text-zinc-200 shrink-0" on:click={() => startEdit(p)}>{editingUser === p.user_id ? 'cancel' : 'edit'}</button>
                      <button class="text-red-400/80 hover:text-red-300 shrink-0" on:click={() => removePersona(p.user_id, p.name)}>retire ✕</button>
                    </div>
                    {#if editingUser === p.user_id}
                      <div class="space-y-1.5 pl-2 border-l-2 border-cursed-500/30">
                        <select bind:value={eModelSel} class="w-full bg-ink-800 border border-steel-700 rounded px-2 py-1 text-[11px] text-zinc-200 font-mono">
                          <option value="">— keep current model —</option>
                          {#each llmSources as s}<option value={`${s.system_id}::${s.model}`} disabled={!s.running}>{s.running ? '🟢' : '⚪'} {s.model} · {s.system}{s.running ? '' : ' (needs deploy)'}</option>{/each}
                        </select>
                        <textarea bind:value={ePrompt} rows="3" placeholder="new soul / system prompt (blank = unchanged)" class="w-full bg-ink-800 border border-steel-700 rounded px-2 py-1 text-[11px] text-zinc-200"></textarea>
                        <button class="btn text-[11px]" on:click={() => saveEdit(p.user_id)}>save</button>
                      </div>
                    {/if}
                  </div>
                {/each}
              {/if}
            </div>
          </section>
        {/if}

        {#if showMod}
          <section class="bg-ink-900 border border-amber-500/30 rounded-sm p-4 space-y-2">
            <div class="text-xs font-mono uppercase tracking-wider text-amber-300">Your moderation filter</div>
            <p class="text-[11px] text-zinc-500">One keyword/phrase per line. Messages containing any of these are hidden for you (client-side, case-insensitive). Your filter only — never affects anyone else.</p>
            <textarea bind:value={modText} rows="4" placeholder="one keyword per line"
                      class="w-full bg-ink-800 border border-steel-700 rounded px-3 py-2 text-sm text-zinc-200 font-mono"></textarea>
            <button class="btn text-xs" on:click={saveModeration}>save filter</button>
          </section>
        {/if}

        <div class="grid grid-cols-1 md:grid-cols-[260px_1fr] gap-3">
          <!-- rooms -->
          <section class="panel p-2 space-y-1 h-[60vh] overflow-y-auto">
            <div class="px-2 py-1 text-[10px] font-mono uppercase tracking-wider text-zinc-500">Rooms ({rooms.length})</div>
            {#each rooms as r (r.room_id)}
              <button class="w-full text-left px-2 py-1.5 rounded text-sm {selected?.room_id === r.room_id ? 'bg-cursed-500/20 text-cursed-100' : 'text-zinc-300 hover:bg-ink-800'}"
                      on:click={() => selectRoom(r)}>
                <div class="flex items-center gap-2">
                  <span class="flex-1 truncate">{r.name}</span>
                  {#if r.members}<span class="text-[9px] text-zinc-600">{r.members}👤</span>{/if}
                  {#if r.last_ts}<span class="text-[10px] text-zinc-600">{fmtTime(r.last_ts)}</span>{/if}
                </div>
                {#if r.last_body}<div class="text-[10px] text-zinc-600 truncate">{bodyFiltered(r.last_body) ? '⊘ hidden' : r.last_body}</div>{/if}
              </button>
            {:else}
              <p class="px-2 py-3 text-xs text-zinc-600">No rooms yet — the community is being set up.</p>
            {/each}
          </section>

          <!-- chat -->
          <section class="panel flex flex-col h-[60vh]">
            {#if selected}
              <div class="px-4 py-2 border-b border-ink-800 flex items-center gap-2">
                <span class="text-sm text-zinc-200 font-mono truncate flex-1">{selected.name}</span>
                <button class="btn text-[11px] inline-flex items-center gap-1" on:click={kickMember} title="Remove a member (kick or ban)">
                  <Icon name="close" class="w-3 h-3" />kick
                </button>
                <button class="btn text-[11px] inline-flex items-center gap-1" on:click={leaveRoom} title="Leave this room">
                  <Icon name="logout" class="w-3 h-3" />leave
                </button>
                <button class="btn text-[11px]" class:active={showPersona} on:click={openPersona}>🎭 + persona</button>
              </div>
              {#if showPersona}
                <div class="p-3 border-b border-ink-800 bg-ink-950/40 space-y-2">
                  <p class="text-[11px] text-zinc-500">Place a persona into <b class="text-cursed-200">{selected.name}</b> — start from a pantheon member + pick a running model. <b>You place it — never automatic.</b></p>
                  <select bind:value={pTemplate} on:change={applyTemplate} class="w-full bg-ink-800 border border-steel-700 rounded px-2 py-1 text-xs text-zinc-200">
                    <option value="">— Blank / from scratch —</option>
                    {#each templates as t}<option value={t.key}>🎭 {t.name}{t.model ? ` · ${t.model}` : ''}</option>{/each}
                  </select>
                  <input bind:value={pName} placeholder="persona name" class="w-full bg-ink-800 border border-steel-700 rounded px-2 py-1 text-xs text-zinc-200" />
                  <textarea bind:value={pPrompt} rows="3" placeholder="soul / personality (system prompt)" class="w-full bg-ink-800 border border-steel-700 rounded px-2 py-1 text-xs text-zinc-200"></textarea>
                  <select bind:value={pModelSel} on:change={applyModel} class="w-full bg-ink-800 border border-steel-700 rounded px-2 py-1 text-xs text-zinc-200 font-mono">
                    <option value="">— pick a model —</option>
                    {#each llmSources as s}<option value={`${s.system_id}::${s.model}`} disabled={!s.running}>{s.running ? '🟢' : '⚪'} {s.model} · {s.system}{s.running ? '' : ' (needs deploy)'}</option>{/each}
                  </select>
                  <button class="btn text-xs" disabled={!!busy || !pName || !pLlm} on:click={placePersona}>place persona</button>
                </div>
              {/if}
              <div class="flex-1 overflow-y-auto p-3 space-y-2">
                {#each messages as m (m.event_id)}
                  {#if filtered(m)}
                    <div class="text-[11px] text-zinc-700 italic">⊘ message hidden by your filter</div>
                  {:else}
                    <div class="text-sm">
                      <span class="font-mono text-[11px] text-cursed-300">{shortUser(m.sender)}</span>
                      <span class="text-[10px] text-zinc-600 ml-1">{fmtTime(m.ts)}</span>
                      <div class="text-zinc-200">{m.body}</div>
                    </div>
                  {/if}
                {:else}
                  <p class="text-xs text-zinc-600">No messages yet — say hello.</p>
                {/each}
              </div>
              <form class="p-2 border-t border-ink-800 flex gap-2" on:submit|preventDefault={send}>
                <input bind:value={draft} placeholder="message {selected.name}…"
                       class="flex-1 bg-ink-800 border border-steel-700 rounded px-3 py-1.5 text-sm text-zinc-200" />
                <button class="btn text-xs" type="submit">send</button>
              </form>
            {:else}
              <p class="m-auto text-xs text-zinc-600">Select a room.</p>
            {/if}
          </section>
        </div>
      </div>
    {/if}
  </main>
</div>
