#!/usr/bin/env node
// Build the bundled HAT knowledge base from pinout.xyz overlay files.
//
// Source: pinout.xyz — https://github.com/pinout-xyz/Pinout.xyz (CC BY-SA 4.0).
// Each board's YAML frontmatter is distilled into a compact record: the physical
// header pins it occupies (the Pi↔HAT mapping), its I2C chips + addresses, type,
// and a one-line description. The prose body + image refs are dropped. The
// supervisor loads the result to identify a detected HAT, draw its pin mapping,
// flag pin collisions, and hand the AI the chip/control facts it needs to write
// code for the board.
//
// Run at bake time on the build host (NOT the Pi):
//   cd scripts && npm i && node build-hat-library.mjs [SRC_OVERLAY_DIR] [OUT_JSON]
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import yaml from 'js-yaml';

const SRC = process.argv[2] || '/tmp/pinout-xyz/src/en/overlay';
const OUT = process.argv[3] || '/tmp/hat-library.json';

if (!fs.existsSync(SRC)) {
  console.error(`overlay dir not found: ${SRC}\nClone first: git clone --depth 1 https://github.com/pinout-xyz/Pinout.xyz /tmp/pinout-xyz`);
  process.exit(1);
}

const files = fs.readdirSync(SRC).filter((f) => f.endsWith('.md'));
const hats = [];
let skipped = 0;

for (const f of files) {
  const txt = fs.readFileSync(path.join(SRC, f), 'utf8');
  // frontmatter lives inside an HTML comment: <!-- --- <yaml> -->
  const m = txt.match(/<!--\s*-{3,}\s*([\s\S]*?)-->/);
  if (!m) { skipped++; continue; }
  let fm;
  try {
    fm = yaml.load(m[1]);
  } catch {
    skipped++;
    continue;
  }
  if (!fm || !fm.name) { skipped++; continue; }

  // Physical header pin -> role. power/ground are keyed by pin number with empty
  // values; `pin` carries an explicit {mode} (i2c/spi/uart/pwm/…).
  const pins = {};
  for (const p of Object.keys(fm.power || {})) pins[p] = 'power';
  for (const p of Object.keys(fm.ground || {})) pins[p] = 'ground';
  for (const [p, v] of Object.entries(fm.pin || {})) {
    pins[p] = v && v.mode ? String(v.mode).toLowerCase() : 'gpio';
  }

  // I2C chips: address -> { name, device }.
  const i2c = {};
  for (const [addr, v] of Object.entries(fm.i2c || {})) {
    const name = v && v.name != null ? String(v.name).trim() : undefined;
    const device = v && v.device != null ? String(v.device).trim() : undefined;
    if (name || device) i2c[String(addr).toLowerCase()] = { name, device };
  }

  const type = Array.isArray(fm.type)
    ? fm.type.map((t) => String(t).trim())
    : typeof fm.type === 'string'
      ? fm.type.split(',').map((s) => s.trim()).filter(Boolean)
      : [];

  const rec = {
    id: f.replace(/\.md$/, ''),
    name: String(fm.name).trim(),
    type,
    pins,
    i2c,
  };
  if (fm.manufacturer) rec.manufacturer = String(fm.manufacturer).trim();
  if (fm.formfactor) rec.formfactor = String(fm.formfactor).trim();
  rec.eeprom = fm.eeprom === true || fm.eeprom === 'yes' || fm.eeprom === 'true';
  if (fm.pincount != null) rec.pincount = Number(fm.pincount) || fm.pincount;
  if (fm.description) rec.description = String(fm.description).trim();
  if (fm.url) rec.url = String(fm.url).trim();
  // drop an empty i2c map to save bytes
  if (Object.keys(rec.i2c).length === 0) delete rec.i2c;

  hats.push(rec);
}

hats.sort((a, b) => a.id.localeCompare(b.id));

// I2C chip reference (technoblogy/i2c-detective) — chip -> address range +
// category + optional ID register/value. Turns an i2cdetect hit into a
// candidate chip: the EEPROM-less / loose-breakout identification path the AI
// needs when someone just plugs sensors onto the header.
function parseChips(inoPath) {
  if (!fs.existsSync(inoPath)) return [];
  const txt = fs.readFileSync(inoPath, 'utf8');
  const re = /\{\s*"([^"]+)"\s*,\s*(\w+)\s*,\s*(0x[0-9a-fA-F]+)\s*,\s*(0x[0-9a-fA-F]+)\s*,\s*(0x[0-9a-fA-F]+)\s*,\s*(0x[0-9a-fA-F]+)\s*\}/g;
  const out = [];
  let m;
  while ((m = re.exec(txt))) {
    const [, name, cat, lo, hi, reg, val] = m;
    const rec = {
      chip: name,
      category: cat.replace(/([a-z])([A-Z])/g, '$1 $2'),
      addr_low: lo.toLowerCase(),
      addr_high: hi.toLowerCase(),
    };
    if (reg !== '0x00' || val !== '0x00') {
      rec.id_reg = reg.toLowerCase();
      rec.id_val = val.toLowerCase();
    }
    out.push(rec);
  }
  out.sort((a, b) => a.chip.localeCompare(b.chip));
  return out;
}
// Curated chip -> address list WITH product/guide links (adafruit/I2C_Addresses).
// The guide URL is the AI's "how to program this chip" entry point (the Adafruit
// learn guide carries the CircuitPython driver + register usage).
function parseAdafruitI2c(dir) {
  if (!dir || !fs.existsSync(dir)) return [];
  const out = [];
  for (const f of fs.readdirSync(dir).filter((f) => /^0x.*\.md$/.test(f))) {
    for (const line of fs.readFileSync(path.join(dir, f), 'utf8').split('\n')) {
      const m = line.match(/^\s*-\s*\[([^\]]+)\]\(([^)]+)\)\s*(?:\(([^)]*)\))?/);
      if (!m) continue;
      const label = m[1].trim();
      const chip = (label.split(/[\s,(]/)[0] || '').replace(/[^\w./-]/g, '');
      if (!chip) continue;
      out.push({ chip, label, guide: m[2].trim(), addr_note: (m[3] || '').trim() });
    }
  }
  return out;
}

// Merge the two chip sources by normalized name. i2c-detective gives category +
// address range + ID register; adafruit adds a guide link + address variants.
const normChip = (s) => s.toLowerCase().replace(/[^a-z0-9]/g, '');
const chipMap = new Map();
for (const c of parseChips(process.argv[4] || '/tmp/i2c-detective/I2CDetective.ino')) {
  chipMap.set(normChip(c.chip), { ...c, sources: ['i2c-detective'] });
}
for (const c of parseAdafruitI2c(process.argv[5] || '/tmp/i2c-addresses')) {
  const k = normChip(c.chip);
  const ex = chipMap.get(k);
  if (ex) {
    if (c.guide && !ex.guide) ex.guide = c.guide;
    if (c.label && !ex.label) ex.label = c.label;
    if (!ex.sources.includes('adafruit')) ex.sources.push('adafruit');
  } else {
    chipMap.set(k, { chip: c.chip, label: c.label, guide: c.guide, addr_note: c.addr_note, sources: ['adafruit'] });
  }
}
const chips = [...chipMap.values()].sort((a, b) => a.chip.localeCompare(b.chip));

const lib = {
  schema: 2,
  sources: ['pinout.xyz (CC BY-SA 4.0)', 'technoblogy/i2c-detective (I2C chip address reference)'],
  count: hats.length,
  hats,
  chip_count: chips.length,
  chips,
};

const json = JSON.stringify(lib);
fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.writeFileSync(OUT, json);
const gz = zlib.gzipSync(json, { level: 9 });
fs.writeFileSync(`${OUT}.gz`, gz);

const withI2c = hats.filter((h) => h.i2c).length;
console.log(`parsed ${hats.length} HATs (${skipped} skipped) from ${files.length} files`);
console.log(`  ${withI2c} carry I2C chip data`);
console.log(`  + ${chips.length} I2C chips in the address reference`);
console.log(`raw json : ${(json.length / 1024).toFixed(1)} KB  -> ${OUT}`);
console.log(`gzipped  : ${(gz.length / 1024).toFixed(1)} KB  -> ${OUT}.gz`);
for (const id of ['adafruit-motor-hat', 'adafruit-servo-hat', 'ab-rtc-pi']) {
  const h = hats.find((x) => x.id === id);
  if (h) console.log(`\nsample [${id}]:\n  ${JSON.stringify({ name: h.name, type: h.type, pins: h.pins, i2c: h.i2c })}`);
}
