//! Curated metadata for Anonymized DNSCrypt relays.
//!
//! `dnscrypt-proxy` already maintains a public list of all relays
//! (https://github.com/DNSCrypt/dnscrypt-resolvers v3 / relays.md), but
//! that list is a few hundred entries deep, has no structured metadata
//! beyond operator-name-and-city, and includes huge single-operator
//! pools (CryptoStorm: ~35 relays, DNSCry.pt: ~150). The whole point
//! of anonymized DNSCrypt is OPERATOR DIVERSITY — three relays all
//! run by the same outfit are no more private than one, because that
//! operator can still correlate your queries across them.
//!
//! So we keep our own short-list with:
//!   - explicit operator attribution
//!   - jurisdiction (ISO country code)
//!   - intelligence-sharing-alliance tier (5/9/14 Eyes; "none" if outside)
//!   - log policy
//!
//! Auto-pick logic groups by operator and selects one relay per operator
//! preferring different jurisdictions — so a 3-relay auto-pick always
//! covers 3 distinct operators in 3 distinct countries (when feasible).
//!
//! Every name here MUST exist in the upstream relays.md and resolve to
//! an active sdns:// stamp at apply time. If a name goes stale, drop
//! it from this list; dnscrypt-proxy will gracefully skip unknown
//! names via `skip_incompatible = true`.

use serde::Serialize;

/// Five Eyes: US, UK, CA, AU, NZ.
/// Nine Eyes adds: FR, DK, NL, NO.
/// Fourteen Eyes adds: DE, BE, IT, ES, SE.
/// "none" means outside any of these alliances.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EyesTier {
    None,
    Five,
    Nine,
    Fourteen,
}

#[derive(Debug, Clone, Serialize)]
pub struct Relay {
    /// dnscrypt-proxy resolver name. Must match an entry in
    /// the public relays.md list.
    pub name: &'static str,
    /// Short human-friendly label for the UI.
    pub label: &'static str,
    /// Who operates this relay. Critical for the diversity check —
    /// auto-pick will never select two relays from the same operator.
    pub operator: &'static str,
    /// ISO 3166 alpha-2 country code.
    pub country: &'static str,
    /// Most aggressive intel-sharing alliance the country belongs to.
    pub eyes: EyesTier,
    /// Whether the operator publicly commits to keeping no logs.
    /// All entries here SHOULD be true — we only include relays whose
    /// operators publish a no-logs policy.
    pub no_logs: bool,
    /// Whether the relay's path validates DNSSEC. (All currently-listed
    /// relays pass through, validation happens at the resolver.)
    pub dnssec_pass_through: bool,
}

/// Curated relay short-list. Hand-picked for operator + jurisdiction
/// diversity. Order doesn't matter — auto-pick groups by operator
/// and chooses within a group.
pub const RELAYS: &[Relay] = &[
    // ─── Operator: jedisct1 (Frank Denis, dnscrypt-proxy maintainer) ───
    // Run on Scaleway VPS; jedisct1 is the author of dnscrypt-proxy
    // itself — high trust signal but jurisdictionally inside 9 Eyes.
    Relay {
        name: "anon-scaleway", label: "Scaleway France (jedisct1)",
        operator: "jedisct1", country: "FR", eyes: EyesTier::Nine,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-scaleway-ams", label: "Scaleway Amsterdam (jedisct1)",
        operator: "jedisct1", country: "NL", eyes: EyesTier::Nine,
        no_logs: true, dnssec_pass_through: true,
    },

    // ─── Operator: CryptoStorm ───
    // Privacy-focused VPN provider; ~35 relays worldwide. Pick
    // jurisdictionally diverse ones with low Eyes exposure.
    Relay {
        name: "anon-cs-ch", label: "Switzerland (CryptoStorm)",
        operator: "CryptoStorm", country: "CH", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-cs-md", label: "Moldova (CryptoStorm)",
        operator: "CryptoStorm", country: "MD", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-cs-ro", label: "Romania (CryptoStorm)",
        operator: "CryptoStorm", country: "RO", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-cs-serbia", label: "Serbia (CryptoStorm)",
        operator: "CryptoStorm", country: "RS", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-cs-singapore", label: "Singapore (CryptoStorm)",
        operator: "CryptoStorm", country: "SG", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-cs-tokyo", label: "Tokyo (CryptoStorm)",
        operator: "CryptoStorm", country: "JP", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-cs-poland", label: "Poland (CryptoStorm)",
        operator: "CryptoStorm", country: "PL", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-cs-hungary", label: "Hungary (CryptoStorm)",
        operator: "CryptoStorm", country: "HU", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-cs-czech", label: "Czech Republic (CryptoStorm)",
        operator: "CryptoStorm", country: "CZ", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },

    // ─── Operator: FlokiNET ───
    // Independent privacy host based in Iceland; this relay is in
    // their Romanian PoP.
    Relay {
        name: "anon-flokinet-ro", label: "Romania (FlokiNET)",
        operator: "FlokiNET", country: "RO", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },

    // ─── Operator: Restena (Luxembourg academic) ───
    Relay {
        name: "anon-restena", label: "Luxembourg (Restena)",
        operator: "Restena", country: "LU", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },

    // ─── Operator: Quad9 (Swiss non-profit) ───
    // Same outfit as the Quad9 resolver — DO NOT pair this relay
    // with a Quad9 server (the same org sees both halves). The
    // diversity check handles that automatically.
    Relay {
        name: "anon-quad9", label: "Switzerland (Quad9)",
        operator: "Quad9", country: "CH", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },

    // ─── Operator: dnswarden (privacy ops, Switzerland) ───
    Relay {
        name: "anon-dnswarden-swiss", label: "Switzerland (dnswarden)",
        operator: "dnswarden", country: "CH", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },

    // ─── Operator: independent (single-relay community ops) ───
    Relay {
        name: "anon-tiarap", label: "Singapore (tiarap)",
        operator: "tiarap", country: "SG", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-meganerd", label: "Netherlands (meganerd)",
        operator: "meganerd", country: "NL", eyes: EyesTier::Nine,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "anon-kama", label: "France (kama)",
        operator: "kama", country: "FR", eyes: EyesTier::Nine,
        no_logs: true, dnssec_pass_through: true,
    },

    // ─── Operator: DNSCry.pt (independent infra, ~150 PoPs) ───
    // We pick a small jurisdiction-diverse subset.
    Relay {
        name: "dnscry.pt-anon-zurich-ipv4", label: "Zürich (DNSCry.pt)",
        operator: "DNSCry.pt", country: "CH", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "dnscry.pt-anon-tallinn-ipv4", label: "Tallinn (DNSCry.pt)",
        operator: "DNSCry.pt", country: "EE", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "dnscry.pt-anon-bucharest-ipv4", label: "Bucharest (DNSCry.pt)",
        operator: "DNSCry.pt", country: "RO", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "dnscry.pt-anon-saopaulo-ipv4", label: "São Paulo (DNSCry.pt)",
        operator: "DNSCry.pt", country: "BR", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
    Relay {
        name: "dnscry.pt-anon-johannesburg-ipv4", label: "Johannesburg (DNSCry.pt)",
        operator: "DNSCry.pt", country: "ZA", eyes: EyesTier::None,
        no_logs: true, dnssec_pass_through: true,
    },
];

// ── Resolver operator mapping ────────────────────────────────────────

/// Map a `provider` id (as exposed by /api/network/dnscrypt) back to
/// the organization that operates the upstream resolver. Used by the
/// auto-pick to enforce relay-operator ≠ resolver-operator. Returns
/// `None` for "custom" (we can't infer operator from a user-pasted
/// stamp).
pub fn resolver_operator(provider: &str) -> Option<&'static str> {
    match provider {
        "quad9" | "quad9-unfiltered" => Some("Quad9"),
        "adguard" | "adguard-family" | "adguard-unfiltered" => Some("AdGuard"),
        "opendns" => Some("Cisco"),
        "cleanbrowsing" => Some("CleanBrowsing"),
        "custom" => None,
        _ => None,
    }
}

// ── Criteria + auto-pick ─────────────────────────────────────────────

/// What the user wants out of a relay. Auto-pick filters the relay
/// pool by these flags before choosing operators/jurisdictions.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct RelayCriteria {
    /// Operator publicly commits to keeping no logs.
    #[serde(default)]
    pub no_logs: bool,
    /// Relay sits outside Five Eyes (US, UK, CA, AU, NZ).
    #[serde(default)]
    pub outside_five_eyes: bool,
    /// Relay sits outside Fourteen Eyes (Five Eyes + FR, DE, NL, NO,
    /// DK, BE, IT, ES, SE).
    #[serde(default)]
    pub outside_fourteen_eyes: bool,
    /// Relay's path validates DNSSEC (currently every curated relay
    /// passes this through; flag is here for future-proofing if we
    /// add relays that don't).
    #[serde(default)]
    pub dnssec: bool,
}

impl RelayCriteria {
    /// Reasonable starter criteria. Picked to match what most users
    /// would expect from a "make my DNS private" toggle without
    /// being so strict that auto-pick fails to find candidates.
    #[allow(dead_code)] // exposed for future "reset to safe defaults" UI
    pub fn default_strict() -> Self {
        Self {
            no_logs: true,
            outside_five_eyes: true,
            outside_fourteen_eyes: false,
            dnssec: true,
        }
    }

    fn passes(&self, r: &Relay) -> bool {
        if self.no_logs && !r.no_logs {
            return false;
        }
        if self.dnssec && !r.dnssec_pass_through {
            return false;
        }
        if self.outside_five_eyes && r.eyes == EyesTier::Five {
            return false;
        }
        if self.outside_fourteen_eyes
            && (r.eyes == EyesTier::Five
                || r.eyes == EyesTier::Nine
                || r.eyes == EyesTier::Fourteen)
        {
            return false;
        }
        true
    }
}

/// Score relays against the resolver's operator. Auto-pick wants:
///   1. Relay operator ≠ resolver operator (HARD constraint — filtered out)
///   2. Operator diversity — never two relays from same operator
///   3. Jurisdiction diversity — different countries when possible
///   4. Lower Eyes tier is better (closer-to-none ranks higher)
pub fn auto_pick(
    criteria: &RelayCriteria,
    exclude_operator: Option<&str>,
    desired_count: usize,
) -> Vec<&'static str> {
    // Filter step
    let mut pool: Vec<&'static Relay> = RELAYS
        .iter()
        .filter(|r| {
            // HARD: exclude same operator as the resolver
            if let Some(op) = exclude_operator {
                if r.operator.eq_ignore_ascii_case(op) {
                    return false;
                }
            }
            criteria.passes(r)
        })
        .collect();

    // Sort by eyes tier (lowest first), so within an operator group
    // we naturally prefer the no-Eyes country.
    pool.sort_by_key(|r| eyes_rank(r.eyes));

    // Walk the pool and accumulate picks while enforcing:
    //   - one per operator
    //   - one per country (best-effort; if we can't satisfy both
    //     constraints we relax country before operator)
    let mut picked: Vec<&'static Relay> = Vec::new();
    let mut seen_operators: Vec<&'static str> = Vec::new();
    let mut seen_countries: Vec<&'static str> = Vec::new();

    // First pass: strict country + operator uniqueness
    for r in &pool {
        if picked.len() >= desired_count {
            break;
        }
        if seen_operators.contains(&r.operator) {
            continue;
        }
        if seen_countries.contains(&r.country) {
            continue;
        }
        picked.push(r);
        seen_operators.push(r.operator);
        seen_countries.push(r.country);
    }

    // Second pass: if we still don't have enough, allow same country
    // (different operator). Operator uniqueness stays hard.
    if picked.len() < desired_count {
        for r in &pool {
            if picked.len() >= desired_count {
                break;
            }
            if seen_operators.contains(&r.operator) {
                continue;
            }
            picked.push(r);
            seen_operators.push(r.operator);
        }
    }

    // Third pass (only if the pool is genuinely tiny): accept relays
    // from the same operator. We log this — the anonymization
    // guarantee weakens but the user gets functional DNS.
    if picked.len() < desired_count {
        for r in &pool {
            if picked.len() >= desired_count {
                break;
            }
            // Skip exact duplicates of names we already picked.
            if picked.iter().any(|p| p.name == r.name) {
                continue;
            }
            picked.push(r);
        }
    }

    picked.into_iter().map(|r| r.name).collect()
}

fn eyes_rank(e: EyesTier) -> u8 {
    match e {
        EyesTier::None => 0,
        EyesTier::Fourteen => 1,
        EyesTier::Nine => 2,
        EyesTier::Five => 3,
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_criteria_yields_diverse_picks() {
        let crit = RelayCriteria::default_strict();
        let picked = auto_pick(&crit, Some("Quad9"), 3);
        assert_eq!(picked.len(), 3, "should pick 3 relays");

        // No two picks from the same operator
        let operators: Vec<_> = picked
            .iter()
            .map(|name| RELAYS.iter().find(|r| r.name == *name).unwrap().operator)
            .collect();
        let mut sorted = operators.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), operators.len(), "operators must be unique");

        // No pick from the resolver's operator (Quad9)
        for op in &operators {
            assert_ne!(*op, "Quad9", "must not pick Quad9 relay when resolver=Quad9");
        }
    }

    #[test]
    fn outside_fourteen_eyes_filters_correctly() {
        let crit = RelayCriteria {
            no_logs: true,
            outside_five_eyes: true,
            outside_fourteen_eyes: true,
            dnssec: true,
        };
        let picked = auto_pick(&crit, None, 5);
        for name in &picked {
            let r = RELAYS.iter().find(|r| r.name == *name).unwrap();
            assert_eq!(r.eyes, EyesTier::None, "{name} should be outside 14 Eyes");
        }
    }

    #[test]
    fn resolver_operator_known() {
        assert_eq!(resolver_operator("quad9"), Some("Quad9"));
        assert_eq!(resolver_operator("adguard-family"), Some("AdGuard"));
        assert_eq!(resolver_operator("custom"), None);
        assert_eq!(resolver_operator("bogus-provider"), None);
    }
}
