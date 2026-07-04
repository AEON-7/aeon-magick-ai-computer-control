// One registry for every page in the app. The control-page toolbar, the
// mobile menu, and the Cmd+K command palette all render from this — add a
// route here and it appears everywhere.

export type NavItem = { href: string; label: string; icon: string; title?: string };
export type SuperApp = NavItem & { color: 'cursed' | 'sky' | 'amber' };

export const SUPER_APPS: SuperApp[] = [
  { href: '/orbnet', label: 'OrbNet',     icon: 'orbnet', color: 'cursed', title: 'Decentralized services hub — Tor hidden services, IPFS, Mysterium dVPN, private chat' },
  { href: '/agent',  label: 'Agent Dash', icon: 'braces', color: 'sky',    title: 'Connected gateways + DGX Sparks, per-agent provisioning' },
  { href: '/gpio',   label: 'GPIO',       icon: 'chip',   color: 'amber',  title: 'GPIO pins, HATs, power + IO — live hardware state' },
];

export const SETTINGS_ITEMS: NavItem[] = [
  { href: '/network',  label: 'Network',    icon: 'globe',  title: 'VPN · encrypted DNS · Tor/I2P · firewall' },
  { href: '/wifi',     label: 'WiFi',       icon: 'wifi',   title: 'WiFi mode, saved networks, setup AP' },
  { href: '/tokens',   label: 'API tokens', icon: 'braces', title: 'API tokens for agents / REST / MCP + lockdown' },
  { href: '/ssh-keys', label: 'SSH keys',   icon: 'key',    title: 'SSH authorized keys' },
  { href: '/files',    label: 'Files',      icon: 'folder', title: 'file transfer + clipboard bridge' },
  { href: '/storage',  label: 'Disk',       icon: 'disc',   title: 'USB CD / disk-drive emulation (mount ISOs)' },
  { href: '/system',   label: 'System',     icon: 'cpu',    title: 'Pi health + reboot/poweroff + stream tuning' },
];

export const MONITOR_ITEMS: NavItem[] = [
  { href: '/security', label: 'Security', icon: 'shield', title: 'blocked packets, firewall + intrusion events' },
  { href: '/dns',      label: 'DNS',      icon: 'funnel', title: 'DNS query log + blacklist' },
  { href: '/audit',    label: 'Audit',    icon: 'list',   title: 'access + privileged-action audit log' },
];

// Deep pages that have no toolbar button — the palette makes them
// reachable without clicking through their parent hub.
export const DEEP_ITEMS: NavItem[] = [
  { href: '/network/vpn-providers', label: 'VPN provider setup', icon: 'globe',  title: 'Mullvad / IVPN / AzireVPN / AirVPN wizards' },
  { href: '/network/i2p',           label: 'I2P config',         icon: 'globe',  title: 'I2P overlay network' },
  { href: '/orbnet/onions',         label: 'Tor hidden services', icon: 'orbnet', title: 'Host .onion sites & apps' },
  { href: '/orbnet/ipfs',           label: 'IPFS',               icon: 'orbnet', title: 'Decentralized file & site hosting' },
  { href: '/orbnet/mysterium',      label: 'Mysterium dVPN',     icon: 'orbnet', title: 'Earn by sharing bandwidth' },
  { href: '/orbnet/chat',           label: 'OrbNet chat',        icon: 'orbnet', title: 'Matrix federation over Tor between Orbs' },
];

/** Everything the palette can jump to, grouped for display. */
export const PALETTE_GROUPS: { group: string; items: NavItem[] }[] = [
  { group: 'console', items: [{ href: '/', label: 'Live control', icon: 'monitor', title: 'The KVM console — stream + keyboard/mouse' }] },
  { group: 'apps', items: SUPER_APPS },
  { group: 'settings', items: SETTINGS_ITEMS },
  { group: 'monitor', items: MONITOR_ITEMS },
  { group: 'more', items: DEEP_ITEMS },
];
