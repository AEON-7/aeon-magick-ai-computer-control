<script lang="ts">
  // First-boot WiFi setup wizard. Shown when the device is in AP fallback
  // mode (no known networks reachable). The supervisor will redirect here
  // from "/" when /api/state reports `mode == "ap-fallback"`. For now this
  // is a static form posting to /api/wifi/configure on the device.

  let ssid = '';
  let password = '';
  let busy = false;
  let result = '';

  async function submit() {
    busy = true;
    result = '';
    try {
      const r = await fetch('/api/wifi/configure', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ssid, password }),
      });
      if (!r.ok) {
        const t = await r.text();
        throw new Error(t.slice(0, 200));
      }
      result = 'Saved. Device will switch to your WiFi in ~10 seconds.';
    } catch (e: any) {
      result = `Error: ${e?.message ?? 'unknown'}`;
    } finally {
      busy = false;
    }
  }
</script>

<div class="h-full flex items-center justify-center p-6">
  <form
    on:submit|preventDefault={submit}
    class="w-full max-w-md bg-ink-900 border border-ink-700 rounded-2xl p-8 space-y-5"
  >
    <h1 class="text-cursed-400 font-mono text-lg tracking-widest">AEON MAGICK AI COMPUTER CONTROL — SETUP</h1>
    <p class="text-zinc-400 text-sm">
      You're connected to the device's setup AP. Configure WiFi to bring it onto
      your network. After joining, the device will go online and this AP closes.
    </p>

    <label class="block">
      <span class="text-xs uppercase tracking-wider text-zinc-500">SSID</span>
      <input
        type="text"
        bind:value={ssid}
        class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-ink-700
               focus:outline-none focus:ring-2 focus:ring-cursed-500"
      />
    </label>

    <label class="block">
      <span class="text-xs uppercase tracking-wider text-zinc-500">password</span>
      <input
        type="password"
        bind:value={password}
        class="mt-1 w-full px-3 py-2 rounded-md bg-ink-800 border border-ink-700
               focus:outline-none focus:ring-2 focus:ring-cursed-500"
      />
    </label>

    <button type="submit" disabled={busy || !ssid} class="btn-primary w-full disabled:opacity-50">
      {busy ? 'saving…' : 'connect'}
    </button>

    {#if result}
      <p class="text-sm {result.startsWith('Error') ? 'text-red-400' : 'text-live-400'}">{result}</p>
    {/if}
  </form>
</div>
