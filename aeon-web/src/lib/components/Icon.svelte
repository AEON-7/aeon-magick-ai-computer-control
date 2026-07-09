<script lang="ts">
  // v74: tiny inline-SVG icon set for the launcher + menus. Stroke uses
  // currentColor so icons inherit the surrounding text colour (the purple
  // "cursed" theme, status greens/ambers/reds, etc.) with zero per-icon
  // styling. Line-style (Lucide-ish) 24×24 geometry — sized via `class`.
  export let name: string;
  let cls = 'w-4 h-4';
  export { cls as class };

  // name → inner SVG markup. Kept as static trusted strings (no user input).
  const P: Record<string, string> = {
    monitor: '<rect x="2" y="3" width="20" height="14" rx="2"/><path d="M8 21h8M12 17v4"/>',
    globe: '<circle cx="12" cy="12" r="10"/><path d="M2 12h20"/><path d="M12 2a15 15 0 0 1 0 20 15 15 0 0 1 0-20"/>',
    wifi: '<path d="M2 8.8a15 15 0 0 1 20 0"/><path d="M5 12.5a10 10 0 0 1 14 0"/><path d="M8.5 16a5 5 0 0 1 7 0"/><path d="M12 20h.01"/>',
    shield: '<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>',
    funnel: '<path d="M22 3H2l8 9.5V19l4 2v-8.5L22 3z"/>',
    list: '<path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/>',
    folder: '<path d="M4 4h5l2 3h9a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z"/>',
    disc: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2.5"/>',
    key: '<circle cx="7.5" cy="15.5" r="5"/><path d="M11 12l9-9"/><path d="M16 7l3 3"/><path d="M19 4l2 2"/>',
    braces: '<path d="M8 3H7a2 2 0 0 0-2 2v4a2 2 0 0 1-2 2 2 2 0 0 1 2 2v4a2 2 0 0 0 2 2h1"/><path d="M16 3h1a2 2 0 0 1 2 2v4a2 2 0 0 0 2 2 2 2 0 0 0-2 2v4a2 2 0 0 1-2 2h-1"/>',
    cpu: '<rect x="5" y="5" width="14" height="14" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 2v2M15 2v2M9 20v2M15 20v2M2 9h2M2 15h2M20 9h2M20 15h2"/>',
    chip: '<rect x="7" y="7" width="10" height="10" rx="1"/><circle cx="12" cy="12" r="1.5"/><path d="M10 2v5M14 2v5M10 17v5M14 17v5M2 10h5M2 14h5M17 10h5M17 14h5"/>',
    orbnet: '<circle cx="12" cy="12" r="2.5"/><circle cx="5" cy="5" r="1.8"/><circle cx="19" cy="5" r="1.8"/><circle cx="5" cy="19" r="1.8"/><circle cx="19" cy="19" r="1.8"/><path d="M6.6 6.6 10 10M17.4 6.6 14 10M6.6 17.4 10 14M17.4 17.4 14 14"/>',
    fleet: '<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/>',
    // share-nodes — three peers linked in a mesh (Model Share / IPFS gossip).
    share: '<circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><path d="M8.6 10.6 15.4 6.4M8.6 13.4 15.4 17.6"/>',
    // Aether — a central orb held in two crossing cosmic orbits, with peer-orbs
    // riding them: the decentralized model network as a constellation. Orbits
    // stroked, orbs filled (currentColor) so it reads sharp at any size.
    aether: '<ellipse cx="12" cy="12" rx="9.3" ry="3.7" transform="rotate(28 12 12)"/><ellipse cx="12" cy="12" rx="9.3" ry="3.7" transform="rotate(-28 12 12)"/><circle cx="12" cy="12" r="2" fill="currentColor" stroke="none"/><circle cx="19.4" cy="8.2" r="1.25" fill="currentColor" stroke="none"/><circle cx="4.6" cy="15.8" r="1.25" fill="currentColor" stroke="none"/>',
    // Aeon Bench — a speedometer/gauge (arc + needle + hub + ticks): performance
    // benchmarking, models raced against the AEON Bench suite for the leaderboard.
    bench: '<path d="M3.5 18a8.5 8.5 0 0 1 17 0"/><path d="M12 18l4.5-4"/><circle cx="12" cy="18" r="1.4" fill="currentColor" stroke="none"/><path d="M12 9.5V11M5 17.4l1.1 .3M19 17.4l-1.1 .3"/>',
    power: '<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.8 0"/>',
    zap: '<path d="M13 2L3 14h9l-1 8 10-12h-9l1-8z"/>',
    reboot: '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/>',
    chevron: '<path d="M6 9l6 6 6-6"/>',
    keyboard: '<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M6 14h.01M18 14h.01M9 14h6"/>',
    // AI chip with a spark inside — a silicon die (rounded square) sprouting
    // pin legs on all four sides, with a lightning spark at its core. Reads
    // as "NPU / accelerated AI".
    spark: '<rect x="7" y="7" width="10" height="10" rx="1.5"/><path d="M9 2v2M15 2v2M9 20v2M15 20v2M2 9h2M2 15h2M20 9h2M20 15h2"/><path d="M12.5 9.5 10.5 12.5h2l-1 2.5 3-3.2h-2.2l1.2-2.3z"/>',
    // BrainCraft HAT: a small TFT screen wearing a friendly face — the Orb's
    // 240x240 display as its "face" (two eyes + a smile), on a stand.
    braincraft: '<rect x="3" y="4" width="18" height="14" rx="2"/><circle cx="9.5" cy="10" r="1"/><circle cx="14.5" cy="10" r="1"/><path d="M9 13.2a3.2 3.2 0 0 0 6 0"/><path d="M9 21h6M12 18v3"/>',
    // Action glyphs (v100) — replace the raw-Unicode buttons (⏏ ⌨ ⛶ ● ☰ …)
    // that rendered differently on every OS. Same 24-grid, currentColor.
    capture: '<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M6 14h.01M9 14h6"/><circle cx="18" cy="13" r="1.5" fill="currentColor" stroke="none"/>',
    release: '<path d="M12 5l7 7H5l7-7z"/><path d="M5 17h14"/>',
    fullscreen: '<path d="M4 9V4h5M15 4h5v5M20 15v5h-5M9 20H4v-5"/>',
    record: '<circle cx="12" cy="12" r="6" fill="currentColor" stroke="none"/>',
    stop: '<rect x="7" y="7" width="10" height="10" rx="1" fill="currentColor" stroke="none"/>',
    menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
    close: '<path d="M6 6l12 12M18 6L6 18"/>',
    lock: '<rect x="5" y="11" width="14" height="9" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>',
    logout: '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="M16 17l5-5-5-5"/><path d="M21 12H9"/>',
    refresh: '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/>',
    enter: '<path d="M20 5v6a3 3 0 0 1-3 3H5"/><path d="M9 10l-4 4 4 4"/>',
    backspace: '<path d="M8 5h12a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H8l-6-7 6-7z"/><path d="M12 10l4 4M16 10l-4 4"/>',
    esc: '<circle cx="12" cy="12" r="9"/><path d="M9 9l6 6M15 9l-6 6"/>',
    tab: '<path d="M3 12h14"/><path d="M13 8l4 4-4 4"/><path d="M21 6v12"/>',

    // ── OrbNet / privacy stack (line icons; communicate the protocol) ──
    // Tor — onion layers (three concentric arcs around a core).
    onion:
      '<circle cx="12" cy="12" r="2" fill="currentColor" stroke="none"/><circle cx="12" cy="12" r="5.5"/><circle cx="12" cy="12" r="9"/>',
    // IPFS — content-addressed cube (isometric box) with a content hash node.
    ipfs:
      '<path d="M12 3l7 4v10l-7 4-7-4V7l7-4z"/><path d="M12 3v18M5 7l7 4 7-4"/><circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none"/>',
    // Mysterium dVPN — peer bandwidth (two endpoints + flowing path).
    mysterium:
      '<circle cx="5" cy="12" r="2.2"/><circle cx="19" cy="12" r="2.2"/><path d="M7.4 12h9.2"/><path d="M9.5 9.2c1.6 1.1 3.4 1.1 5 0M9.5 14.8c1.6-1.1 3.4-1.1 5 0"/>',
    // Matrix homeserver / chat — room + message tail.
    matrix:
      '<path d="M5 5h10a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H9l-4 3v-3H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2z"/><path d="M8 10h6M8 13h4"/>',
    // Encrypted DNS — lock over a small resolver disc.
    dns:
      '<circle cx="12" cy="14" r="6"/><path d="M12 11v3l2 1.2"/><path d="M9 7V5.5a3 3 0 0 1 6 0V7"/><rect x="8" y="7" width="8" height="5" rx="1"/>',
    // Tunnel / I2P — packet entering a tunnel mouth.
    tunnel:
      '<path d="M3 8h8a6 6 0 0 1 0 12H3"/><path d="M3 12h8"/><circle cx="18" cy="12" r="2"/><path d="M15.5 12H11"/>',
    // Mesh / Tailscale — four peers linked.
    mesh:
      '<circle cx="6" cy="6" r="2"/><circle cx="18" cy="6" r="2"/><circle cx="6" cy="18" r="2"/><circle cx="18" cy="18" r="2"/><path d="M8 6h8M6 8v8M18 8v8M8 18h8"/>',
    // Model weights / library — stacked layers with a spark (weights file).
    model:
      '<path d="M4 7h16v3H4zM4 12h16v3H4zM4 17h16v3H4z"/><path d="M8 8.5h.01M8 13.5h.01M8 18.5h.01"/>',
    // VPN tunnel shield variant already have shield; wire = link with keyhole.
    vpn:
      '<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="M9.5 12.5 11 14l3.5-3.5"/>',
  };
  $: inner = P[name] ?? '';
</script>

<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
     stroke-linecap="round" stroke-linejoin="round" class={cls} aria-hidden="true">
  {@html inner}
</svg>
