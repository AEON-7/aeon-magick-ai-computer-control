<script lang="ts">
  // OrbNet — anonymous Matrix federation over Tor between Aeon Magick Orbs.
  // Activate the homeserver, auto-join the community, chat in interest rooms,
  // spin off groups + DMs, set your own moderation filter. Talks to the
  // supervisor's admin-only /api/orbnet/* surface.
  import { onMount, onDestroy } from 'svelte';

  type Status = {
    ok: boolean; enabled: boolean; onion: string; homeserver_up: boolean;
    tor: string; conduit: string; owner: string | null;
    display_name: string; handle: string; auto_join_community: boolean;
    moderation_keywords: string[];
  };
  type Room = { room_id: string; name: string; last_ts: number };
  type Msg = { sender: string; body: string; ts: number; event_id: string };

  let status: Status | null = null;
  let rooms: Room[] = [];
  let selected: Room | null = null;
  let messages: Msg[] = [];
  let draft = '';
  let err = '';
  let busy = '';
  let poll: ReturnType<typeof setInterval>;

  // activate form
  let handle = '';
  let displayName = '';

  // moderation
  let modText = '';
  let showMod = false;

  const j = (r: Response) => r.json();
  const api = (path: string, opts: RequestInit = {}) =>
    fetch(`/api/orbnet${path}`, { credentials: 'same-origin', ...opts }).then(j);

  async function loadStatus() {
    try {
      status = await api('/status');
      if (status?.moderation_keywords) modText = status.moderation_keywords.join('\n');
      err = '';
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
    busy = 'Activating OrbNet — bootstrapping Tor + the homeserver (~1–2 min)…';
    err = '';
    try {
      const r = await api('/enable', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ handle, display_name: displayName }),
      });
      if (!r.ok) err = typeof r.err === 'string' ? r.err : JSON.stringify(r.err);
    } catch (e: any) {
      err = e?.message ?? 'activation failed';
    } finally {
      busy = '';
      await loadStatus();
    }
  }
  async function deactivate() {
    if (!confirm('Take OrbNet offline? Your account + rooms persist for re-enable.')) return;
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
    const name = prompt('Group name?');
    if (!name) return;
    const r = await api('/group', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, invite: [] }),
    });
    if (r.ok) await loadRooms();
    else alert('Could not create group: ' + JSON.stringify(r.err));
  }
  async function startDm() {
    const uid = prompt('Direct message which Orb user? (e.g. @nova:abc…onion)');
    if (!uid) return;
    const r = await api('/dm', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ user_id: uid }),
    });
    if (r.ok) await loadRooms();
    else alert('Could not start DM: ' + JSON.stringify(r.err));
  }

  // moderation: hide messages whose body matches any keyword (case-insensitive)
  function filtered(m: Msg): boolean {
    const kws = status?.moderation_keywords ?? [];
    if (!kws.length || !m.body) return false;
    const b = m.body.toLowerCase();
    return kws.some((k) => b.includes(k.toLowerCase()));
  }
  const shortOnion = (o: string) => (o ? o.slice(0, 8) + '…' + o.slice(-10) : '');
  const shortUser = (u: string) => (u || '').replace(/:.*onion$/, ':…');
  const fmtTime = (ts: number) => (ts ? new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '');

  onMount(() => {
    loadStatus();
    poll = setInterval(() => {
      if (status?.enabled) { loadStatus(); if (selected) loadMessages(); }
    }, 6000);
  });
  onDestroy(() => clearInterval(poll));

  $: dotCls = (s: string) => (s === 'active' ? 'bg-live-400' : 'bg-red-500');
</script>

<div class="h-full flex flex-col">
  <header class="flex items-center justify-between px-5 py-3 border-b border-ink-700 bg-ink-900">
    <div class="flex items-center gap-3">
      <a href="/" class="text-cursed-400 font-mono text-sm tracking-widest hover:underline">← AEON MAGICK</a>
      <span class="text-zinc-400 font-mono text-xs uppercase tracking-wider">🔮 OrbNet</span>
    </div>
    {#if status?.enabled}
      <button class="text-[11px] font-mono text-red-300/80 hover:text-red-300" on:click={deactivate}>take offline ✕</button>
    {/if}
  </header>

  <main class="flex-1 overflow-auto">
    {#if err}<div class="m-4 p-3 rounded bg-red-900/20 border border-red-500/40 text-red-300 text-sm">{err}</div>{/if}
    {#if busy}<div class="m-4 p-3 rounded bg-cursed-500/10 border border-cursed-500/30 text-cursed-200 text-sm">⏳ {busy}</div>{/if}

    {#if !status}
      <p class="p-5 text-zinc-500 text-sm">loading…</p>

    {:else if !status.enabled}
      <!-- ── OFF: description + activate ─────────────────────────────── -->
      <div class="p-5 max-w-2xl mx-auto w-full space-y-5">
        <section class="bg-ink-900 border border-cursed-500/30 rounded-xl p-6 space-y-3">
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
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-5 space-y-3">
          <h2 class="font-mono text-sm uppercase tracking-wider text-zinc-400">Activate</h2>
          <label class="block text-xs text-zinc-500">Handle <span class="text-zinc-600">(pseudonymous; @handle:your-onion)</span>
            <input bind:value={handle} placeholder="e.g. aurora — leave blank for a random one"
                   class="mt-1 w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200 font-mono" />
          </label>
          <label class="block text-xs text-zinc-500">Display name <span class="text-zinc-600">(shown in chats)</span>
            <input bind:value={displayName} placeholder="optional"
                   class="mt-1 w-full bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200" />
          </label>
          <button class="btn bg-cursed-600/30 border-cursed-500/50 text-cursed-100 hover:bg-cursed-600/40"
                  disabled={!!busy} on:click={activate}>🔮 Activate OrbNet</button>
        </section>
      </div>

    {:else}
      <!-- ── ON: status + community + chat ───────────────────────────── -->
      <div class="p-4 space-y-3">
        <section class="bg-ink-900 border border-ink-700 rounded-xl p-4 flex flex-wrap items-center gap-x-6 gap-y-2 text-sm">
          <div class="flex items-center gap-2"><span class="w-2 h-2 rounded-full {dotCls(status.tor)}"></span><span class="text-zinc-400">Tor</span></div>
          <div class="flex items-center gap-2"><span class="w-2 h-2 rounded-full {dotCls(status.conduit)}"></span><span class="text-zinc-400">Homeserver</span></div>
          <div class="font-mono text-[11px] text-cursed-300" title={status.onion}>{shortOnion(status.onion)}</div>
          {#if status.owner}<div class="font-mono text-[11px] text-zinc-400">you: {shortUser(status.owner)}</div>{/if}
          <div class="ml-auto flex items-center gap-2">
            <button class="btn text-xs" on:click={createGroup}>+ group</button>
            <button class="btn text-xs" on:click={startDm}>+ DM</button>
            <button class="btn text-xs" class:active={showMod} on:click={() => (showMod = !showMod)}>moderation</button>
          </div>
        </section>

        {#if showMod}
          <section class="bg-ink-900 border border-amber-500/30 rounded-xl p-4 space-y-2">
            <div class="text-xs font-mono uppercase tracking-wider text-amber-300">Your moderation filter</div>
            <p class="text-[11px] text-zinc-500">One keyword/phrase per line. Messages containing any of these are hidden for you (client-side, case-insensitive). Your filter only — never affects anyone else.</p>
            <textarea bind:value={modText} rows="4" placeholder="one keyword per line"
                      class="w-full bg-ink-800 border border-ink-700 rounded px-3 py-2 text-sm text-zinc-200 font-mono"></textarea>
            <button class="btn text-xs" on:click={saveModeration}>save filter</button>
          </section>
        {/if}

        <div class="grid grid-cols-1 md:grid-cols-[260px_1fr] gap-3">
          <!-- rooms -->
          <section class="bg-ink-900 border border-ink-700 rounded-xl p-2 space-y-1 h-[60vh] overflow-y-auto">
            <div class="px-2 py-1 text-[10px] font-mono uppercase tracking-wider text-zinc-500">Rooms ({rooms.length})</div>
            {#each rooms as r (r.room_id)}
              <button class="w-full text-left px-2 py-1.5 rounded text-sm flex items-center gap-2 {selected?.room_id === r.room_id ? 'bg-cursed-500/20 text-cursed-100' : 'text-zinc-300 hover:bg-ink-800'}"
                      on:click={() => selectRoom(r)}>
                <span class="flex-1 truncate">{r.name}</span>
                {#if r.last_ts}<span class="text-[10px] text-zinc-600">{fmtTime(r.last_ts)}</span>{/if}
              </button>
            {:else}
              <p class="px-2 py-3 text-xs text-zinc-600">No rooms yet — the community is being set up.</p>
            {/each}
          </section>

          <!-- chat -->
          <section class="bg-ink-900 border border-ink-700 rounded-xl flex flex-col h-[60vh]">
            {#if selected}
              <div class="px-4 py-2 border-b border-ink-800 text-sm text-zinc-200 font-mono truncate">{selected.name}</div>
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
                       class="flex-1 bg-ink-800 border border-ink-700 rounded px-3 py-1.5 text-sm text-zinc-200" />
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
