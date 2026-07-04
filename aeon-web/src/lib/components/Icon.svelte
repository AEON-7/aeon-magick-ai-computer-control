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
  };
  $: inner = P[name] ?? '';
</script>

<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
     stroke-linecap="round" stroke-linejoin="round" class={cls} aria-hidden="true">
  {@html inner}
</svg>
