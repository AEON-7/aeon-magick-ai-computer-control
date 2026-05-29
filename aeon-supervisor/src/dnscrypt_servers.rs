//! Metadata + criteria-based selection for upstream DNSCrypt
//! resolvers (v55+).
//!
//! Parallels `dnscrypt_relays.rs` but for the upstream-resolver side
//! of the DNSCrypt request. Where `dnscrypt_relays` cared about
//! operator diversity (no two relays from the same operator), the
//! resolver layer only routes through ONE server per query — the one
//! dnscrypt-proxy's internal latency probe rates as fastest.
//!
//! So our model here is different:
//!   - User can pick ONE specific server (mode="specific"), OR
//!   - User specifies criteria (mode="auto") and we hand
//!     dnscrypt-proxy ALL matching servers. Its
//!     `lb_strategy = "p2"` weighted-power-of-two picks the
//!     lowest-latency one live, refreshing every couple minutes.
//!
//! Criteria the user can apply:
//!   - no_logs           (operator-declared in the sdns:// stamp)
//!   - dnssec_validating (operator-declared)
//!   - no_filter         (raw answers, no malware/ad blocking)
//!   - outside_5_eyes    (relay country ∉ {US, GB, CA, AU, NZ})
//!   - outside_14_eyes   (∉ Five Eyes + {FR, DE, NL, NO, DK, BE, IT, ES, SE})
//!   - min_trust_score   (1-5, computed from operator reputation tier)
//!
//! Each catalog entry carries privacy_score + trust_score (0-5)
//! derived at build time by `scripts/regenerate-dnscrypt-resolvers.py`
//! from a mix of stamp-declared properties + a hand-curated operator
//! reputation table. The UI shows the scores so users can sort/filter
//! intelligently.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

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
pub struct Resolver {
    pub name: String,
    pub label: String,
    pub operator: String,
    pub country: String,
    pub eyes: EyesTier,
    pub transport: String,
    pub port: u16,
    pub addr: String,
    pub dnssec: bool,
    pub no_logs: bool,
    pub no_filter: bool,
    #[serde(default)]
    pub filters: Vec<String>,
    pub privacy_score: u8,
    pub trust_score: u8,
    pub operator_tier: u8,
    #[serde(default)]
    pub description: String,
}

const EMBEDDED_RESOLVERS_JSON: &str = include_str!("../data/dnscrypt-resolvers.json");
static CATALOG: OnceLock<Vec<Resolver>> = OnceLock::new();

pub fn catalog() -> &'static [Resolver] {
    CATALOG
        .get_or_init(|| {
            serde_json::from_str::<Vec<Resolver>>(EMBEDDED_RESOLVERS_JSON)
                .unwrap_or_else(|e| {
                    tracing::error!("failed to parse embedded dnscrypt-resolvers.json: {e}");
                    Vec::new()
                })
        })
        .as_slice()
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ResolverCriteria {
    #[serde(default)]
    pub no_logs: bool,
    #[serde(default)]
    pub dnssec: bool,
    #[serde(default)]
    pub no_filter: bool,
    #[serde(default)]
    pub outside_five_eyes: bool,
    #[serde(default)]
    pub outside_fourteen_eyes: bool,
    /// 1-5. 0 means "don't care". Filters out resolvers whose
    /// trust_score is below this floor.
    #[serde(default)]
    pub min_trust_score: u8,
}

impl ResolverCriteria {
    fn passes(&self, r: &Resolver) -> bool {
        if self.no_logs && !r.no_logs {
            return false;
        }
        if self.dnssec && !r.dnssec {
            return false;
        }
        if self.no_filter && !r.no_filter {
            return false;
        }
        if self.outside_five_eyes && r.eyes == EyesTier::Five {
            return false;
        }
        if self.outside_fourteen_eyes
            && matches!(r.eyes, EyesTier::Five | EyesTier::Nine | EyesTier::Fourteen)
        {
            return false;
        }
        if self.min_trust_score > 0 && r.trust_score < self.min_trust_score {
            return false;
        }
        true
    }
}

/// Filter the catalog by user criteria + return the matching resolver
/// names — these get fed as a list to dnscrypt-proxy's `server_names`
/// so its internal latency probe routes per-query to the fastest one.
///
/// `cap` caps the list to a reasonable size — dnscrypt-proxy probes
/// every server on startup which gets slow above a few dozen.
///
/// Diversity: a single operator can have dozens of equally-scored
/// PoPs (CryptoStorm: 37, DNSCry.pt: 148). Letting one operator
/// monopolise the auto-pick pool defeats the "trust spread" the user
/// wants from filtering. So we soft-cap PER OPERATOR at
/// `cap / 4`-ish, giving at least 4 different operators a slot
/// before any of them gets a second. Then we relax and fill the
/// rest by privacy/trust/latency ranking.
pub fn auto_pick(criteria: &ResolverCriteria, cap: usize) -> Vec<String> {
    let mut pool: Vec<&'static Resolver> =
        catalog().iter().filter(|r| criteria.passes(r)).collect();

    // Higher is better — sort descending. Stable so within-tier
    // ordering follows the catalog's input order (operator A→Z).
    pool.sort_by(|a, b| {
        b.privacy_score
            .cmp(&a.privacy_score)
            .then(b.trust_score.cmp(&a.trust_score))
            .then(b.operator_tier.cmp(&a.operator_tier))
    });

    // Two passes to enforce operator diversity. Pass 1: one slot per
    // operator until we've collected at least 4 distinct operators
    // OR exhausted the pool. Pass 2: greedy fill by sort order with
    // a per-operator cap. Pass 3: if still short, fill freely.
    let per_op_cap = ((cap + 3) / 4).max(2);
    let mut picked: Vec<&'static Resolver> = Vec::new();
    let mut op_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();

    // Pass 1: take the highest-scoring resolver from each operator
    // in turn until we've covered everyone or hit the cap.
    let mut seen_operators: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for r in &pool {
        if picked.len() >= cap {
            break;
        }
        if seen_operators.contains(r.operator.as_str()) {
            continue;
        }
        seen_operators.insert(r.operator.as_str());
        *op_counts.entry(r.operator.as_str()).or_insert(0) += 1;
        picked.push(r);
    }

    // Pass 2: fill remaining slots, respecting the per-operator cap.
    for r in &pool {
        if picked.len() >= cap {
            break;
        }
        if picked.iter().any(|p| p.name == r.name) {
            continue;
        }
        let cnt = op_counts.get(r.operator.as_str()).copied().unwrap_or(0);
        if cnt >= per_op_cap {
            continue;
        }
        *op_counts.entry(r.operator.as_str()).or_insert(0) += 1;
        picked.push(r);
    }

    // Pass 3: emergency fallback when criteria are so strict the pool
    // doesn't fill even with the cap relaxed.
    for r in &pool {
        if picked.len() >= cap {
            break;
        }
        if picked.iter().any(|p| p.name == r.name) {
            continue;
        }
        picked.push(r);
    }

    picked.into_iter().map(|r| r.name.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_loads() {
        let c = catalog();
        assert!(c.len() > 50, "catalog should have many resolvers, got {}", c.len());
    }

    #[test]
    fn strict_criteria_yields_some() {
        let crit = ResolverCriteria {
            no_logs: true,
            dnssec: true,
            outside_five_eyes: true,
            outside_fourteen_eyes: false,
            no_filter: false,
            min_trust_score: 3,
        };
        let picks = auto_pick(&crit, 20);
        assert!(!picks.is_empty(), "strict criteria should still find some resolvers");
        // Spot-check every pick actually meets the criteria
        let cat = catalog();
        for n in &picks {
            let r = cat.iter().find(|r| &r.name == n).expect("name in catalog");
            assert!(r.no_logs, "{n} should be no_logs");
            assert!(r.dnssec, "{n} should DNSSEC validate");
            assert_ne!(r.eyes, EyesTier::Five, "{n} should be outside 5 Eyes");
            assert!(r.trust_score >= 3, "{n} should have trust >= 3");
        }
    }

    #[test]
    fn cap_respected() {
        let crit = ResolverCriteria::default();
        let picks = auto_pick(&crit, 10);
        assert!(picks.len() <= 10);
    }
}
