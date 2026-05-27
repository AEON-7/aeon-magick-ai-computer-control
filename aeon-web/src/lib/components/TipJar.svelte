<script lang="ts">
  // Tip-the-developer widget. Lives at the bottom of settings pages.
  // Collapsed by default to stay non-intrusive.
  //
  // Each wallet renders as a row: chain glyph, label, truncated address,
  // copy button, "open in wallet" deep-link button, and an inline QR
  // svg. QR codes are generated client-side via qrcode-generator —
  // no network round-trip, no third-party scan service.

  import qrcodeGenerator from 'qrcode-generator';

  type Wallet = {
    id: string;
    label: string;
    glyph: string;          // unicode chain glyph (kept ASCII-safe)
    address: string;
    uriScheme: string;      // BIP21-style "name:" prefix
    color: string;          // tailwind class for the glyph accent
  };

  // Curated set — matches the addresses on AEON-7's GitHub.
  const wallets: Wallet[] = [
    {
      id: 'btc',
      label: 'Bitcoin',
      glyph: '₿',
      address: 'bc1q09xmzn00q4z3c5raene0f3pzn9d9pvawfm0py4',
      uriScheme: 'bitcoin',
      color: 'text-amber-400',
    },
    {
      id: 'eth',
      label: 'Ethereum',
      glyph: 'Ξ',
      address: '0x1512667F6D61454ad531d2E45C0a5d1fd82D0500',
      uriScheme: 'ethereum',
      color: 'text-indigo-300',
    },
    {
      id: 'sol',
      label: 'Solana',
      glyph: '◎',
      address: 'DgQsjHdAnT5PNLQTNpJdpLS3tYGpVcsHQCkpoiAKsw8t',
      uriScheme: 'solana',
      color: 'text-fuchsia-400',
    },
    {
      id: 'xmr',
      label: 'Monero',
      glyph: 'ⓜ',
      address:
        '836XrSKw4R76vNi3QPJ5Fa9ugcyvE2cWmKSPv3AhpTNNKvqP8v5ba9JRL4Vh7UnFNjDz3E2GXZDVVenu3rkZaNdUFhjAvgd',
      uriScheme: 'monero',
      color: 'text-orange-400',
    },
  ];

  // The bare URI scheme (BIP21 / Solana Pay / Monero URI / EIP-681).
  // Used as the QR-encoded payload (every standards-compliant wallet
  // scanner consumes this form).
  function uriFor(w: Wallet): string {
    return `${w.uriScheme}:${w.address}`;
  }

  // What the "open" button should navigate to. For chains where the URI
  // scheme has a widely-registered desktop handler (Bitcoin Core / Trezor
  // Suite / Sparrow for BTC; Monero GUI / Cake / Feather for XMR), the
  // bare scheme works fine — every modern OS dispatches it to the wallet.
  //
  // Ethereum and Solana are typically held in browser-extension wallets
  // (MetaMask, Phantom) with no OS-level scheme handler, so clicking
  // `ethereum:0x...` on Safari just throws "address is invalid". For
  // those, we use the wallet's universal/app link which:
  //   • opens the wallet directly if installed (deep link)
  //   • falls back to the wallet's web page if not (downloads / wallet
  //     connect QR), so the user never hits a dead-end error dialog.
  function openUrlFor(w: Wallet): string {
    switch (w.id) {
      case 'eth':
        // MetaMask universal link — `@1` pins to Ethereum mainnet.
        // Works on desktop + mobile + as a web-page fallback.
        return `https://metamask.app.link/send/${w.address}@1`;
      case 'sol':
        // Phantom's universal link "send" route. Recipient pre-fills
        // the destination address; user picks amount + token.
        return `https://phantom.app/ul/v1/send?recipient=${encodeURIComponent(w.address)}`;
      default:
        return uriFor(w);
    }
  }

  // Build an inline SVG <path> from qrcode-generator's module map.
  // Avoids embedding an <img src=data:…> blob and keeps the QR crisp
  // at any size via CSS.
  function qrSvg(text: string, sizePx = 96): string {
    // typeNumber=0 → auto, ECC=L. Monero addresses are 95 chars so QR
    // gets dense; L-level keeps the modules at a readable size.
    const qr = qrcodeGenerator(0, 'L');
    qr.addData(text);
    qr.make();
    const modules = qr.getModuleCount();
    const cell = sizePx / modules;
    const rects: string[] = [];
    for (let r = 0; r < modules; r++) {
      for (let c = 0; c < modules; c++) {
        if (qr.isDark(r, c)) {
          rects.push(
            `<rect x="${(c * cell).toFixed(2)}" y="${(r * cell).toFixed(2)}" ` +
              `width="${cell.toFixed(2)}" height="${cell.toFixed(2)}" />`,
          );
        }
      }
    }
    return (
      `<svg xmlns="http://www.w3.org/2000/svg" width="${sizePx}" height="${sizePx}" ` +
      `viewBox="0 0 ${sizePx} ${sizePx}" shape-rendering="crispEdges">` +
      `<rect width="${sizePx}" height="${sizePx}" fill="#e5e5e5"/>` +
      `<g fill="#0a0a0a">${rects.join('')}</g></svg>`
    );
  }

  let copied: string | null = null;
  async function copy(addr: string, id: string) {
    try {
      await navigator.clipboard.writeText(addr);
      copied = id;
      setTimeout(() => (copied = null), 1500);
    } catch (e) {
      // Older browsers / non-secure contexts — fall back to a manual
      // hint instead of silently failing.
      alert('Couldn\'t copy automatically. Address:\n\n' + addr);
    }
  }

  function truncate(addr: string): string {
    return addr.length > 26 ? `${addr.slice(0, 12)}…${addr.slice(-10)}` : addr;
  }
</script>

<details open class="mt-8 bg-gradient-to-br from-cursed-900/40 via-ink-900/70 to-fuchsia-900/30
                     border border-cursed-500/40 rounded-xl group
                     shadow-[0_0_25px_rgba(217,70,239,0.18)]
                     hover:shadow-[0_0_35px_rgba(217,70,239,0.30)]
                     transition-shadow">
  <summary
    class="cursor-pointer select-none px-5 py-3 flex items-center justify-between
           text-cursed-200 hover:text-cursed-100 transition-colors"
  >
    <span class="font-mono text-sm uppercase tracking-wider flex items-center gap-2">
      <span class="text-cursed-400 text-lg animate-pulse">♥</span>
      tip the developer
      <span class="text-[10px] text-zinc-400 normal-case font-sans">— one person, spare time, no VC</span>
    </span>
    <span class="text-[10px] font-mono opacity-70 group-open:hidden">
      expand
    </span>
    <span class="text-[10px] font-mono opacity-70 hidden group-open:inline">
      collapse
    </span>
  </summary>

  <div class="border-t border-ink-800 p-5 space-y-3">
    <p class="text-xs text-zinc-500 leading-relaxed">
      Aeon Magick is built and maintained by one person, in spare time, with
      no funding behind it. If it's saved you a hardware purchase or
      a debugging headache, a tip of any size keeps the next persona /
      pipeline / kernel quirk fix coming. None of these prompts will ever
      gate functionality — the project is and stays free.
    </p>

    <div class="space-y-2">
      {#each wallets as w (w.id)}
        <div
          class="flex items-center gap-3 p-3 rounded-lg bg-ink-950 border border-ink-800
                 hover:border-ink-700 transition-colors"
        >
          <!-- Glyph -->
          <div class="flex-shrink-0 w-9 h-9 rounded-md bg-ink-900 border border-ink-800
                      flex items-center justify-center text-lg {w.color} font-bold">
            {w.glyph}
          </div>

          <!-- Label + address -->
          <div class="flex-1 min-w-0 space-y-0.5">
            <div class="text-zinc-300 text-xs font-medium">{w.label}</div>
            <div class="text-[10px] font-mono text-zinc-500 truncate"
                 title={w.address}>
              {truncate(w.address)}
            </div>
          </div>

          <!-- Actions -->
          <div class="flex items-center gap-1.5">
            <button
              class="text-[10px] font-mono px-2 py-1 rounded
                     border border-ink-700 hover:border-cursed-500
                     text-zinc-400 hover:text-cursed-300 transition-colors"
              on:click={() => copy(w.address, w.id)}
              title="Copy address"
            >
              {copied === w.id ? '✓ copied' : 'copy'}
            </button>
            <a
              class="text-[10px] font-mono px-2 py-1 rounded
                     border border-ink-700 hover:border-cursed-500
                     text-zinc-400 hover:text-cursed-300 transition-colors"
              href={openUrlFor(w)}
              target={w.id === 'eth' || w.id === 'sol' ? '_blank' : undefined}
              rel={w.id === 'eth' || w.id === 'sol' ? 'noreferrer' : undefined}
              title={w.id === 'eth'
                ? 'Opens MetaMask if installed; otherwise the MetaMask web page (pre-filled recipient).'
                : w.id === 'sol'
                ? 'Opens Phantom if installed; otherwise the Phantom web page (pre-filled recipient).'
                : 'Open in installed wallet'}
            >
              open
            </a>
          </div>

          <!-- QR -->
          <div class="flex-shrink-0 w-12 h-12 rounded bg-zinc-200 p-0.5"
               title="Scan with a wallet app">
            {@html qrSvg(uriFor(w), 48)}
          </div>
        </div>
      {/each}
    </div>

    <p class="text-[10px] text-zinc-600 leading-relaxed pt-1">
      Addresses verified against the maintainer's
      <span class="font-mono text-zinc-500">AEON-7</span> profile.
      QR encodes the full payment URI — scanning with any standards-compliant
      wallet pre-fills the recipient. Click <span class="font-mono">open</span>
      to launch your installed wallet via the chain's URI scheme
      (<span class="font-mono">bitcoin:</span>, <span class="font-mono">ethereum:</span>,
      <span class="font-mono">solana:</span>, <span class="font-mono">monero:</span>).
    </p>
  </div>
</details>
