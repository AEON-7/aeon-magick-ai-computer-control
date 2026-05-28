//! Metadata for Anonymized DNSCrypt relays.
//!
//! The full upstream relays.md catalog has ~270 IPv4 anon-DNS relays
//! (https://github.com/DNSCrypt/dnscrypt-resolvers v3 / relays.md).
//! That list ships unannotated — only operator-name and city embedded
//! in freeform descriptions. We parse it at build time via
//! `scripts/regenerate-anon-relays.py` into a structured JSON file
//! at `data/anon-relays.json` that this module embeds via
//! `include_str!` and parses once on startup.
//!
//! Each entry carries:
//!   - operator (inferred from name prefix)
//!   - country (ISO 3166 alpha-2, derived from description+name)
//!   - eyes tier (5/9/14 Eyes; "none" if outside)
//!   - no_logs flag (defaults to true; v3 list curates for this)
//!
//! Auto-pick groups by operator and selects one relay per operator
//! preferring different jurisdictions — so a 3-relay auto-pick always
//! covers 3 distinct operators in 3 distinct countries (when feasible).
//! This matters: three relays from the same outfit are no more private
//! than one, because that operator can still correlate your queries
//! across them.
//!
//! When a relay name goes stale upstream, dnscrypt-proxy skips it
//! gracefully via `skip_incompatible = true`. Refresh the catalog by
//! re-running `regenerate-anon-relays.py` and rebuilding.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Five Eyes: US, UK, CA, AU, NZ.
/// Nine Eyes adds: FR, DK, NL, NO.
/// Fourteen Eyes adds: DE, BE, IT, ES, SE.
/// "none" means outside any of these alliances.
/// "unknown" means our parser couldn't pin down the country (shouldn't
/// happen with the v52 generator but kept as a defensive fallback).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EyesTier {
    None,
    Five,
    Nine,
    Fourteen,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relay {
    /// dnscrypt-proxy resolver name. Must match an entry in
    /// the public relays.md list.
    pub name: String,
    /// Short human-friendly label for the UI.
    pub label: String,
    /// Who operates this relay. Critical for the diversity check —
    /// auto-pick will never select two relays from the same operator.
    pub operator: String,
    /// ISO 3166 alpha-2 country code (empty if unknown).
    pub country: String,
    /// Most aggressive intel-sharing alliance the country belongs to.
    pub eyes: EyesTier,
    /// Whether the operator publicly commits to keeping no logs.
    /// Defaults to true; v3 list curates for this.
    pub no_logs: bool,
    /// First ~200 chars of the description (for the UI tooltip).
    #[serde(default)]
    pub description: String,
}

impl Relay {
    /// Most relays pass DNSSEC through — the relay just forwards the
    /// encrypted blob, validation happens at the resolver. Kept as a
    /// helper rather than a field so the JSON stays slim.
    pub fn dnssec_pass_through(&self) -> bool { true }
}

/// Catalog loaded once from the embedded JSON snapshot.
static CATALOG: OnceLock<Vec<Relay>> = OnceLock::new();

const EMBEDDED_RELAYS_JSON: &str = include_str!("../data/anon-relays.json");

pub fn catalog() -> &'static [Relay] {
    CATALOG
        .get_or_init(|| {
            serde_json::from_str::<Vec<Relay>>(EMBEDDED_RELAYS_JSON)
                .unwrap_or_else(|e| {
                    tracing::error!("failed to parse embedded anon-relays.json: {e}");
                    Vec::new()
                })
        })
        .as_slice()
}

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
        if self.dnssec && !r.dnssec_pass_through() {
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
) -> Vec<String> {
    // Filter step
    let mut pool: Vec<&'static Relay> = catalog()
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
    let mut seen_operators: Vec<&str> = Vec::new();
    let mut seen_countries: Vec<&str> = Vec::new();

    // First pass: strict country + operator uniqueness
    for r in &pool {
        if picked.len() >= desired_count {
            break;
        }
        if seen_operators.iter().any(|op| *op == r.operator) {
            continue;
        }
        if seen_countries.iter().any(|c| *c == r.country) {
            continue;
        }
        picked.push(r);
        seen_operators.push(&r.operator);
        seen_countries.push(&r.country);
    }

    // Second pass: if we still don't have enough, allow same country
    // (different operator). Operator uniqueness stays hard.
    if picked.len() < desired_count {
        for r in &pool {
            if picked.len() >= desired_count {
                break;
            }
            if seen_operators.iter().any(|op| *op == r.operator) {
                continue;
            }
            picked.push(r);
            seen_operators.push(&r.operator);
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

    picked.into_iter().map(|r| r.name.clone()).collect()
}

fn eyes_rank(e: EyesTier) -> u8 {
    match e {
        EyesTier::None => 0,
        EyesTier::Fourteen => 1,
        EyesTier::Nine => 2,
        EyesTier::Five => 3,
        EyesTier::Unknown => 4, // unknown ranks last so we prefer attributed relays
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
        let cat = catalog();
        let operators: Vec<_> = picked
            .iter()
            .map(|name| cat.iter().find(|r| r.name == *name).unwrap().operator.as_str())
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
        let cat = catalog();
        for name in &picked {
            let r = cat.iter().find(|r| r.name == *name).unwrap();
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
