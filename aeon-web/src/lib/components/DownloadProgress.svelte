<script lang="ts">
  // A compact progress readout for an in-flight Model Share task. When the
  // backend knows the total size it reports `pct` + bytes and we render a real
  // violet progress bar; otherwise we fall back to a spinner + bytes-pulled so
  // the user still sees motion. An "error: …" phase renders in red, no bar.
  export let task: {
    phase: string;
    pct?: number | null;
    done_bytes?: number;
    total_bytes?: number;
  };
  export let label = '';

  function fmt(n: number): string {
    if (!n || n < 0) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0,
      x = n;
    while (x >= 1024 && i < u.length - 1) {
      x /= 1024;
      i++;
    }
    return `${x.toFixed(x < 10 && i > 0 ? 1 : 0)} ${u[i]}`;
  }

  $: phase = task?.phase ?? '';
  $: isErr = phase.startsWith('error');
  $: pct = typeof task?.pct === 'number' ? Math.max(0, Math.min(100, task.pct)) : null;
  $: hasBar = pct !== null && !isErr;
  $: done = task?.done_bytes ?? 0;
  $: total = task?.total_bytes ?? 0;
</script>

<div class="w-full space-y-1">
  <div class="flex items-center gap-2 text-xs font-mono {isErr ? 'text-red-400' : 'text-amber-300'}">
    {#if !isErr && !hasBar}
      <span class="inline-block h-3 w-3 shrink-0 rounded-full border-2 border-amber-400 border-t-transparent animate-spin"></span>
    {/if}
    {#if label}
      <span class="truncate max-w-[12rem] text-ink-300" title={label}>{label}</span>
      <span class="text-ink-600">·</span>
    {/if}
    <span class="truncate">{isErr ? phase.replace(/^error:\s*/, '⚠ ') : phase}</span>
    {#if hasBar}
      <span class="ml-auto tabular-nums text-cursed-300">{pct}%</span>
    {:else if !isErr && done > 0}
      <span class="ml-auto tabular-nums text-ink-500">{fmt(done)}</span>
    {/if}
  </div>

  {#if hasBar}
    <div class="h-2 rounded-full bg-ink-800 overflow-hidden">
      <div
        class="h-full rounded-full bg-gradient-to-r from-cursed-600 via-cursed-400 to-cursed-300 transition-all duration-500 ease-out"
        style="width:{pct}%; box-shadow:0 0 10px #8b5cf699"
      ></div>
    </div>
    {#if total > 0}
      <div class="text-[10px] text-ink-500 font-mono tabular-nums">{fmt(done)} / {fmt(total)}</div>
    {/if}
  {/if}
</div>
