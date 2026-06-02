<script lang="ts">
  // v66: flat-styled brand marks for the three VPN providers.
  //
  // These are deliberately *simplified, flat* renditions in each
  // brand's primary color — recognizable identity for the wizard tabs
  // and provider cards without shipping (or misrepresenting) the
  // vendors' official trademarked logo assets. Pure inline SVG: no
  // network fetch, no external files, scales crisply at any size.
  //
  //   mullvad  — signature yellow tile + the mole-face motif (two eyes)
  //   ivpn     — blue security shield
  //   azirevpn — blue tile with an "A" monogram
  export let provider: string;
  export let size: number = 24;
  /// Desaturate slightly when the logo sits on an inactive tab so the
  /// active provider's mark pops.
  export let muted: boolean = false;

  // Brand primary colors (approximate, flat).
  const BG: Record<string, string> = {
    mullvad: '#ffd524',
    ivpn: '#4a78e0',
    azirevpn: '#2f6fe0',
    airvpn: '#16a085',
  };
  $: bg = BG[provider] ?? '#6b7280';
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 32 32"
  role="img"
  aria-label="{provider} logo"
  style="flex-shrink:0; {muted ? 'filter:grayscale(0.55) opacity(0.7);' : ''}"
>
  {#if provider === 'mullvad'}
    <!-- yellow rounded tile + simplified mole face -->
    <rect x="1" y="1" width="30" height="30" rx="7" fill={bg} />
    <ellipse cx="16" cy="18" rx="8.5" ry="7" fill="#1a1a1a" />
    <circle cx="12.5" cy="16.5" r="1.7" fill={bg} />
    <circle cx="19.5" cy="16.5" r="1.7" fill={bg} />
    <ellipse cx="16" cy="21.5" rx="2.2" ry="1.5" fill={bg} />
  {:else if provider === 'ivpn'}
    <!-- flat security shield -->
    <path
      d="M16 2 L28 6 V15 C28 23 22 28 16 30 C10 28 4 23 4 15 V6 Z"
      fill={bg}
    />
    <path
      d="M11 16 l3.5 3.5 L22 12"
      fill="none"
      stroke="#ffffff"
      stroke-width="2.6"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
  {:else if provider === 'azirevpn'}
    <!-- blue rounded tile + A monogram -->
    <rect x="1" y="1" width="30" height="30" rx="7" fill={bg} />
    <path
      d="M16 7 L24 25 H20.3 L18.7 21 H13.3 L11.7 25 H8 Z M14.4 18 H17.6 L16 13.8 Z"
      fill="#ffffff"
    />
  {:else if provider === 'airvpn'}
    <!-- teal rounded tile + flat cloud over "air" wind lines -->
    <rect x="1" y="1" width="30" height="30" rx="7" fill={bg} />
    <circle cx="13" cy="15" r="4.6" fill="#ffffff" />
    <circle cx="18.5" cy="15.5" r="3.6" fill="#ffffff" />
    <rect x="11" y="15" width="10.5" height="4.6" rx="2.3" fill="#ffffff" />
    <rect x="9.5" y="22" width="13" height="1.6" rx="0.8" fill="#ffffff" opacity="0.9" />
    <rect x="12.5" y="25" width="8" height="1.4" rx="0.7" fill="#ffffff" opacity="0.65" />
  {:else}
    <!-- fallback generic VPN tile -->
    <rect x="1" y="1" width="30" height="30" rx="7" fill={bg} />
    <text x="16" y="21" text-anchor="middle" font-size="13"
          font-family="monospace" fill="#ffffff">VPN</text>
  {/if}
</svg>
