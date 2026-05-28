#!/usr/bin/env python3
"""
Build-time script: fetches the canonical dnscrypt-proxy v3 relays.md,
parses each anonymized-DNS relay entry, annotates with structured
metadata (operator, country, Eyes-tier, log policy, etc.), and emits
`aeon-supervisor/data/anon-relays.json` for the supervisor to embed
via include_str! at compile time.

We can't infer all of this perfectly from freeform descriptions, so
we layer:
  - Operator: name-prefix heuristics (anon-cs-* = CryptoStorm, etc.)
  - Country: keyword search in the description, mapped to ISO codes
  - Eyes tier: country -> alliance lookup
  - Log policy: defaults to "no_logs" unless description says otherwise
                (the v3 list informally curates for no-log operators)

Re-run when the upstream list changes meaningfully. The bake won't
auto-refresh — that's deliberate; the supervisor's catalog is
deterministic per release.

Usage:
    python3 scripts/regenerate-anon-relays.py
"""

import json
import os
import re
import sys
import urllib.request
from pathlib import Path

UPSTREAM_URL = (
    "https://raw.githubusercontent.com/DNSCrypt/dnscrypt-resolvers/"
    "master/v3/relays.md"
)
OUT_PATH = (
    Path(__file__).resolve().parent.parent
    / "aeon-supervisor"
    / "data"
    / "anon-relays.json"
)

# ── Country mapping ──────────────────────────────────────────────────
# Each tuple is (regex matched against the description, ISO 3166 alpha-2).
# Order matters — earlier regexes win. Cities are listed before country
# names so "Berlin" → DE rather than the default-DE country regex
# claiming everything german.
COUNTRY_PATTERNS = [
    # Cities → country (helps when description gives only a city)
    (r"\b(zurich|zürich|swiss|switzerland|geneva|basel)\b", "CH"),
    (r"\b(amsterdam|netherlands|nl |the netherlands|eygelshoven|naaldwijk)\b", "NL"),
    (r"\b(berlin|frankfurt|munich|nuremberg|nürnberg|dusseldorf|düsseldorf|jena|bremen|hamburg|stuttgart|cologne|köln|molln|leipzig|wiesbaden|germany|german)\b", "DE"),
    (r"\b(paris|marseille|lyon|france|french|fr )\b", "FR"),
    (r"\b(london|manchester|birmingham|coventry|redditch|newcastle|england|britain|uk |united kingdom)\b", "GB"),
    (r"\b(milan|rome|palermo|italy|italian)\b", "IT"),
    (r"\b(madrid|barcelona|spain|spanish)\b", "ES"),
    (r"\b(lisbon|porto|portugal)\b", "PT"),
    (r"\b(stockholm|sweden|swedish|hudiksvall|sandefjord)\b", "SE"),
    (r"\b(oslo|bergen|norway|norwegian)\b", "NO"),
    (r"\b(copenhagen|denmark|danish)\b", "DK"),
    (r"\b(helsinki|finland|finnish|tuusula)\b", "FI"),
    (r"\b(reykjavik|iceland|hafnarfjordur)\b", "IS"),
    (r"\b(dublin|ireland|irish)\b", "IE"),
    (r"\b(brussels|belgium)\b", "BE"),
    (r"\b(vienna|wien|austria|austrian)\b", "AT"),
    (r"\b(prague|czech republic|czech|brno)\b", "CZ"),
    (r"\b(warsaw|krakow|gdansk|gdańsk|poland|polish)\b", "PL"),
    (r"\b(budapest|hungary|hungarian)\b", "HU"),
    (r"\b(romania|romanian|bucharest|timisoara|timișoara|oradea)\b", "RO"),
    (r"\b(luxembourg)\b", "LU"),
    (r"\b(slovakia|slovak|bratislava)\b", "SK"),
    (r"\b(slovenia|ljubljana)\b", "SI"),
    (r"\b(serbia|serbian|belgrade)\b", "RS"),
    (r"\b(moldova|moldovan|chisinau|chișinău)\b", "MD"),
    (r"\b(estonia|tallinn)\b", "EE"),
    (r"\b(latvia|riga)\b", "LV"),
    (r"\b(lithuania|vilnius)\b", "LT"),
    (r"\b(ukraine|kyiv|kiev)\b", "UA"),
    (r"\b(bulgaria|sofia)\b", "BG"),
    (r"\b(greece|greek|athens|thessaloniki)\b", "GR"),
    (r"\b(albania|tirana)\b", "AL"),
    (r"\b(russia|moscow|saint petersburg)\b", "RU"),
    (r"\b(armenia|yerevan)\b", "AM"),
    (r"\b(azerbaijan|baku)\b", "AZ"),
    (r"\b(turkey|turkish|istanbul|ankara)\b", "TR"),
    (r"\b(israel|tel aviv|telaviv|jerusalem)\b", "IL"),
    (r"\b(uae|united arab emirates|dubai|fujairah)\b", "AE"),
    (r"\b(pakistan|islamabad|karachi|lahore)\b", "PK"),
    (r"\b(india|mumbai|bengaluru|bangalore|new delhi|chennai|hyderabad)\b", "IN"),
    (r"\b(bangladesh|dhaka)\b", "BD"),
    (r"\b(thailand|bangkok)\b", "TH"),
    (r"\b(vietnam|hanoi|ho chi minh)\b", "VN"),
    (r"\b(philippines|manila)\b", "PH"),
    (r"\b(indonesia|jakarta)\b", "ID"),
    (r"\b(singapore)\b", "SG"),
    (r"\b(hong kong)\b", "HK"),
    (r"\b(taiwan|taipei)\b", "TW"),
    (r"\b(south korea|korea|seoul|busan)\b", "KR"),
    (r"\b(japan|japanese|tokyo|osaka|nagoya|conoha)\b", "JP"),
    (r"\b(australia|sydney|melbourne|brisbane|adelaide|perth|canberra|sydney02)\b", "AU"),
    (r"\b(new zealand|auckland|wellington)\b", "NZ"),
    (r"\b(south africa|johannesburg|cape town|capetown)\b", "ZA"),
    (r"\b(nigeria|lagos|abuja|ikeja)\b", "NG"),
    (r"\b(mauritius|ebene|ebène)\b", "MU"),
    (r"\b(brazil|sao paulo|são paulo|saopaulo|rio de janeiro)\b", "BR"),
    (r"\b(argentina|buenos aires)\b", "AR"),
    (r"\b(chile|santiago|valdivia)\b", "CL"),
    (r"\b(colombia|bogota|bogotá)\b", "CO"),
    (r"\b(peru|lima)\b", "PE"),
    (r"\b(ecuador|guayaquil|quito)\b", "EC"),
    (r"\b(mexico|cdmx|guadalajara|querétaro|queretaro)\b", "MX"),
    # US comes last + matches city-state combos
    (r"\b(new york|nyc|los angeles|chicago|seattle|miami|atlanta|dallas|houston|"
     r"phoenix|denver|portland|san francisco|sanjose|santaclara|fremont|"
     r"jacksonville|tampa|las vegas|grand rapids|kansas city|salt lake city|"
     r"saltlakecity|libertylake|berkeley springs|berkeleysprings|allentown|"
     r"flint|detroit|indianapolis|durham|spokane|ashburn|ogden|ottoville|"
     r"taos|port edwards|portedwards|usa|united states|u\.s\.a|washington d\.?c)\b", "US"),
    (r"\b(canada|toronto|montreal|vancouver|calgary|halifax|ottawa)\b", "CA"),
]

# Some cities only ever appear inside the relay name (not description),
# so let the name itself also be matched against the same patterns.
NAME_CITY_PATTERNS = [
    (r"-(zurich|geneva)$", "CH"),
    (r"-(amsterdam|naaldwijk|eygelshoven)(\d+)?-ipv4$", "NL"),
    (r"-(berlin|frankfurt|munich|nuremberg|dusseldorf|bremen|jena|molln|hamburg|cologne|stuttgart)(\d+)?-ipv4$", "DE"),
    (r"-(paris|marseille|lyon)(\d+)?-ipv4$", "FR"),
    (r"-(london|manchester|coventry|redditch|newcastle)(\d+)?-ipv4$", "GB"),
    (r"-(milan|palermo|rome)(\d+)?-ipv4$", "IT"),
    (r"-(madrid|barcelona)(\d+)?-ipv4$", "ES"),
    (r"-(lisbon|porto)(\d+)?-ipv4$", "PT"),
    (r"-(stockholm|hudiksvall|sandefjord)(\d+)?-ipv4$", "SE"),
    (r"-(oslo|bergen)(\d+)?-ipv4$", "NO"),
    (r"-(copenhagen)(\d+)?-ipv4$", "DK"),
    (r"-(helsinki|tuusula)(\d+)?-ipv4$", "FI"),
    (r"-(hafnarfjordur|reykjavik)(\d+)?-ipv4$", "IS"),
    (r"-(dublin)(\d+)?-ipv4$", "IE"),
    (r"-(brussels)(\d+)?-ipv4$", "BE"),
    (r"-(vienna|wien)(\d+)?-ipv4$", "AT"),
    (r"-(prague|brno)(\d+)?-ipv4$", "CZ"),
    (r"-(warsaw|krakow|gdansk)(\d+)?-ipv4$", "PL"),
    (r"-(budapest)(\d+)?-ipv4$", "HU"),
    (r"-(bucharest|timisoara|oradea)(\d+)?-ipv4$", "RO"),
    (r"-(luxembourg)(\d+)?-ipv4$", "LU"),
    (r"-(bratislava)(\d+)?-ipv4$", "SK"),
    (r"-(ljubljana)(\d+)?-ipv4$", "SI"),
    (r"-(belgrade)(\d+)?-ipv4$", "RS"),
    (r"-(chisinau|chișinău)(\d+)?-ipv4$", "MD"),
    (r"-(tallinn)(\d+)?-ipv4$", "EE"),
    (r"-(riga)(\d+)?-ipv4$", "LV"),
    (r"-(vilnius)(\d+)?-ipv4$", "LT"),
    (r"-(kyiv|kiev)(\d+)?-ipv4$", "UA"),
    (r"-(sofia)(\d+)?-ipv4$", "BG"),
    (r"-(athens|thessaloniki)(\d+)?-ipv4$", "GR"),
    (r"-(tirana)(\d+)?-ipv4$", "AL"),
    (r"-(moscow)(\d+)?-ipv4$", "RU"),
    (r"-(yerevan)(\d+)?-ipv4$", "AM"),
    (r"-(baku)(\d+)?-ipv4$", "AZ"),
    (r"-(telaviv)(\d+)?-ipv4$", "IL"),
    (r"-(fujairah)(\d+)?-ipv4$", "AE"),
    (r"-(islamabad)(\d+)?-ipv4$", "PK"),
    (r"-(mumbai|bengaluru|chennai|hyderabad|newdelhi)(\d+)?-ipv4$", "IN"),
    (r"-(dhaka)(\d+)?-ipv4$", "BD"),
    (r"-(bangkok)(\d+)?-ipv4$", "TH"),
    (r"-(hanoi)(\d+)?-ipv4$", "VN"),
    (r"-(manila)(\d+)?-ipv4$", "PH"),
    (r"-(jakarta)(\d+)?-ipv4$", "ID"),
    (r"-(singapore)(\d+)?-ipv4$", "SG"),
    (r"-(hongkong)(\d+)?-ipv4$", "HK"),
    (r"-(seoul|busan)(\d+)?-ipv4$", "KR"),
    (r"-(tokyo|osaka|nagoya)(\d+)?-ipv4$", "JP"),
    (r"-(sydney|melbourne|brisbane|adelaide|perth)(\d+)?-ipv4$", "AU"),
    (r"-(auckland)(\d+)?-ipv4$", "NZ"),
    (r"-(johannesburg|capetown)(\d+)?-ipv4$", "ZA"),
    (r"-(lagos|ikeja)(\d+)?-ipv4$", "NG"),
    (r"-(ebenecity|ebene)(\d+)?-ipv4$", "MU"),
    (r"-(saopaulo|riodejaneiro|rio)(\d+)?-ipv4$", "BR"),
    (r"-(buenosaires)(\d+)?-ipv4$", "AR"),
    (r"-(valdivia|santiago)(\d+)?-ipv4$", "CL"),
    (r"-(bogota)(\d+)?-ipv4$", "CO"),
    (r"-(lima)(\d+)?-ipv4$", "PE"),
    (r"-(guayaquil|quito)(\d+)?-ipv4$", "EC"),
    (r"-(queretaro|cdmx|mexicocity)(\d+)?-ipv4$", "MX"),
    (r"-(newyork|losangeles|chicago|seattle|miami|atlanta|dallas|houston|"
     r"phoenix|denver|portland|sanfrancisco|sanjose|santaclara|fremont|"
     r"jacksonville|tampa|lasvegas|grandrapids|kansascity|saltlakecity|"
     r"libertylake|berkeleysprings|allentown|flint|detroit|indianapolis|"
     r"durham|spokane|ashburn|ogden|ottoville|taos|portedwards|philadelphia|"
     r"boston|baltimore|cleveland|stlouis|nashville|memphis|austin|sanantonio|"
     r"orlando|columbus|charlotte|raleigh|pittsburgh|cincinnati|milwaukee)"
     r"(\d+)?-ipv4$", "US"),
    (r"-(toronto|montreal|vancouver|calgary|halifax|ottawa)(\d+)?-ipv4$", "CA"),
    # CryptoStorm uses two-letter codes inline
    (r"^anon-cs-ch$", "CH"),
    (r"^anon-cs-md$", "MD"),
    (r"^anon-cs-nl$", "NL"),
    (r"^anon-cs-fr$", "FR"),
    (r"^anon-cs-de$", "DE"),
    (r"^anon-cs-il$", "US"),  # CS uses US state codes too — these need manual mapping
    (r"^anon-cs-fl$", "US"),
    (r"^anon-cs-ga$", "US"),
    (r"^anon-cs-nv$", "US"),
    (r"^anon-cs-nyc$", "US"),
    (r"^anon-cs-tx$", "US"),
    (r"^anon-cs-ore$", "US"),
    (r"^anon-cs-sea$", "US"),
    (r"^anon-cs-la$", "US"),
    (r"^anon-cs-dc$", "US"),
    (r"^anon-cs-ro$", "RO"),
    (r"^anon-cs-pt$", "PT"),
    (r"^anon-cs-it$", "IT"),
    (r"^anon-cs-sk$", "SK"),
    (r"^anon-cs-se$", "SE"),
    (r"^anon-cs-swe$", "SE"),
    (r"^anon-cs-norway$", "NO"),
    (r"^anon-cs-finland$", "FI"),
    (r"^anon-cs-poland$", "PL"),
    (r"^anon-cs-hungary$", "HU"),
    (r"^anon-cs-czech$", "CZ"),
    (r"^anon-cs-serbia$", "RS"),
    (r"^anon-cs-singapore$", "SG"),
    (r"^anon-cs-tokyo$", "JP"),
    (r"^anon-cs-sydney$", "AU"),
    (r"^anon-cs-london$", "GB"),
    (r"^anon-cs-manchester$", "GB"),
    (r"^anon-cs-austria$", "AT"),
    (r"^anon-cs-barcelona$", "ES"),
    (r"^anon-cs-belgium$", "BE"),
    (r"^anon-cs-berlin$", "DE"),
    (r"^anon-cs-milan$", "IT"),
    (r"^anon-cs-montreal$", "CA"),
    (r"^anon-cs-vancouver$", "CA"),
    (r"^anon-cs-dus$", "DE"),
    (r"^anon-cs-hungary$", "HU"),
]

# Five Eyes: US, UK, CA, AU, NZ.
EYES_5 = {"US", "GB", "CA", "AU", "NZ"}
# Nine Eyes adds: FR, DK, NL, NO.
EYES_9 = EYES_5 | {"FR", "DK", "NL", "NO"}
# Fourteen Eyes adds: DE, BE, IT, ES, SE.
EYES_14 = EYES_9 | {"DE", "BE", "IT", "ES", "SE"}

def eyes_tier(country: str) -> str:
    if not country:
        return "unknown"
    if country in EYES_5:
        return "five"
    if country in EYES_9:
        return "nine"
    if country in EYES_14:
        return "fourteen"
    return "none"


# ── Operator inference from relay name ──────────────────────────────
OPERATOR_PATTERNS = [
    (re.compile(r"^anon-cs-"), "CryptoStorm"),
    (re.compile(r"^dnscry\.pt-anon-"), "DNSCry.pt"),
    (re.compile(r"^anon-dnscry\.pt-"), "DNSCry.pt"),
    (re.compile(r"^anon-dnscrypt-ca-"), "DNSCrypt.ca"),
    (re.compile(r"^anon-saldns\d+-conoha-"), "μODNS"),
    (re.compile(r"^anon-scaleway"), "jedisct1"),
    (re.compile(r"^anon-kama"), "jedisct1"),
    (re.compile(r"^anon-quad9"), "Quad9"),
    (re.compile(r"^anon-restena"), "Restena"),
    (re.compile(r"^anon-flokinet"), "FlokiNET"),
    (re.compile(r"^anon-dnswarden"), "dnswarden"),
    (re.compile(r"^anon-meganerd"), "meganerd"),
    (re.compile(r"^anon-tiarap"), "tiarap"),
    (re.compile(r"^anon-tuna"), "tuna"),
    (re.compile(r"^anon-inconnu"), "inconnu"),
    (re.compile(r"^anon-acsacsar"), "acsacsar"),
    (re.compile(r"^anon-bcn"), "bcn"),
    (re.compile(r"^anon-fdn"), "FDN"),
    (re.compile(r"^anon-serbica"), "litepay"),
    (re.compile(r"^anon-yeahapi"), "YeahAPI"),
    (re.compile(r"^anon-wikimedia"), "Wikimedia"),
    (re.compile(r"^anon-ev-"), "ev"),
    (re.compile(r"^anon-plan9"), "plan9"),
]

def operator_of(name: str) -> str:
    for pat, op in OPERATOR_PATTERNS:
        if pat.match(name):
            return op
    return "independent"


def country_of(name: str, desc: str) -> str:
    text = desc.lower()
    for pat, code in COUNTRY_PATTERNS:
        if re.search(pat, text):
            return code
    for pat, code in NAME_CITY_PATTERNS:
        if re.search(pat, name):
            return code
    return ""


def parse_relays(text: str):
    """Parse the upstream relays.md into structured records.
    Filter to IPv4-only — we don't run an IPv6 stack."""
    records = []
    sections = re.split(r"(?m)^## ", text)
    for s in sections[1:]:
        lines = s.split("\n")
        name = lines[0].strip()

        # Skip IPv6-only variants — they're listed as `name-ipv6` or
        # `name6`. Mixing in IPv6 entries when we run an IPv4-only
        # stack would just give "incompatible" errors in dnscrypt-proxy.
        if name.endswith("-ipv6") or name.endswith("6") and not name.endswith("ipv4"):
            continue
        # Also skip explicit "doh" relays — anonymized DNS only makes
        # sense with DNSCrypt-protocol relays (sdns://AQ stamp). The
        # name "anon-cs-doh*" is rare but exists.
        if "-doh" in name:
            continue

        desc_lines = []
        stamp = ""
        for l in lines[1:]:
            ls = l.strip()
            if not ls:
                if desc_lines:
                    continue
                continue
            if ls.startswith("sdns://"):
                stamp = ls
                break
            desc_lines.append(ls)
        desc = " ".join(desc_lines).strip()

        # Confirm protocol is DNSCrypt v2 — stamps starting with
        # "sdns://gA" are anonymized DNS relays. Anything else
        # shouldn't be in this list, but be defensive.
        if not stamp.startswith("sdns://gA") and not stamp.startswith("sdns://gR") and not stamp.startswith("sdns://g"):
            continue

        operator = operator_of(name)
        country = country_of(name, desc)
        tier = eyes_tier(country)

        # Heuristic: nearly every v3 relay is no-logs (it's the norm
        # for inclusion). Flip to False only if the description says
        # something about logging — none currently do, but future-proof.
        no_logs = True
        if re.search(r"\blog(ging|s)?\b", desc.lower()) and "no log" not in desc.lower():
            no_logs = False

        records.append({
            "name": name,
            "label": label_for(name, desc, country),
            "operator": operator,
            "country": country,
            "eyes": tier,
            "no_logs": no_logs,
            "description": desc[:200],   # trimmed; UI doesn't need 800 chars
        })

    return records


def label_for(name: str, desc: str, country: str) -> str:
    """Build a short UI label like 'Switzerland (CryptoStorm)' or
    'Tokyo (DNSCry.pt)'."""
    # For CryptoStorm/DNSCry.pt, use the city/region segment.
    m = re.match(r"^anon-cs-(.+)$", name)
    if m:
        loc = m.group(1).replace("-", " ").title()
        return f"{loc} (CryptoStorm)"
    m = re.match(r"^dnscry\.pt-anon-(.+)-ipv4$", name)
    if m:
        loc = m.group(1).replace("-", " ").title()
        return f"{loc} (DNSCry.pt)"
    # Default: country + operator
    op = operator_of(name)
    if country:
        return f"{country} ({op})"
    return f"{name} ({op})"


def main():
    print(f"fetching {UPSTREAM_URL}", file=sys.stderr)
    with urllib.request.urlopen(UPSTREAM_URL, timeout=20) as r:
        text = r.read().decode("utf-8")

    records = parse_relays(text)
    print(f"parsed {len(records)} IPv4 anonymized relays", file=sys.stderr)

    # Sort: operator A→Z, then country A→Z, then name. Operator
    # grouping makes the UI render nicely.
    records.sort(key=lambda r: (r["operator"].lower(), r["country"], r["name"]))

    # Quick summary of country coverage
    by_country = {}
    for r in records:
        by_country.setdefault(r["country"] or "?", 0)
        by_country[r["country"] or "?"] += 1
    print("country coverage:", file=sys.stderr)
    for c, n in sorted(by_country.items(), key=lambda x: -x[1]):
        print(f"  {c or '?':4} {n}", file=sys.stderr)

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUT_PATH, "w") as f:
        json.dump(records, f, indent=2, ensure_ascii=False)
        f.write("\n")
    print(f"wrote {OUT_PATH}", file=sys.stderr)


if __name__ == "__main__":
    main()
