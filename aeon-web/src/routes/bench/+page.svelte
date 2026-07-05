<script lang="ts">
  // Aeon Bench — deploy the Aeon Bench Pod (open LLM benchmarking: pull → verify
  // → serve → benchmark → sign → submit to the aeon-bench.com leaderboard). The
  // pod serves the model + runs the benchmark CO-LOCATED on one host (it hits
  // http://127.0.0.1:8000, network_mode: host — no external-endpoint override),
  // and serving needs an NVIDIA GPU. So the pod deploys onto a GPU server linked
  // in the Agent Dashboard; this Orb is the console/gateway to its dashboard
  // (:8080), which also opens straight in a browser.
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
    type ConnectedSystem,
    type BenchStatus,
  } from '$lib/api';

  let systems: ConnectedSystem[] = [];
  let target = '';
  let hfLink = '';
  let hfToken = '';
  let maxLen = '65536'; // 64k — the Hermes agentic harness needs it; the pod won't go lower
  let showAdvanced = false;
  let adv = { AEON_QUANT: '', AEON_MAX_TOKENS: '', AEON_DASH_PORT: '', AEON_MOTHERSHIP: '' };

  let st: BenchStatus | null = null;
  let deploying = false;
  let lastTarget = '';
  let poll: ReturnType<typeof setInterval>;

  // Valid deploy targets are GPU servers. On the headless Orb-server build the
  // host itself can serve (if it has a GPU), so "local" is offered there; on the
  // Pi console it never is — the pod goes to a connected box.
  $: targetOptions = [
    ...($serverMode ? [{ id: 'local', label: 'This server (local)' }] : []),
    ...systems.map((s) => ({ id: s.id, label: `${s.label} · ${s.address}${s.roles?.includes('dgx') ? ' · GPU' : ''}` })),
  ];
  $: noHost = targetOptions.length === 0;
  // Pick a default target once options exist; refresh whenever it changes.
  $: if (!target && targetOptions.length) target = targetOptions[0].id;
  $: if (target && target !== lastTarget) {
    lastTarget = target;
    st = null;
    refresh();
  }

  const ACTIVE = ['queued', 'cloning', 'updating', 'installing-docker', 'building'];
  $: phase = st?.phase ?? 'idle';
  $: busy = deploying || ACTIVE.includes(phase);
  $: running = !!st?.running || phase === 'running';
  $: dashPort = st?.dash_port ?? (Number(adv.AEON_DASH_PORT) || 8080);
  $: dashHost = st?.host ?? (typeof window !== 'undefined' ? window.location.hostname : 'localhost');
  $: dashUrl = `http://${dashHost}:${dashPort}`;
  $: targetLabel = targetOptions.find((o) => o.id === target)?.label ?? target;

  const PHASE_PCT: Record<string, number> = {
    queued: 5, cloning: 15, updating: 15, 'installing-docker': 30, building: 65, running: 100, failed: 100, stopped: 0, idle: 0,
  };

  async function refresh() {
    if (!target) return;
    try {
      const r = await benchStatus(target);
      if (r?.ok) st = r;
    } catch {}
  }
  function startPolling() {
    clearInterval(poll);
    poll = setInterval(refresh, 3000);
  }

  async function deploy() {
    if (!/^[\w.\-]+\/[\w.\-]+$/.test(hfLink.trim())) {
      toast('Enter a HuggingFace model id like "org/model".', 'error');
      return;
    }
    const len = Number(maxLen) || 65536;
    if (len < 65536) {
      toast('Context length must be at least 64k (65536) for the Hermes harness.', 'error');
      return;
    }
    deploying = true;
    const env: Record<string, string> = { AEON_MAX_MODEL_LEN: String(len) };
    for (const [k, v] of Object.entries(adv)) if (v.trim()) env[k] = v.trim();
    try {
      const r = await benchDeploy({ target, hf_link: hfLink.trim(), hf_token: hfToken.trim() || undefined, env });
      if (!r?.ok) {
        toast('Deploy failed: ' + (r?.err ?? 'unknown error'), 'error');
        deploying = false;
        return;
      }
      toast(`Deploying the bench pod on ${targetLabel}…`, 'success');
      await refresh();
      startPolling();
    } catch (e: any) {
      toast('Deploy failed: ' + (e?.message ?? 'error'), 'error');
    }
    deploying = false;
  }

  async function stop() {
    if (!(await confirmRite({ title: 'Stop the bench pod?', body: `This runs "docker compose down" on ${targetLabel}.`, confirmLabel: 'Stop pod', danger: true }))) return;
    try {
      const r = await benchStop(target);
      if (r?.ok) { toast('Pod stopped.', 'info'); await refresh(); }
      else toast('Stop failed: ' + (r?.err ?? 'error'), 'error');
    } catch (e: any) { toast('Stop failed: ' + (e?.message ?? 'error'), 'error'); }
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
  <PageHeader title="Aeon Bench" subtitle="deploy the LLM-benchmarking pod">
    {#if running}
      <a href={dashUrl} target="_blank" rel="noopener" class="btn-primary text-xs py-1 px-2.5 rounded inline-flex items-center gap-1">
        Open Dashboard <span aria-hidden="true">↗</span>
      </a>
    {/if}
  </PageHeader>

  <main class="flex-1 overflow-y-auto px-4 sm:px-6 py-6 w-full max-w-3xl mx-auto space-y-5">
    <!-- intro -->
    <div class="flex items-start gap-3">
      <div class="shrink-0 w-11 h-11 rounded-lg bg-rose-600/15 border border-rose-500/40 text-rose-300 flex items-center justify-center">
        <Icon name="bench" class="w-6 h-6" />
      </div>
      <div class="text-sm text-ink-300 leading-relaxed">
        Race any open model through the <span class="text-rose-300 font-mono">AEON Bench</span> suite —
        <span class="text-ink-200">pull → verify weights → serve → benchmark (text · agentic · vision · audio · arena · perf) → ed25519-sign → submit</span>
        to the global leaderboard. The pod deploys onto a <span class="text-ink-200">GPU server</span> in your Agent Dashboard; this Orb is your
        gateway to its dashboard — open it here or straight in a browser.
      </div>
    </div>

    {#if noHost}
      <!-- No GPU server linked — the pod has nowhere to serve the model. -->
      <div class="rounded-xl border border-rose-700/40 bg-rose-900/10 p-5 text-center space-y-3">
        <div class="text-sm text-ink-200 leading-relaxed max-w-md mx-auto">
          <span class="text-rose-300 font-mono">No model host connected.</span>
          Aeon Bench serves the model <em>and</em> runs the benchmark together on one <strong>NVIDIA-GPU</strong> box —
          the Pi has no GPU, so link a DGX / gateway to run it on.
        </div>
        <a href="/agent" class="inline-block btn-primary text-sm px-4 py-2 rounded">↳ Connect to model host</a>
        <p class="text-[11px] text-ink-500">Add a system in the Agent Dashboard, then come back and deploy.</p>
      </div>
    {:else}
      <!-- GPU note -->
      <div class="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3.5 py-2.5 text-[12px] text-amber-200/90 flex gap-2">
        <span aria-hidden="true">⚠</span>
        <span>The pod serves the model + runs the benchmark <strong>co-located on the target</strong> — so the deploy target must be an <strong>NVIDIA-GPU</strong> server. Docker is installed there on first deploy.</span>
      </div>

      <!-- deploy form -->
      <div class="rounded-xl border border-ink-700 bg-ink-900 p-4 sm:p-5 space-y-4">
        <div class="grid gap-4 sm:grid-cols-2">
          <label class="block space-y-1">
            <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono">Deploy target — GPU host</span>
            <select bind:value={target} class="w-full bg-ink-950 border border-ink-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none">
              {#each targetOptions as o}
                <option value={o.id}>{o.label}</option>
              {/each}
            </select>
          </label>
          <label class="block space-y-1">
            <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono">Model — HuggingFace id</span>
            <input bind:value={hfLink} placeholder="org/Model-Name" class="w-full bg-ink-950 border border-ink-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none" />
          </label>
        </div>
        <div class="grid gap-4 sm:grid-cols-2">
          <label class="block space-y-1">
            <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono">Context length</span>
            <input bind:value={maxLen} inputmode="numeric" class="w-full bg-ink-950 border border-ink-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none" />
            <span class="text-[10.5px] text-ink-500">64k (65536) minimum — required for the Hermes agentic harness.</span>
          </label>
          <label class="block space-y-1">
            <span class="text-[11px] uppercase tracking-wider text-ink-400 font-mono">HuggingFace token <span class="text-ink-600 normal-case">(optional)</span></span>
            <input bind:value={hfToken} type="password" placeholder="hf_… (for gated models)" autocomplete="off" class="w-full bg-ink-950 border border-ink-700 rounded px-2.5 py-2 text-sm font-mono focus:border-rose-500 outline-none" />
          </label>
        </div>

        <button type="button" class="text-[12px] text-ink-400 hover:text-cursed-300 font-mono" on:click={() => (showAdvanced = !showAdvanced)}>
          {showAdvanced ? '▾' : '▸'} Advanced options
        </button>
        {#if showAdvanced}
          <div class="grid gap-3 sm:grid-cols-2 pt-1">
            {#each [['AEON_QUANT', 'Quantization', 'e.g. awq'], ['AEON_MAX_TOKENS', 'Max tokens', '2048'], ['AEON_DASH_PORT', 'Dashboard port', '8080'], ['AEON_MOTHERSHIP', 'Mothership URL', 'https://aeon-bench.com']] as [key, label, ph]}
              <label class="block space-y-1">
                <span class="text-[11px] text-ink-500 font-mono">{label}</span>
                <input bind:value={adv[key]} placeholder={ph} class="w-full bg-ink-950 border border-ink-800 rounded px-2 py-1.5 text-[13px] font-mono focus:border-ink-600 outline-none" />
              </label>
            {/each}
          </div>
        {/if}

        <div class="flex items-center gap-3 pt-1">
          <button class="btn-primary text-sm py-2 px-4 rounded inline-flex items-center gap-1.5 disabled:opacity-50" on:click={deploy} disabled={busy}>
            {#if busy}<span class="inline-block h-3.5 w-3.5 rounded-full border-2 border-white/50 border-t-transparent animate-spin"></span>{/if}
            {running ? 'Redeploy' : busy ? 'Deploying…' : 'Deploy bench pod'}
          </button>
          {#if running}
            <button class="text-sm text-ink-400 hover:text-red-400 font-mono" on:click={stop}>stop pod</button>
          {/if}
          <span class="text-[11px] text-ink-500 font-mono ml-auto truncate max-w-[14rem]">→ {targetLabel}</span>
        </div>
      </div>

      <!-- status / progress -->
      {#if st && phase !== 'idle'}
        <div class="rounded-xl border {phase === 'failed' ? 'border-red-800/60' : running ? 'border-emerald-600/40' : 'border-ink-700'} bg-ink-900 p-4 sm:p-5 space-y-3">
          <div class="flex items-center justify-between gap-2">
            <span class="font-mono text-sm {phase === 'failed' ? 'text-red-300' : running ? 'text-emerald-300' : 'text-amber-300'}">
              {#if running}● Pod running on {targetLabel}{:else if phase === 'failed'}⚠ Deploy failed{:else if phase === 'stopped'}Pod stopped{:else}{phase}…{/if}
            </span>
            {#if busy}<span class="tabular-nums text-[11px] text-ink-400">{PHASE_PCT[phase] ?? 0}%</span>{/if}
          </div>
          {#if busy}
            <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
              <div class="h-full rounded-full bg-gradient-to-r from-rose-600 to-rose-400 transition-all duration-500" style="width:{PHASE_PCT[phase] ?? 0}%"></div>
            </div>
            <p class="text-[11px] text-ink-500">Building the pod images + pulling weights can take several minutes on the first run.</p>
          {/if}

          {#if running}
            <div class="flex flex-wrap items-center gap-2 pt-1">
              <a href={dashUrl} target="_blank" rel="noopener" class="btn-primary text-sm py-1.5 px-3 rounded inline-flex items-center gap-1">Open Dashboard <span aria-hidden="true">↗</span></a>
              <a href={dashUrl} target="_blank" rel="noopener" class="text-[12px] text-cursed-300 hover:text-cursed-200 font-mono break-all">{dashUrl}</a>
            </div>
          {/if}

          {#if st.log}
            <details class="text-[11px]">
              <summary class="cursor-pointer text-ink-500 hover:text-ink-300 font-mono">deploy log</summary>
              <pre class="mt-1.5 whitespace-pre-wrap break-all font-mono text-[10.5px] leading-snug text-ink-400 bg-ink-950 border border-ink-800 rounded p-2 max-h-56 overflow-y-auto">{st.log}</pre>
            </details>
          {/if}
        </div>
      {/if}
    {/if}

    <!-- Patreon -->
    <div class="rounded-xl border border-cursed-700/40 bg-cursed-900/10 p-4 sm:p-5 flex flex-col sm:flex-row sm:items-center gap-3">
      <div class="flex-1 text-sm text-ink-300 leading-relaxed">
        <span class="text-cursed-300 font-mono">Build your own Orb.</span> Join the
        <span class="text-cursed-200">AeonForge Patreon</span> for the full compiled image files (flash straight to your Raspberry Pi),
        complete build instructions, and a 3D-print enclosure file for the full build.
      </div>
      <a href="https://www.patreon.com/AeonForge7/posts/happy-4th-of-162921493" target="_blank" rel="noopener"
         class="btn-primary text-sm py-2 px-4 rounded shrink-0 inline-flex items-center gap-1.5 self-start sm:self-auto">
        Join the Patreon <span aria-hidden="true">↗</span>
      </a>
    </div>
  </main>
</div>
