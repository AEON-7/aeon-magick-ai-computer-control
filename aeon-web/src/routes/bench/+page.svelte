<script lang="ts">
  // Aeon Bench — deploy the Aeon Bench Pod and drive the **verified path**,
  // including remote endpoint benches over SSH on any connected system.
  import { onMount, onDestroy } from 'svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { serverMode } from '$lib/mode';
  import { toast } from '$lib/toast';
  import { confirmRite } from '$lib/confirm';
  import {
    listSystems,
    benchDeploy,
    benchStatus,
    benchStop,
    benchModelInfo,
    benchUpdates,
    benchUpdate,
    benchAuthorize,
    benchScanEndpoints,
    benchRunVerified,
    benchJobs,
    type ConnectedSystem,
    type BenchStatus,
    type BenchModelInfo,
    type BenchUpdates,
    type BenchEndpoint,
  } from '$lib/api';

  let systems: ConnectedSystem[] = [];
  let target = '';
  let hfLink = '';
  let hfToken = '';
  let showAdvanced = false;
  // Co-located GPU serve (true) vs control-plane-only for remote verified (false).
  let wantGpu = true;
  let adv = { AEON_PORT: '', AEON_SYSTEM: '', AEON_PAUSE_CONTAINERS: '' };

  let st: BenchStatus | null = null;
  let deploying = false;
  let lastTarget = '';
  let poll: ReturnType<typeof setInterval>;

  let upd: BenchUpdates | null = null;
  let updating = false;
  let checkedTarget = '';

  // ── Verified remote run ──
  let serveSystem = '';
  let endpoints: BenchEndpoint[] = [];
  let selectedEp: BenchEndpoint | null = null;
  let verifyHf = '';
  let endpointModel = '';
  let deepVerify = false;
  let preset = 'comprehensive';
  let scanning = false;
  let authorizing = false;
  let runningVerified = false;
  let lastJobId = '';
  let jobsSnap: unknown = null;
  let authorizedNote = '';

  let info: BenchModelInfo | null = null;
  let infoLoading = false;
  let infoTimer: ReturnType<typeof setTimeout>;
  $: detectModel(hfLink);
  function detectModel(link: string) {
    clearTimeout(infoTimer);
    const id = link.trim();
    if (!/^[\w.\-]+\/[\w.\-]+$/.test(id)) {
      info = null;
      infoLoading = false;
      return;
    }
    infoLoading = true;
    infoTimer = setTimeout(async () => {
      try {
        const r = await benchModelInfo(id);
        info = r?.ok ? r : null;
      } catch {
        info = null;
      }
      infoLoading = false;
    }, 600);
  }

  // Always offer this Orb as a pod host (remote-endpoint control plane on Pi;
  // co-located GPU on headless server builds).
  $: targetOptions = [
    {
      id: 'local',
      label: $serverMode
        ? 'This server (local)'
        : 'This Orb (remote-endpoint mode · no GPU)',
    },
    ...systems.map((s) => ({
      id: s.id,
      label: `${s.label} · ${s.address}${s.roles?.includes('dgx') ? ' · GPU' : ''}`,
    })),
  ];
  $: serveOptions = systems.map((s) => ({
    id: s.id,
    label: `${s.label} · ${s.address}${s.roles?.includes('dgx') ? ' · GPU' : ''}`,
  }));
  $: noServeHost = serveOptions.length === 0;
  $: if (!target && targetOptions.length) target = targetOptions[0].id;
  $: if (!serveSystem && serveOptions.length) serveSystem = serveOptions[0].id;
  $: if (target && target !== lastTarget) {
    lastTarget = target;
    st = null;
    upd = null;
    checkedTarget = '';
    endpoints = [];
    selectedEp = null;
    lastJobId = '';
    authorizedNote = '';
    // Default GPU off when pod is on this Pi (no NVIDIA).
    wantGpu = target !== 'local' || !!$serverMode;
    refresh();
  }

  const ACTIVE = ['queued', 'pulling', 'installing-docker', 'starting', 'updating'];
  $: phase = st?.phase ?? 'idle';
  $: busy = deploying || ACTIVE.includes(phase);
  $: running = !!st?.running || phase === 'running';
  $: dashPort = st?.dash_port ?? (Number(adv.AEON_PORT) || 8091);
  $: dashHost = st?.host ?? (typeof window !== 'undefined' ? window.location.hostname : 'localhost');
  $: dashUrl = `http://${dashHost}:${dashPort}`;
  $: targetLabel = targetOptions.find((o) => o.id === target)?.label ?? target;
  $: serveLabel = serveOptions.find((o) => o.id === serveSystem)?.label ?? serveSystem;

  const PHASE_PCT: Record<string, number> = {
    queued: 5,
    pulling: 45,
    'installing-docker': 20,
    starting: 80,
    updating: 45,
    running: 100,
    failed: 100,
    stopped: 0,
    idle: 0,
  };

  async function refresh() {
    if (!target) return;
    try {
      const r = await benchStatus(target);
      if (r?.ok) st = r;
      if (r?.running && checkedTarget !== target) {
        checkedTarget = target;
        checkUpdates();
      }
      if (r?.running) {
        try {
          const j = await benchJobs(target);
          if (j?.ok) jobsSnap = j.jobs ?? j.result ?? null;
        } catch {}
      }
    } catch {}
  }

  async function checkUpdates() {
    try {
      const r = await benchUpdates(target);
      if (r?.ok) upd = r;
    } catch {}
  }

  async function updatePod() {
    updating = true;
    try {
      const r = await benchUpdate(target);
      if (!r?.ok) {
        toast('Update failed: ' + (r?.err ?? 'unknown error'), 'error');
      } else {
        toast(`Updating the pod on ${targetLabel}…`, 'success');
        upd = null;
        checkedTarget = '';
        await refresh();
        startPolling();
      }
    } catch (e: any) {
      toast('Update failed: ' + (e?.message ?? 'error'), 'error');
    }
    updating = false;
  }
  function startPolling() {
    clearInterval(poll);
    poll = setInterval(refresh, 3000);
  }

  async function deploy() {
    const hf = hfLink.trim();
    if (hf && !/^[\w.\-]+\/[\w.\-]+$/.test(hf)) {
      toast(
        'That doesn\'t look like a model id — use "org/model", or leave it blank and pick one in the dashboard.',
        'error',
      );
      return;
    }
    deploying = true;
    const env: Record<string, string> = {};
    for (const [k, v] of Object.entries(adv)) if (v.trim()) env[k] = v.trim();
    try {
      const r = await benchDeploy({
        target,
        hf_link: hf,
        hf_token: hfToken.trim() || undefined,
        env,
        gpu: wantGpu,
      });
      if (!r?.ok) {
        toast('Deploy failed: ' + (r?.err ?? 'unknown error'), 'error');
        deploying = false;
        return;
      }
      toast(
        wantGpu
          ? `Deploying GPU bench pod on ${targetLabel}…`
          : `Deploying remote-endpoint pod on ${targetLabel}…`,
        'success',
      );
      await refresh();
      startPolling();
    } catch (e: any) {
      toast('Deploy failed: ' + (e?.message ?? 'error'), 'error');
    }
    deploying = false;
  }

  async function stop() {
    if (
      !(await confirmRite({
        title: 'Stop the bench pod?',
        body: `This removes the pod container (docker rm -f) on ${targetLabel}.`,
        confirmLabel: 'Stop pod',
        danger: true,
      }))
    )
      return;
    try {
      const r = await benchStop(target);
      if (r?.ok) {
        toast('Pod stopped.', 'info');
        await refresh();
      } else toast('Stop failed: ' + (r?.err ?? 'error'), 'error');
    } catch (e: any) {
      toast('Stop failed: ' + (e?.message ?? 'error'), 'error');
    }
  }

  async function authorizeServe() {
    if (!serveSystem) {
      toast('Pick a serve system first.', 'error');
      return;
    }
    authorizing = true;
    authorizedNote = '';
    try {
      const r = await benchAuthorize({ target, serve_system: serveSystem });
      if (!r?.ok) {
        toast('Authorize failed: ' + (r?.err ?? 'error'), 'error');
      } else {
        authorizedNote =
          r.status === 'already_authorized'
            ? `Pod key already on ${serveLabel}`
            : `Pod key installed on ${serveLabel} (${r.remote_host ?? ''})`;
        toast(authorizedNote, 'success');
      }
    } catch (e: any) {
      toast('Authorize failed: ' + (e?.message ?? 'error'), 'error');
    }
    authorizing = false;
  }

  async function scanServe() {
    if (!serveSystem) {
      toast('Pick a serve system first.', 'error');
      return;
    }
    scanning = true;
    endpoints = [];
    selectedEp = null;
    try {
      const r = await benchScanEndpoints(target, serveSystem);
      if (!r?.ok) {
        toast('Scan failed: ' + (r?.err ?? 'error'), 'error');
      } else {
        const list = Array.isArray(r.endpoints) ? r.endpoints : [];
        endpoints = list;
        if (!list.length) {
          toast('No live OpenAI-compatible endpoints found on that host.', 'info');
        } else {
          toast(`Found ${list.length} endpoint(s).`, 'success');
        }
      }
    } catch (e: any) {
      toast('Scan failed: ' + (e?.message ?? 'error'), 'error');
    }
    scanning = false;
  }

  function pickEndpoint(ep: BenchEndpoint) {
    selectedEp = ep;
    if (ep.hf_guess) verifyHf = String(ep.hf_guess);
    const models = ep.models ?? (ep.model ? [ep.model] : []);
    endpointModel = models[0] ? String(models[0]) : '';
  }

  async function runVerified() {
    const hf = verifyHf.trim() || hfLink.trim();
    if (!/^[\w.\-]+\/[\w.\-]+$/.test(hf)) {
      toast('HF link required — exact quant repo being served (org/model).', 'error');
      return;
    }
    const serve_url = selectedEp?.url ? String(selectedEp.url) : '';
    if (!serve_url) {
      toast('Scan and select a live endpoint first (verified remote path).', 'error');
      return;
    }
    runningVerified = true;
    try {
      const r = await benchRunVerified({
        target,
        hf_link: hf,
        serve_url,
        remote_host: serveSystem || undefined,
        endpoint_model: endpointModel.trim() || undefined,
        verify_endpoint: true,
        deep_verify: deepVerify,
        preset,
      });
      if (!r?.ok) {
        toast('Run failed: ' + (r?.err ?? 'error'), 'error');
      } else {
        lastJobId = r.job_id ? String(r.job_id) : '';
        toast(
          lastJobId
            ? `Verified run started · job ${lastJobId}`
            : 'Verified run started — watch progress in the pod dashboard.',
          'success',
        );
        startPolling();
      }
    } catch (e: any) {
      toast('Run failed: ' + (e?.message ?? 'error'), 'error');
    }
    runningVerified = false;
  }

  onMount(async () => {
    try {
      const r = await listSystems();
      if (r?.ok) systems = r.systems ?? [];
    } catch {}
    startPolling();
  });
  onDestroy(() => clearInterval(poll));
</script>

<div class="min-h-screen bg-ink-950 text-ink-100 flex flex-col">
  <PageHeader title="Aeon Bench" subtitle="verified LLM benchmarks · remote SSH path" index="14">
    {#if running}
      <a
        href={dashUrl}
        target="_blank"
        rel="noopener"
        class="btn-primary text-xs py-1 px-2.5 rounded inline-flex items-center gap-1"
      >
        Open Dashboard <span aria-hidden="true">↗</span>
      </a>
    {/if}
  </PageHeader>

  <main class="flex-1 overflow-y-auto px-4 sm:px-6 py-6 w-full max-w-3xl mx-auto space-y-5">
    <div class="flex items-start gap-3">
      <div
        class="shrink-0 w-11 h-11 rounded-sm bg-rose-600/15 border border-rose-500/40 text-rose-300 flex items-center justify-center"
      >
        <Icon name="bench" class="w-6 h-6" />
      </div>
      <div class="text-sm text-ink-300 leading-relaxed">
        Race any open model through <span class="text-rose-300 font-mono">AEON Bench</span> —
        <span class="text-ink-200"
          >pull → verify weights → serve → benchmark → ed25519-sign → submit</span
        >
        to the leaderboard. Deploy the pod on a <span class="text-ink-200">GPU host</span> (co-located)
        or on this Orb as a control plane, then use the
        <span class="text-ink-200">verified remote path</span>: authorize the pod’s SSH key on any
        connected system that is already serving a model, scan endpoints, and launch an attested run
        without re-serving.
      </div>
    </div>

    <!-- 1. Deploy pod -->
    <div class="panel p-4 sm:p-5 space-y-4">
      <h2 class="text-[11px] uppercase tracking-wider text-rose-300/90 font-mono">1 · Deploy pod</h2>
      <div class="grid gap-4 sm:grid-cols-2">
        <label class="block space-y-1">
          <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono">Pod host</span>
          <select
            bind:value={target}
            class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none"
          >
            {#each targetOptions as o}
              <option value={o.id}>{o.label}</option>
            {/each}
          </select>
        </label>
        <label class="block space-y-1">
          <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono"
            >Pre-load model <span class="text-ink-600 normal-case">(optional)</span></span
          >
          <input
            bind:value={hfLink}
            placeholder="org/Model — or pick in dashboard"
            class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none"
          />
        </label>
      </div>
      <div class="grid gap-4 sm:grid-cols-2">
        <label class="block space-y-1">
          <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono"
            >HuggingFace token <span class="text-ink-600 normal-case">(optional)</span></span
          >
          <input
            bind:value={hfToken}
            type="password"
            placeholder="hf_… (gated models)"
            autocomplete="off"
            class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none"
          />
        </label>
        <label class="flex items-center gap-2 pt-6 cursor-pointer select-none">
          <input type="checkbox" bind:checked={wantGpu} class="rounded border-steel-600" />
          <span class="text-[12px] text-ink-300"
            >GPU co-located serve (<code class="text-ink-400">--gpus all</code>)</span
          >
        </label>
      </div>
      {#if !wantGpu}
        <div
          class="rounded-sm border border-sky-500/30 bg-sky-500/10 px-3.5 py-2.5 text-[12px] text-sky-100/90"
        >
          Remote-endpoint mode: the pod will not claim a GPU. Use it to scan / fingerprint / hash-verify
          serves on connected systems (attested via <code>endpoint_verified</code> when no local GPU).
        </div>
      {:else}
        <div
          class="rounded-sm border border-amber-500/30 bg-amber-500/10 px-3.5 py-2.5 text-[12px] text-amber-200/90"
        >
          Target needs NVIDIA + nvidia-container-toolkit for co-located pull → serve → bench.
        </div>
      {/if}

      {#if infoLoading || info}
        <div class="rounded-sm border border-ink-800 bg-ink-950/60 px-3.5 py-2.5 text-[12px] space-y-1.5">
          {#if infoLoading}
            <span class="text-ink-500 font-mono">reading model config…</span>
          {:else if info}
            <div class="flex flex-wrap gap-x-3 gap-y-1 font-mono text-ink-300">
              {#if info.params_b}<span class="text-ink-100">{info.params_b}B</span>{/if}
              <span>quant <span class="text-rose-300">{info.quant ?? 'fp16/bf16'}</span></span>
              {#if info.effective_ctx}<span
                  >ctx <span class="text-ink-100">{Math.round(info.effective_ctx / 1024)}k</span
                  ></span
                >{/if}
            </div>
            {#each info.warnings ?? [] as w}
              <div class="text-amber-300/90">⚠ {w}</div>
            {/each}
          {/if}
        </div>
      {/if}

      <button
        type="button"
        class="text-[12px] text-ink-400 hover:text-cursed-300 font-mono"
        on:click={() => (showAdvanced = !showAdvanced)}
      >
        {showAdvanced ? '▾' : '▸'} Advanced options
      </button>
      {#if showAdvanced}
        <div class="grid gap-3 sm:grid-cols-2 pt-1">
          {#each [['AEON_PORT', 'Dashboard port', '8091'], ['AEON_SYSTEM', 'Hardware label', 'e.g. DGX-Spark'], ['AEON_PAUSE_CONTAINERS', 'Pause container during runs', 'name']] as [key, label, ph]}
            <label class="block space-y-1">
              <span class="text-[11px] text-ink-500 font-mono">{label}</span>
              <input
                bind:value={adv[key]}
                placeholder={ph}
                class="w-full bg-ink-950 border border-ink-800 rounded px-2 py-1.5 text-[13px] font-mono focus:border-steel-600 outline-none"
              />
            </label>
          {/each}
        </div>
      {/if}

      <div class="flex items-center gap-3 pt-1">
        <button
          class="btn-primary text-sm py-2 px-4 rounded inline-flex items-center gap-1.5 disabled:opacity-50"
          on:click={deploy}
          disabled={busy}
        >
          {#if busy}<span
              class="inline-block h-3.5 w-3.5 rounded-full border-2 border-white/50 border-t-transparent animate-spin"
            ></span>{/if}
          {running ? 'Redeploy' : busy ? 'Deploying…' : 'Deploy bench pod'}
        </button>
        {#if running}
          <button class="text-sm text-ink-400 hover:text-red-400 font-mono" on:click={stop}
            >stop pod</button
          >
        {/if}
        <span class="text-[11px] text-ink-500 font-mono ml-auto truncate max-w-[14rem]"
          >→ {targetLabel}</span
        >
      </div>
    </div>

    <!-- deploy status -->
    {#if st && phase !== 'idle'}
      <div
        class="rounded-sm border {phase === 'failed'
          ? 'border-red-800/60'
          : running
            ? 'border-emerald-600/40'
            : 'border-steel-700'} bg-ink-900 p-4 sm:p-5 space-y-3"
      >
        <div class="flex items-center justify-between gap-2">
          <span
            class="font-mono text-sm {phase === 'failed'
              ? 'text-red-300'
              : running
                ? 'text-emerald-300'
                : 'text-amber-300'}"
          >
            {#if running}● Pod running on {targetLabel}{:else if phase === 'failed'}⚠ Deploy failed{:else if phase === 'stopped'}Pod stopped{:else}{phase}…{/if}
          </span>
          {#if busy}<span class="tabular-nums text-[11px] text-ink-400"
              >{PHASE_PCT[phase] ?? 0}%</span
            >{/if}
        </div>
        {#if busy}
          <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
            <div
              class="h-full rounded-full bg-gradient-to-r from-rose-600 to-rose-400 transition-all duration-500"
              style="width:{PHASE_PCT[phase] ?? 0}%"
            ></div>
          </div>
        {/if}
        {#if running}
          <div class="flex flex-wrap items-center gap-2 pt-1">
            <a
              href={dashUrl}
              target="_blank"
              rel="noopener"
              class="btn-primary text-sm py-1.5 px-3 rounded inline-flex items-center gap-1"
              >Open Dashboard <span aria-hidden="true">↗</span></a
            >
            <a
              href={dashUrl}
              target="_blank"
              rel="noopener"
              class="text-[12px] text-cursed-300 hover:text-cursed-200 font-mono break-all"
              >{dashUrl}</a
            >
          </div>
          {#if upd?.update_available}
            <div
              class="flex flex-wrap items-center gap-2 rounded-sm border border-amber-600/40 bg-amber-900/10 px-3 py-2"
            >
              <button
                on:click={updatePod}
                disabled={updating || busy}
                class="text-sm py-1.5 px-3 rounded bg-amber-600 hover:bg-amber-500 text-ink-950 font-medium disabled:opacity-50"
              >
                {updating ? 'Updating…' : 'Update Pod'}
              </button>
              <span class="text-[11px] text-amber-200/90 font-mono">
                newer image{#if upd.deployed && upd.latest}
                  · {upd.deployed} → {upd.latest}{/if}
              </span>
            </div>
          {:else if upd && upd.deployed}
            <p class="text-[11px] text-ink-500 font-mono">Pod up to date ✓ · {upd.deployed}</p>
          {/if}
        {/if}
        {#if st.log}
          <details class="text-[11px]">
            <summary class="cursor-pointer text-ink-500 hover:text-ink-300 font-mono"
              >deploy log</summary
            >
            <pre
              class="mt-1.5 whitespace-pre-wrap break-all font-mono text-[10.5px] leading-snug text-ink-400 bg-ink-950 border border-ink-800 rounded p-2 max-h-56 overflow-y-auto"
            >{st.log}</pre>
          </details>
        {/if}
      </div>
    {/if}

    <!-- 2. Verified remote path -->
    <div class="panel p-4 sm:p-5 space-y-4">
      <h2 class="text-[11px] uppercase tracking-wider text-rose-300/90 font-mono">
        2 · Verified remote run
      </h2>
      <p class="text-[12px] text-ink-400 leading-relaxed">
        Point the running pod at a model already serving on a <strong class="text-ink-200"
          >connected system</strong
        >. The Orb installs the pod’s SSH key, scans for OpenAI-compatible endpoints, and launches
        an attested run (<code class="text-ink-300">hf_link</code> +
        <code class="text-ink-300">serve_url</code> +
        <code class="text-ink-300">remote_host</code> +
        <code class="text-ink-300">verify_endpoint</code>).
      </p>

      {#if !running}
        <div
          class="rounded-sm border border-ink-700 bg-ink-950/50 px-3.5 py-3 text-[12px] text-ink-400"
        >
          Deploy the pod first (section 1) so the Orb can reach its API on port {dashPort}.
        </div>
      {:else if noServeHost}
        <div
          class="rounded-sm border border-rose-700/40 bg-rose-900/10 px-3.5 py-3 text-[12px] text-ink-300 space-y-2"
        >
          <p>No connected systems yet — link a GPU / serve host in the Agent Dashboard.</p>
          <a href="/agent" class="btn-primary text-xs px-3 py-1.5 rounded inline-block"
            >↳ Agent systems</a
          >
        </div>
      {:else}
        <label class="block space-y-1">
          <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono"
            >Serve system (SSH target)</span
          >
          <select
            bind:value={serveSystem}
            class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none"
          >
            {#each serveOptions as o}
              <option value={o.id}>{o.label}</option>
            {/each}
          </select>
        </label>

        <div class="flex flex-wrap gap-2">
          <button
            class="text-sm py-1.5 px-3 rounded border border-steel-600 hover:border-rose-500/60 font-mono disabled:opacity-50"
            on:click={authorizeServe}
            disabled={authorizing || !running}
          >
            {authorizing ? 'Authorizing…' : '1. Authorize pod SSH key'}
          </button>
          <button
            class="text-sm py-1.5 px-3 rounded border border-steel-600 hover:border-rose-500/60 font-mono disabled:opacity-50"
            on:click={scanServe}
            disabled={scanning || !running}
          >
            {scanning ? 'Scanning…' : '2. Scan endpoints'}
          </button>
        </div>
        {#if authorizedNote}
          <p class="text-[11px] text-emerald-400/90 font-mono">{authorizedNote}</p>
        {/if}

        {#if endpoints.length}
          <div class="space-y-2">
            <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono"
              >Live endpoints</span
            >
            {#each endpoints as ep}
              <button
                type="button"
                class="w-full text-left rounded-sm border px-3 py-2 font-mono text-[12px] transition-colors {selectedEp ===
                ep
                  ? 'border-rose-500 bg-rose-900/20'
                  : 'border-ink-700 bg-ink-950 hover:border-steel-500'}"
                on:click={() => pickEndpoint(ep)}
              >
                <div class="text-ink-100 break-all">{ep.url ?? '—'}</div>
                <div class="text-ink-500 mt-0.5">
                  {#if ep.hf_guess}
                    hf <span class="text-rose-300">{ep.hf_guess}</span>
                  {:else}
                    <span class="text-amber-300/80">no hf_guess — paste exact quant repo below</span>
                  {/if}
                  {#if ep.models?.length || ep.model}
                    · model {ep.models?.[0] ?? ep.model}
                  {/if}
                </div>
              </button>
            {/each}
          </div>
        {/if}

        <div class="grid gap-3 sm:grid-cols-2">
          <label class="block space-y-1 sm:col-span-2">
            <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono"
              >HF link (exact quant served)</span
            >
            <input
              bind:value={verifyHf}
              placeholder="org/Exact-Quant-Repo"
              class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none"
            />
          </label>
          <label class="block space-y-1">
            <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono"
              >Endpoint model id</span
            >
            <input
              bind:value={endpointModel}
              placeholder="served model name"
              class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none"
            />
          </label>
          <label class="block space-y-1">
            <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono">Preset</span>
            <select
              bind:value={preset}
              class="w-full bg-ink-950 border border-steel-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none"
            >
              <option value="comprehensive">comprehensive (ranks)</option>
              <option value="hard-bench">hard-bench</option>
              <option value="god-mode">god-mode</option>
            </select>
          </label>
        </div>
        <label class="flex items-center gap-2 cursor-pointer select-none text-[12px] text-ink-300">
          <input type="checkbox" bind:checked={deepVerify} class="rounded border-steel-600" />
          Deep-verify (sha256 running container weights over SSH · slower ·
          <code>endpoint_verified</code>)
        </label>

        <button
          class="btn-primary text-sm py-2 px-4 rounded inline-flex items-center gap-1.5 disabled:opacity-50"
          on:click={runVerified}
          disabled={runningVerified || !running || !selectedEp}
        >
          {#if runningVerified}<span
              class="inline-block h-3.5 w-3.5 rounded-full border-2 border-white/50 border-t-transparent animate-spin"
            ></span>{/if}
          {runningVerified ? 'Starting…' : '3. Run verified bench'}
        </button>
        {#if lastJobId}
          <p class="text-[12px] text-emerald-300 font-mono">
            job_id: {lastJobId} · watch in dashboard or poll jobs
          </p>
        {/if}
        {#if jobsSnap}
          <details class="text-[11px]">
            <summary class="cursor-pointer text-ink-500 hover:text-ink-300 font-mono"
              >pod jobs snapshot</summary
            >
            <pre
              class="mt-1.5 whitespace-pre-wrap break-all font-mono text-[10.5px] text-ink-400 bg-ink-950 border border-ink-800 rounded p-2 max-h-48 overflow-y-auto"
            >{JSON.stringify(jobsSnap, null, 2)}</pre>
          </details>
        {/if}
      {/if}
    </div>

    <div
      class="rounded-sm border border-cursed-700/40 bg-cursed-900/10 p-4 sm:p-5 flex flex-col sm:flex-row sm:items-center gap-3"
    >
      <div class="flex-1 text-sm text-ink-300 leading-relaxed">
        <span class="text-cursed-300 font-mono">Build your own Orb.</span> Join the
        <span class="text-cursed-200">AeonForge Patreon</span> for full compiled images.
      </div>
      <a
        href="https://www.patreon.com/AeonForge7/posts/happy-4th-of-162921493"
        target="_blank"
        rel="noopener"
        class="btn-primary text-sm py-2 px-4 rounded shrink-0 inline-flex items-center gap-1.5 self-start sm:self-auto"
      >
        Join the Patreon <span aria-hidden="true">↗</span>
      </a>
    </div>
  </main>
</div>
