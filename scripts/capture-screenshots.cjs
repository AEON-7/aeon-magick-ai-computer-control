#!/usr/bin/env node
// ─────────────────────────────────────────────────────────────────────────────
// capture-screenshots.js — re-runnable showcase screenshot capture for the
// AEON Magick web UI, with built-in REDACTION of anything sensitive.
//
// What it does (read-only against the live device):
//   1. logs in via POST /api/login (session cookie lands on the browser context)
//   2. visits each showcase route, opens the relevant tab/modal/section
//   3. runs the REDACT pass (below) before EVERY screenshot
//   4. writes docs/images/<name>.png
//
// It NEVER clicks "Provision" / "Grant SSH" (those mint one-time secrets) — it
// captures the buttons, not their output.
//
// Usage:
//   cd /tmp && npm i playwright && npx playwright install chromium
//   AEON_URL=https://<device-ip> AEON_PASS='<admin-password>' \
//     node /path/to/aeon/scripts/capture-screenshots.js
//
// Env:
//   AEON_URL   default https://192.168.1.56
//   AEON_USER  default admin
//   AEON_PASS  REQUIRED (admin password; pass via env, never hard-code)
//   AEON_OUT   default <repo>/docs/images
//
// Redaction covers: private IPv4 + IPv6 (ULA/link-local), home-lab hostnames,
// the Matrix homeserver domain + @user handles, aeon_tok_* tokens, OpenSSH
// private-key blocks, credential paths, and absolute /home|/Users paths (the
// unix username); it also blurs the Skills chips and the tip-jar widget. Adjust
// HOSTS/MATRIX below to match your environment.
// ─────────────────────────────────────────────────────────────────────────────
const path = require('path');
const { chromium } = require('playwright');

const BASE = process.env.AEON_URL || 'https://192.168.1.56';
const USER = process.env.AEON_USER || 'admin';
const PASS = process.env.AEON_PASS || '';
const OUT = process.env.AEON_OUT || path.resolve(__dirname, '..', 'docs', 'images');

// Environment-specific redaction targets — edit to taste.
const HOSTS = ['ProArt-Studiobook-W7604J3D', 'ProArt', 'spark-ecdd', 'pacific', 'spark'];
const MATRIX_DOMAIN = 'matrix.unhash.me';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Optional allow-list: AEON_SHOTS=aether,model-push captures only those images
// (others are navigated but not written), so you can refresh one section without
// re-shooting everything.
const ONLY = (process.env.AEON_SHOTS || '').split(',').map((s) => s.trim()).filter(Boolean);
const want = (name) => ONLY.length === 0 || ONLY.includes(name);

// Serialized into the page; must be self-contained (no closure over Node vars
// other than the three injected as args).
function REDACT({ HOSTS, MATRIX_DOMAIN }) {
  const IP = /\b(?:10|127|192\.168|172\.(?:1[6-9]|2\d|3[01]))(?:\.\d{1,3}){1,3}\b/g;
  const IPV6 = /\b(?:f[cde][0-9a-f]{2})(?::[0-9a-f]{0,4}){2,}\b/gi; // ULA + link-local
  const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const md = esc(MATRIX_DOMAIN);

  const scrub = (s) => {
    if (!s) return s;
    let t = s;
    t = t.replace(/aeon_tok_[A-Za-z0-9_]+/g, 'aeon_tok_••••••');
    if (/BEGIN [A-Z ]*PRIVATE KEY/.test(t)) t = '«redacted private key»';
    t = t.replace(new RegExp('@([A-Za-z0-9._-]+):' + md, 'g'), '@$1:▓▓▓');
    t = t.replace(new RegExp(md, 'g'), '▓▓▓.▓▓▓');
    t = t.replace(/~\/voip-[A-Za-z0-9._-]+\/\.env/g, '~/▓▓▓/.env');
    t = t.replace(/~\/\.openclaw_[A-Za-z0-9._-]+_creds\.json/g, '~/▓▓▓_creds.json');
    t = t.replace(/~\/\.openclaw\/credentials\/?/g, '~/▓▓▓/credentials/');
    t = t.replace(/\/(home|Users)\/[A-Za-z0-9._-]+\//g, '/$1/▓▓▓/'); // absolute home paths leak the unix username
    for (const h of HOSTS) t = t.replace(new RegExp(esc(h), 'g'), '▓▓▓▓');
    t = t.replace(IP, '••••••');
    t = t.replace(IPV6, '••••••');
    return t;
  };

  const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, null);
  const ns = [];
  while (w.nextNode()) ns.push(w.currentNode);
  for (const n of ns) {
    const v = scrub(n.nodeValue);
    if (v !== n.nodeValue) n.nodeValue = v;
  }
  for (const el of document.querySelectorAll('input, textarea')) {
    if (el.value) el.value = scrub(el.value);
    if (el.placeholder) el.placeholder = scrub(el.placeholder);
  }
  for (const el of document.querySelectorAll('[title]')) {
    const v = scrub(el.getAttribute('title'));
    if (v != null) el.setAttribute('title', v);
  }
  // blur the donation widget rather than capturing it
  document.querySelectorAll('details').forEach((d) => {
    if (/tip the developer/i.test(d.textContent || '')) d.style.filter = 'blur(8px)';
  });
  document.querySelectorAll('[class*="tipjar"], [class*="TipJar"], .tip-jar').forEach((e) => {
    e.style.filter = 'blur(8px)';
  });
  // blur the Skills-section chips (they name personal tools)
  Array.from(document.querySelectorAll('.agd-sec')).forEach((sec) => {
    if (/^\s*skills/i.test(sec.textContent || '')) {
      sec.querySelectorAll('.agd-chip').forEach((c) => { c.style.filter = 'blur(6px)'; });
    }
  });
}

async function redact(page) {
  await page.evaluate(REDACT, { HOSTS, MATRIX_DOMAIN });
  await sleep(150);
}

async function shoot(page, name, { full = false, clip } = {}) {
  if (!want(name)) { console.log(`  · skip ${name} (filtered)`); return; }
  await redact(page);
  const opts = { path: path.join(OUT, `${name}.png`) };
  if (clip) opts.clip = clip;
  else opts.fullPage = full;
  await page.screenshot(opts);
  console.log(`  ✓ ${name}.png`);
}

async function go(page, route) {
  await page
    .goto(BASE + route, { waitUntil: 'networkidle', timeout: 30000 })
    .catch((e) => console.log(`  · goto ${route}: ${e.message.split('\n')[0]}`));
  await sleep(1500);
}

async function clickText(page, text, { tag = 'button', timeout = 4000 } = {}) {
  try {
    await page.locator(`${tag}:has-text("${text}")`).first().click({ timeout });
    return true;
  } catch (e) {
    console.log(`  · click "${text}" miss: ${e.message.split('\n')[0]}`);
    return false;
  }
}

(async () => {
  if (!PASS) {
    console.error('Set AEON_PASS to the admin password (env var).');
    process.exit(2);
  }
  const browser = await chromium.launch();
  const ctx = await browser.newContext({
    ignoreHTTPSErrors: true,
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 2,
  });

  const resp = await ctx.request.post(`${BASE}/api/login`, {
    data: { username: USER, password: PASS },
  });
  console.log(`login: HTTP ${resp.status()}`);
  if (!resp.ok()) {
    await browser.close();
    process.exit(2);
  }

  const page = await ctx.newPage();

  // 1) FLAGSHIP — live computer-control / vision+HID view.
  console.log('› / (control)');
  await go(page, '/');
  await sleep(1500);
  await shoot(page, 'control');

  // 2) AGENT OVERVIEW — pantheon + GPU/CPU/RAM + token chart.
  console.log('› /agent (overview)');
  await go(page, '/agent');
  await sleep(3000);
  await page.evaluate(() => window.scrollTo(0, 0));
  await shoot(page, 'agent-overview', { full: true });

  // 2b) AGENT DETAIL MODAL — open first agent card, capture, then close.
  console.log('› agent detail modal');
  const opened = await page
    .locator('.agent-click')
    .first()
    .click({ timeout: 5000 })
    .then(() => true)
    .catch(() => false);
  await sleep(1800);
  if (opened) await shoot(page, 'agent-detail');
  await page.locator('.agd-close').first().click({ timeout: 3000 }).catch(() => page.mouse.click(20, 20));
  await sleep(800);

  // 2c) CONTAINERS tab.
  console.log('› containers tab');
  await clickText(page, 'Containers');
  await sleep(2200);
  await page.evaluate(() => window.scrollTo(0, 0));
  await shoot(page, 'containers', { full: true });

  // 2d) EASY DEPLOY — a <section class="deploy"> in the Containers tab. Select a
  //     catalog card to reveal the GPU slider + flag editor, then frame it.
  console.log('› easy deploy');
  const hasDeploy = await page.evaluate(() => !!document.querySelector('section.deploy'));
  if (hasDeploy) {
    await page.locator('.dep-card').first().click({ timeout: 4000 }).catch(() => {});
    await sleep(900);
    await page.evaluate(() => {
      const ed = document.querySelector('.dep-editor');
      ed?.scrollIntoView({ block: 'center' });
      window.scrollBy(0, -120);
    });
    await sleep(500);
    await redact(page);
    const clip = await page.evaluate(() => {
      const ed = document.querySelector('.dep-editor');
      const r = ed?.getBoundingClientRect();
      if (!r) return null;
      const y = Math.max(0, r.top - 150);
      const bottom = Math.min(window.innerHeight, r.bottom + 16);
      return { x: 0, y, width: window.innerWidth, height: Math.max(300, bottom - y) };
    });
    if (clip) await shoot(page, 'easy-deploy', { clip });
    else await shoot(page, 'easy-deploy');
  } else {
    console.log('  · section.deploy absent (system not docker+GPU eligible)');
  }

  // 2e) TERMINAL tab — open a pane so xterm renders.
  console.log('› terminal tab');
  await clickText(page, 'Terminal');
  await sleep(1200);
  let pane = await page.locator('.term-open-btn').first().click({ timeout: 3000 }).then(() => true).catch(() => false);
  if (!pane) pane = await clickText(page, 'Open', { timeout: 2500 });
  await sleep(3000);
  await shoot(page, 'terminal');

  // 2f) AETHER MODEL SHARE — the decentralized model library. These are shot at
  //     the native viewport (1440×900, a normal browser window) rather than a
  //     tall full-page stitch, so the docs show what a user actually sees.
  if (want('aether') || want('model-push') || want('aether-download')) {
    console.log('› /model-share (Aether)');
    await go(page, '/model-share');
    await sleep(2800);
    await page.evaluate(() => window.scrollTo(0, 0));
    await shoot(page, 'aether');

    // 2g) PUSH-TO-SERVER dialog — open it on the first model card that carries a
    //     push button. With no connected systems it shows the "connect a system"
    //     entry state; with ≥1 it shows the target picker + folder browser.
    console.log('› Aether push-to-server dialog');
    const pushOpened = await page
      .locator('button:has-text("Push to server")')
      .first()
      .click({ timeout: 4000 })
      .then(() => true)
      .catch(() => false);
    await sleep(1200);
    if (pushOpened) {
      await shoot(page, 'model-push');
      await page.keyboard.press('Escape').catch(() => {});
    } else {
      console.log('  · no push button (no models) — skipped model-push');
    }
    await sleep(400);

    // 2h) DOWNLOAD IN ACTION — the sandboxed pull with a live byte-level progress
    //     bar. Peer downloads over IPFS can finish in seconds once the blocks are
    //     cached nearby, so catching a mid-download frame by luck is unreliable.
    //     Instead we stage a genuine in-progress state deterministically: intercept
    //     the registry poll and mark one peer-only model as fetching at 34% (the
    //     exact task shape the backend emits — verified against a live 469 MB
    //     pull). The bar, phase label and byte readout are the real component
    //     rendering real data; only the trigger is staged. No device state changes.
    if (want('aether-download')) {
      console.log('› Aether download-in-progress (staged from the live task shape)');
      await page.route('**/api/ipfs/models/registry', async (route) => {
        let data;
        const resp = await route.fetch();
        try { data = await resp.json(); } catch { return route.fulfill({ response: resp }); }
        const m =
          (data.models || []).find((x) => /Qwen2\.5-0\.5B/.test(x.entry?.name || '')) ||
          (data.models || []).find((x) => !x.local && (x.entry?.size_bytes || 0) > 50 * 1024 * 1024);
        if (m) {
          m.local = false; // render its card as a fetchable (non-hosted) model
          data.tasks = { ...(data.tasks || {}) };
          data.tasks[m.entry.cid] = {
            phase: 'downloading to quarantine…',
            pct: 34,
            done_bytes: 167116800,
            total_bytes: m.entry.size_bytes || 491400032,
          };
        }
        await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) });
      });
      await go(page, '/model-share');
      await sleep(2600);
      // Foreground the in-flight progress row (unique .bg-ink-900/60 wrapper) so
      // the bar — and the same model's card mid-download below it — fill the frame.
      // The app scrolls an inner <main>, so drive that scroller directly and seat
      // the row ~120px from the top rather than relying on window scrollIntoView.
      await page.evaluate(() => {
        const row = Array.from(document.querySelectorAll('div')).find((d) =>
          (d.className || '').toString().includes('bg-ink-900/60'));
        if (!row) return;
        let sc = row.parentElement;
        while (sc && sc !== document.body) {
          const oy = getComputedStyle(sc).overflowY;
          if ((oy === 'auto' || oy === 'scroll') && sc.scrollHeight > sc.clientHeight + 4) break;
          sc = sc.parentElement;
        }
        const scroller = sc && sc !== document.body ? sc : document.scrollingElement || document.documentElement;
        const top = scroller.getBoundingClientRect ? scroller.getBoundingClientRect().top : 0;
        scroller.scrollTop += row.getBoundingClientRect().top - top - 120;
      });
      await sleep(700);
      await shoot(page, 'aether-download');
      await page.unroute('**/api/ipfs/models/registry').catch(() => {});
    }
  }

  // 3) NETWORK / PRIVACY — expand the Tor+I2P and VPN <details>, neutralize the
  //    inner-scroller height clamp so the full privacy stack fits, then clip
  //    from the "Privacy stack" pills through the VPN tunnel section.
  console.log('› /network (privacy)');
  await page.setViewportSize({ width: 1440, height: 4600 });
  await go(page, '/network');
  await sleep(2000);
  await page.evaluate(() => {
    Array.from(document.querySelectorAll('details')).forEach((d) => {
      const sum = (d.querySelector('summary')?.textContent || '').toLowerCase();
      if (/tip the developer/.test(d.textContent || '')) return;
      d.open = /privacy overlay|vpn \(wan tunnel\)/.test(sum);
    });
    document.querySelectorAll('main').forEach((m) => {
      m.style.height = 'auto';
      m.style.maxHeight = 'none';
      m.style.overflow = 'visible';
      m.style.flex = 'none';
    });
  });
  await sleep(800);
  await page.evaluate(() => window.dispatchEvent(new Event('resize')));
  await sleep(2200);
  await page.evaluate(() => {
    const h = Array.from(document.querySelectorAll('h2, h3')).find((e) => /^\s*privacy stack/i.test(e.textContent || ''));
    (h?.closest('section') || h)?.scrollIntoView({ block: 'start' });
    window.scrollBy(0, -10);
  });
  await sleep(500);
  await redact(page);
  const netClip = await page.evaluate(() => {
    const h = Array.from(document.querySelectorAll('h2, h3')).find((e) => /^\s*privacy stack/i.test(e.textContent || ''));
    const sec = h?.closest('section') || h;
    const vpn = Array.from(document.querySelectorAll('details')).find((d) => /VPN \(WAN tunnel\)/i.test(d.querySelector('summary')?.textContent || ''));
    const top = Math.max(0, (sec?.getBoundingClientRect().top ?? 0) - 6);
    const bottom = vpn ? vpn.getBoundingClientRect().bottom + 14 : top + 4400;
    const height = Math.min(window.innerHeight - top, bottom - top);
    return { x: 0, y: top, width: window.innerWidth, height: Math.max(400, height) };
  });
  await shoot(page, 'network-privacy', { clip: netClip });
  await page.setViewportSize({ width: 1440, height: 900 });

  // 4) TOKENS / API keys.
  console.log('› /tokens');
  await go(page, '/tokens');
  await shoot(page, 'tokens', { full: true });

  // 5) AUDIT log.
  console.log('› /audit');
  await go(page, '/audit');
  await shoot(page, 'audit', { full: true });

  // 6) SECURITY console.
  console.log('› /security');
  await go(page, '/security');
  await shoot(page, 'security', { full: true });

  // ── Additional showcase surfaces (full feature walkthrough) ──────────────
  const extra = [
    ['/system', 'system'],
    ['/storage', 'storage'],
    ['/files', 'files'],
    ['/wifi', 'wifi'],
    ['/setup/wifi', 'setup-wifi'],
    ['/network/vpn-providers', 'vpn-providers'],
    ['/network/i2p', 'i2p'],
    ['/orbnet', 'orbnet'],
    ['/orbnet/chat', 'orbnet-chat'],
    ['/orbnet/ipfs', 'orbnet-ipfs'],
    ['/orbnet/mysterium', 'orbnet-mysterium'],
    ['/orbnet/onions', 'orbnet-onions'],
    ['/hailo', 'hailo'],
    ['/braincraft', 'braincraft'],
    ['/gpio', 'gpio'],
    ['/fleet', 'fleet'],
    ['/dns', 'dns'],
    ['/ssh-keys', 'ssh-keys'],
    ['/bench', 'aeon-bench'],
    ['/login', 'login'],
  ];
  for (const [route, name] of extra) {
    if (!want(name)) continue;
    console.log(`› ${route} (${name})`);
    await page.setViewportSize({ width: 1440, height: 900 });
    await go(page, route);
    await sleep(1200);
    await page.evaluate(() => window.scrollTo(0, 0));
    await shoot(page, name, { full: true });
  }

  await browser.close();
  console.log('DONE →', OUT);
})().catch((e) => {
  console.error('FATAL', e);
  process.exit(1);
});
