#!/usr/bin/env python3
"""
Build-time script (v55): fetch the canonical dnscrypt-proxy v3
public-resolvers.md, parse + annotate each entry, and emit
`aeon-supervisor/data/dnscrypt-resolvers.json`.

We embed only DNSCrypt v2-protocol entries (sdns://AQ stamps) — DoH /
DoT / ODoH leak the resolver's hostname via TLS SNI, and the user
already has a Custom slot for pasting those if they really want.

Each output record carries:
    name              dnscrypt-proxy resolver name (matches upstream list)
    label             "Switzerland (Quad9)"-style UI label
    operator          inferred from name prefix + description text
    country           ISO 3166 alpha-2
    eyes              none / fourteen / nine / five / unknown
    transport         "DNSCrypt" (only DNSCrypt v2 here)
    ipv4              true (we drop -ipv6 entries — Pi runs IPv4-only stack)
    port              TCP/UDP port from the stamp
    props:
      dnssec_validating  operator validates DNSSEC
      no_logs            operator commits to no logs
      no_filter          no on-server filtering (raw answers)
    filters           list like ["malware", "adult", "ads"] inferred from desc
    privacy_score     0-5 (no_logs + dnssec + no_filter + eyes_tier)
    trust_score       0-5 (operator tier + audited + DNSCrypt-only + anonymized)
    description       first ~200 chars of upstream description

Refresh by re-running this + rebuilding the Rust binary. Bake doesn't
auto-refresh (deterministic per release).
"""

import base64
import json
import re
import sys
import urllib.request
from collections import OrderedDict
from pathlib import Path

UPSTREAM_URL = (
    "https://raw.githubusercontent.com/DNSCrypt/dnscrypt-resolvers/"
    "master/v3/public-resolvers.md"
)
OUT_PATH = (
    Path(__file__).resolve().parent.parent
    / "aeon-supervisor"
    / "data"
    / "dnscrypt-resolvers.json"
)

# ── Eyes alliances (reused from regenerate-anon-relays.py) ──────────
EYES_5 = {"US", "GB", "CA", "AU", "NZ"}
EYES_9 = EYES_5 | {"FR", "DK", "NL", "NO"}
EYES_14 = EYES_9 | {"DE", "BE", "IT", "ES", "SE"}

def eyes_tier(c: str) -> str:
    if not c:
        return "unknown"
    if c in EYES_5: return "five"
    if c in EYES_9: return "nine"
    if c in EYES_14: return "fourteen"
    return "none"

# ── Operator inference ─────────────────────────────────────────────
# Each tuple is (regex matched against the resolver name, canonical operator
# string). First match wins. Operators we want grouped together (e.g. the
# many cisco-* PoPs) collapse to a single operator name.
OPERATOR_PATTERNS = [
    (re.compile(r"^adguard-dns($|-family$|-unfiltered$)"), "AdGuard"),
    (re.compile(r"^cs-"), "CryptoStorm"),
    (re.compile(r"^dnscry\.pt-"), "DNSCry.pt"),
    (re.compile(r"^dnscrypt\.ca-?"), "DNSCrypt.ca"),
    (re.compile(r"^cisco($|-)"), "Cisco/OpenDNS"),
    (re.compile(r"^cleanbrowsing-"), "CleanBrowsing"),
    (re.compile(r"^quad9-"), "Quad9"),
    (re.compile(r"^restena-"), "Restena"),
    (re.compile(r"^dnswarden-"), "dnswarden"),
    (re.compile(r"^scaleway$|^scaleway-|^kama$|^jp\.tiar\.app$"), "jedisct1"),
    (re.compile(r"^flokinet"), "FlokiNET"),
    (re.compile(r"^uncensoreddns-"), "UncensoredDNS"),
    (re.compile(r"^ahadns-"), "AhaDNS"),
    (re.compile(r"^bortzmeyer$|^fdn$"), "FDN"),
    (re.compile(r"^meganerd$"), "meganerd"),
    (re.compile(r"^inconnu$"), "inconnu"),
    (re.compile(r"^tiarap$"), "tiarap"),
    (re.compile(r"^wikimedia$"), "Wikimedia"),
    (re.compile(r"^plan9-"), "plan9-dns"),
    (re.compile(r"^controld-"), "Control D"),
    (re.compile(r"^mullvad-"), "Mullvad"),
    (re.compile(r"^libredns($|-)"), "LibreDNS"),
    (re.compile(r"^nextdns($|-)"), "NextDNS"),
    (re.compile(r"^dns\.sb-"), "DNS.SB"),
    (re.compile(r"^opennic-"), "OpenNIC"),
    (re.compile(r"^yandex"), "Yandex"),
    (re.compile(r"^comodo-"), "Comodo"),
]

def operator_of(name: str) -> str:
    for pat, op in OPERATOR_PATTERNS:
        if pat.match(name):
            return op
    return "independent"

# ── Operator trust tiers ───────────────────────────────────────────
# 3 = household-name / non-profit / audited / academic
# 2 = commercial with public privacy policy + transparency
# 1 = small operator with no formal audit; relies on community trust
# Default for unlisted = 1.
OPERATOR_TIER = {
    "Quad9": 3,                # Swiss non-profit, audited, no-log
    "AdGuard": 2,              # commercial w/ published policy
    "Cisco/OpenDNS": 2,        # commercial, big infra
    "CleanBrowsing": 2,        # commercial
    "Mullvad": 3,              # audited no-log VPN provider
    "Control D": 2,            # commercial
    "NextDNS": 2,              # commercial
    "Wikimedia": 3,            # non-profit
    "Restena": 3,              # Luxembourg academic
    "UncensoredDNS": 3,        # long-running independent no-log
    "FDN": 3,                  # French nonprofit ISP, audited
    "dnswarden": 2,            # community-known privacy ops
    "DNS.SB": 2,
    "OpenNIC": 2,              # alt-root volunteer net
    "AhaDNS": 2,
    "LibreDNS": 2,             # GreekDevs nonprofit-ish
    "Comodo": 2,
    "Yandex": 1,               # commercial, Russian jurisdiction
    "jedisct1": 3,             # dnscrypt-proxy author runs these
    "CryptoStorm": 2,          # privacy-VPN provider
    "DNSCry.pt": 2,            # large independent operator
    "DNSCrypt.ca": 2,
    "FlokiNET": 2,
    "meganerd": 1,
    "inconnu": 1,
    "tiarap": 1,
    "plan9-dns": 1,
    "independent": 1,
}

# ── Country detection (reused from relay parser, condensed) ─────────
CITY_TO_CC = {
    # Europe
    "zurich": "CH", "geneva": "CH", "swiss": "CH", "switzerland": "CH",
    "amsterdam": "NL", "naaldwijk": "NL", "eygelshoven": "NL",
    "berlin": "DE", "frankfurt": "DE", "munich": "DE", "nuremberg": "DE",
    "dusseldorf": "DE", "düsseldorf": "DE", "jena": "DE", "bremen": "DE",
    "hamburg": "DE", "molln": "DE", "leipzig": "DE", "cologne": "DE",
    "köln": "DE", "wiesbaden": "DE", "stuttgart": "DE",
    "paris": "FR", "marseille": "FR", "lyon": "FR", "france": "FR",
    "london": "GB", "manchester": "GB", "coventry": "GB", "redditch": "GB",
    "newcastle": "GB",
    "milan": "IT", "rome": "IT", "palermo": "IT",
    "madrid": "ES", "barcelona": "ES",
    "lisbon": "PT", "porto": "PT",
    "stockholm": "SE", "hudiksvall": "SE", "sandefjord": "SE",
    "oslo": "NO", "bergen": "NO",
    "copenhagen": "DK",
    "helsinki": "FI", "tuusula": "FI",
    "hafnarfjordur": "IS", "reykjavik": "IS",
    "dublin": "IE",
    "brussels": "BE",
    "vienna": "AT", "wien": "AT",
    "prague": "CZ", "brno": "CZ",
    "warsaw": "PL", "gdansk": "PL", "gdańsk": "PL",
    "budapest": "HU",
    "bucharest": "RO", "timisoara": "RO", "oradea": "RO",
    "luxembourg": "LU",
    "bratislava": "SK",
    "ljubljana": "SI",
    "belgrade": "RS",
    "chisinau": "MD", "chișinău": "MD",
    "tallinn": "EE",
    "riga": "LV",
    "vilnius": "LT",
    "kyiv": "UA", "kiev": "UA",
    "sofia": "BG",
    "athens": "GR", "thessaloniki": "GR",
    "tirana": "AL",
    "moscow": "RU",
    "yerevan": "AM",
    "baku": "AZ",
    # Middle East / Asia
    "tel aviv": "IL", "telaviv": "IL",
    "fujairah": "AE",
    "islamabad": "PK",
    "mumbai": "IN", "bengaluru": "IN", "chennai": "IN", "hyderabad": "IN",
    "newdelhi": "IN", "delhi": "IN",
    "dhaka": "BD",
    "bangkok": "TH",
    "hanoi": "VN",
    "manila": "PH",
    "jakarta": "ID",
    "singapore": "SG",
    "hong kong": "HK", "hongkong": "HK",
    "seoul": "KR", "busan": "KR",
    "tokyo": "JP", "osaka": "JP", "nagoya": "JP", "japan": "JP",
    "taipei": "TW", "taiwan": "TW",
    # Oceania
    "sydney": "AU", "melbourne": "AU", "brisbane": "AU", "adelaide": "AU",
    "perth": "AU", "australia": "AU",
    "auckland": "NZ", "wellington": "NZ", "new zealand": "NZ",
    # Africa
    "johannesburg": "ZA", "cape town": "ZA", "capetown": "ZA",
    "lagos": "NG", "ikeja": "NG",
    "ebenecity": "MU", "ebene": "MU", "ebène": "MU",
    # Americas
    "saopaulo": "BR", "sao paulo": "BR", "são paulo": "BR",
    "buenos aires": "AR",
    "santiago": "CL", "valdivia": "CL",
    "bogota": "CO", "bogotá": "CO",
    "lima": "PE",
    "guayaquil": "EC",
    "queretaro": "MX", "querétaro": "MX",
    "toronto": "CA", "montreal": "CA", "vancouver": "CA", "calgary": "CA",
    "halifax": "CA",
    # US — long list of cities most likely to appear
    "new york": "US", "newyork": "US", "nyc": "US",
    "los angeles": "US", "losangeles": "US",
    "chicago": "US", "seattle": "US", "miami": "US", "atlanta": "US",
    "dallas": "US", "houston": "US", "phoenix": "US", "denver": "US",
    "portland": "US", "san francisco": "US", "sanfrancisco": "US",
    "sanjose": "US", "santaclara": "US", "fremont": "US",
    "jacksonville": "US", "tampa": "US",
    "las vegas": "US", "lasvegas": "US",
    "philadelphia": "US",
    "boston": "US", "baltimore": "US", "cleveland": "US",
    "st louis": "US", "stlouis": "US",
    "nashville": "US", "memphis": "US",
    "austin": "US", "san antonio": "US", "sanantonio": "US",
    "orlando": "US", "columbus": "US", "charlotte": "US",
    "raleigh": "US", "pittsburgh": "US", "cincinnati": "US",
    "milwaukee": "US", "grand rapids": "US", "grandrapids": "US",
    "kansas city": "US", "kansascity": "US",
    "salt lake city": "US", "saltlakecity": "US",
    "liberty lake": "US", "libertylake": "US",
    "berkeley springs": "US", "berkeleysprings": "US",
    "allentown": "US", "flint": "US", "detroit": "US",
    "indianapolis": "US", "durham": "US", "spokane": "US",
    "ashburn": "US", "ogden": "US", "ottoville": "US", "taos": "US",
    "port edwards": "US", "portedwards": "US",
}

COUNTRY_NAMES = {
    "switzerland": "CH", "swiss": "CH",
    "netherlands": "NL", "nl ": "NL",
    "germany": "DE", "german": "DE",
    "france": "FR", "french": "FR",
    "united kingdom": "GB", "uk ": "GB", "u.k.": "GB", "britain": "GB",
    "england": "GB",
    "italy": "IT", "italian": "IT",
    "spain": "ES", "spanish": "ES",
    "portugal": "PT",
    "sweden": "SE", "swedish": "SE",
    "norway": "NO", "norwegian": "NO",
    "denmark": "DK", "danish": "DK",
    "finland": "FI", "finnish": "FI",
    "iceland": "IS",
    "ireland": "IE", "irish": "IE",
    "belgium": "BE", "belgian": "BE",
    "austria": "AT",
    "czech": "CZ",
    "poland": "PL", "polish": "PL",
    "hungary": "HU",
    "romania": "RO", "romanian": "RO",
    "luxembourg": "LU",
    "slovakia": "SK", "slovak": "SK",
    "slovenia": "SI",
    "serbia": "RS",
    "moldova": "MD",
    "estonia": "EE",
    "latvia": "LV",
    "lithuania": "LT",
    "ukraine": "UA",
    "bulgaria": "BG",
    "greece": "GR", "greek": "GR",
    "albania": "AL",
    "russia": "RU",
    "armenia": "AM",
    "azerbaijan": "AZ",
    "turkey": "TR", "turkish": "TR",
    "israel": "IL",
    "uae": "AE", "united arab emirates": "AE", "dubai": "AE",
    "pakistan": "PK",
    "india": "IN",
    "bangladesh": "BD",
    "thailand": "TH",
    "vietnam": "VN",
    "philippines": "PH",
    "indonesia": "ID",
    "singapore": "SG",
    "south korea": "KR", "korea": "KR",
    "japan": "JP", "japanese": "JP",
    "taiwan": "TW",
    "australia": "AU",
    "new zealand": "NZ",
    "south africa": "ZA",
    "nigeria": "NG",
    "mauritius": "MU",
    "brazil": "BR",
    "argentina": "AR",
    "chile": "CL",
    "colombia": "CO",
    "peru": "PE",
    "ecuador": "EC",
    "mexico": "MX",
    "canada": "CA",
    "united states": "US", "usa": "US", "u.s.a": "US",
}

def country_from(name: str, desc: str) -> str:
    text = (name + " " + desc).lower()
    # Cities first (more specific)
    for kw, cc in CITY_TO_CC.items():
        if kw in text:
            return cc
    for kw, cc in COUNTRY_NAMES.items():
        if kw in text:
            return cc
    # Heuristic: anycast or anycast-flavoured — we mark unknown
    return ""

# ── sdns:// decoder ────────────────────────────────────────────────
def b64dec_sdns(s: str) -> bytes:
    """URL-safe base64 decode with auto-padding."""
    pad = "=" * (-len(s) % 4)
    return base64.urlsafe_b64decode(s + pad)

def decode_dnscrypt_stamp(stamp: str):
    """Parse a sdns:// stamp. Returns dict with protocol/props/addr/port/pk/provider_name,
    or None if not a DNSCrypt v2 stamp."""
    if not stamp.startswith("sdns://"):
        return None
    data = b64dec_sdns(stamp[7:])
    if not data:
        return None
    proto = data[0]
    if proto != 0x01:
        return None  # only DNSCrypt v2 here
    # 8-byte props field (little-endian flags)
    props = int.from_bytes(data[1:9], "little")
    dnssec = bool(props & 1)
    no_logs = bool(props & 2)
    no_filter = bool(props & 4)
    i = 9
    addr_len = data[i]; i += 1
    addr = data[i:i + addr_len].decode("ascii", errors="replace")
    i += addr_len
    # Port detection from addr
    port = 443
    addr_no_port = addr
    if ":" in addr and not addr.startswith("["):
        host, _, p = addr.rpartition(":")
        try:
            port = int(p)
            addr_no_port = host
        except ValueError:
            pass
    elif addr.startswith("[") and "]:" in addr:
        addr_no_port, _, p = addr.partition("]:")
        addr_no_port = addr_no_port.lstrip("[")
        try:
            port = int(p)
        except ValueError:
            pass
    return {
        "protocol": "DNSCrypt",
        "dnssec": dnssec,
        "no_logs": no_logs,
        "no_filter": no_filter,
        "addr": addr_no_port,
        "port": port,
    }

# ── Filter inference from description ──────────────────────────────
def filters_from_desc(desc: str, no_filter: bool) -> list:
    if no_filter:
        return []
    out = []
    d = desc.lower()
    if "malware" in d or "phishing" in d or "threat" in d:
        out.append("malware")
    if "adult" in d or "porn" in d or "safesearch" in d or "family" in d:
        out.append("adult")
    if "ad " in d or "ad-block" in d or "adblock" in d or "ads " in d \
            or "tracker" in d or "tracking" in d:
        out.append("ads")
    if "cryptojack" in d or "crypto-mining" in d:
        out.append("crypto-mining")
    return out

# ── Privacy / trust scoring ────────────────────────────────────────
def privacy_score(no_logs: bool, dnssec: bool, no_filter: bool, eyes: str) -> int:
    """0-5. Weighted toward the stamp-declared no_logs (operator
    commitment) since that's the strongest single signal."""
    score = 0
    if no_logs: score += 2          # biggest signal
    if dnssec: score += 1           # protects against poisoning, not privacy per se but trust
    if no_filter: score += 1        # raw answers = more transparent
    if eyes == "none": score += 1
    elif eyes == "fourteen": score += 0  # in fourteen Eyes but not 9/5
    return min(score, 5)

def trust_score(operator: str, props: dict) -> int:
    """0-5. Operator reputation tier + props-driven bonuses."""
    tier = OPERATOR_TIER.get(operator, 1)
    score = tier  # 1, 2, or 3
    if props["no_logs"]: score += 1
    if props["dnssec"]: score += 1
    return min(score, 5)

# ── Label generation ──────────────────────────────────────────────
def make_label(name: str, country: str, operator: str) -> str:
    # Strip operator prefix to get a "location"-like remainder
    # e.g. "cs-tokyo" -> "Tokyo (CryptoStorm)"
    rest = name
    for prefix in ["cs-", "dnscry.pt-", "cisco-", "cleanbrowsing-",
                   "adguard-dns-", "quad9-dnscrypt-",
                   "uncensoreddns-", "ahadns-", "plan9-dns-"]:
        if rest.startswith(prefix):
            rest = rest[len(prefix):]
            break
    rest = rest.replace("-", " ").title()
    if rest in ("Dns", "Default", ""):
        return f"{operator}"
    if country:
        return f"{rest} ({operator})"
    return f"{rest} ({operator})"

# ── Main parser ────────────────────────────────────────────────────
def parse_resolvers(text: str):
    records = []
    sections = re.split(r"(?m)^## ", text)
    for s in sections[1:]:
        lines = s.split("\n")
        name = lines[0].strip()
        # IPv4-only stack — skip explicit -ipv6 / -ip6 variants
        if name.endswith("-ipv6") or name.endswith("6"):
            continue

        desc_lines = []
        stamp = ""
        for l in lines[1:]:
            ls = l.strip()
            if not ls:
                continue
            if ls.startswith("sdns://"):
                stamp = ls
                break
            desc_lines.append(ls)
        desc = " ".join(desc_lines).strip()
        if not stamp:
            continue

        decoded = decode_dnscrypt_stamp(stamp)
        if decoded is None:
            # not DNSCrypt-protocol — skip
            continue

        operator = operator_of(name)
        country = country_from(name, desc)
        tier = eyes_tier(country)
        filters = filters_from_desc(desc, decoded["no_filter"])
        priv = privacy_score(decoded["no_logs"], decoded["dnssec"],
                             decoded["no_filter"], tier)
        trust = trust_score(operator, decoded)
        label = make_label(name, country, operator)

        records.append(OrderedDict([
            ("name", name),
            ("label", label),
            ("operator", operator),
            ("country", country),
            ("eyes", tier),
            ("transport", "DNSCrypt"),
            ("port", decoded["port"]),
            ("addr", decoded["addr"]),
            ("dnssec", decoded["dnssec"]),
            ("no_logs", decoded["no_logs"]),
            ("no_filter", decoded["no_filter"]),
            ("filters", filters),
            ("privacy_score", priv),
            ("trust_score", trust),
            ("operator_tier", OPERATOR_TIER.get(operator, 1)),
            ("description", desc[:200]),
        ]))
    return records

def main():
    print(f"fetching {UPSTREAM_URL}", file=sys.stderr)
    with urllib.request.urlopen(UPSTREAM_URL, timeout=20) as r:
        text = r.read().decode("utf-8")

    records = parse_resolvers(text)
    print(f"parsed {len(records)} DNSCrypt v2 IPv4 resolvers", file=sys.stderr)

    # Sort: operator A→Z, country A→Z, name. Operator grouping renders nicely.
    records.sort(key=lambda r: (r["operator"].lower(), r["country"], r["name"]))

    # Quick summary
    from collections import Counter
    ops = Counter(r["operator"] for r in records)
    print("Operator distribution:", file=sys.stderr)
    for o, n in ops.most_common(15):
        print(f"  {o:20} {n}", file=sys.stderr)
    print("Privacy / trust score distribution:", file=sys.stderr)
    privc = Counter(r["privacy_score"] for r in records)
    trustc = Counter(r["trust_score"] for r in records)
    for s in range(6):
        print(f"  privacy={s}: {privc[s]:3}    trust={s}: {trustc[s]:3}", file=sys.stderr)
    countries = Counter(r["country"] or "?" for r in records)
    print(f"Country coverage: {len(countries)} distinct", file=sys.stderr)

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUT_PATH, "w") as f:
        json.dump(records, f, indent=2, ensure_ascii=False)
        f.write("\n")
    print(f"wrote {OUT_PATH}", file=sys.stderr)


if __name__ == "__main__":
    main()
