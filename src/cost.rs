//! Model pricing and cost accounting.
//!
//! Stoke ships with **no model names and no prices**. Everything here comes from
//! the operator's `[pricing]` table. That is not just an abstraction preference:
//! a gateway that guesses a price guesses wrong, and a spend firewall that
//! silently prices an unknown model at $0 does not enforce anything — the budget
//! meter never moves and the cap never trips.
//!
//! So the rule is: a provider on a metered tier may only serve a model whose
//! price the operator declared. Anything else is refused before dispatch.

use once_cell::sync::{Lazy, OnceCell};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-model pricing (USD per 1M tokens).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_1m: f64,
    pub output_per_1m: f64,
}

/// What to do when a metered provider is asked to serve a model with no
/// configured price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unpriced {
    /// Refuse the request. Spend you cannot measure is spend you cannot cap.
    Refuse,
    /// Serve it and meter it at $0. Opt-in only, for operators who accept that
    /// `budget_usd` will not see this traffic.
    Free,
}

impl Unpriced {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "free" | "allow" => Unpriced::Free,
            _ => Unpriced::Refuse,
        }
    }
}

/// Tiers that run on hardware the operator owns. Compute there is not billed
/// per token, so an absent price is the truth rather than a gap.
///
/// Note the omission: an empty tier is NOT free. A provider with no declared
/// tier could be anything, and defaulting the ambiguous case to "free" is how a
/// cloud endpoint ends up serving unmetered traffic. Config load rejects it.
pub fn is_free_tier(tier: &str) -> bool {
    matches!(tier, "local" | "remote")
}

/// Cost calculation from token usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
}

impl CostBreakdown {
    pub fn zero(model: &str) -> Self {
        Self {
            model: model.to_string(),
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cost_usd: 0.0,
        }
    }

    pub fn merge(&mut self, other: &CostBreakdown) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        self.cost_usd += other.cost_usd;
    }
}

/// Registry of operator-declared model prices.
pub struct Pricer {
    prices: HashMap<String, ModelPricing>,
    unpriced: Unpriced,
}

impl Default for Pricer {
    /// Empty and fail-closed. A Pricer nobody configured must not quietly
    /// authorise metered traffic.
    fn default() -> Self {
        Self { prices: HashMap::new(), unpriced: Unpriced::Refuse }
    }
}

/// Stable prefix on the dispatch gate's refusal. The gate lives in the router,
/// where every failure is a `String` the handler turns into a 502; this lets the
/// handler recognise its own refusal and answer 403 instead. Declining to spend
/// is a policy decision, not an upstream failure.
pub const UNPRICED_ERROR: &str = "no price configured for model";

/// Did this dispatch error come from the pricing gate rather than a provider?
pub fn is_unpriced_error(msg: &str) -> bool {
    msg.contains(UNPRICED_ERROR)
}

impl Pricer {
    pub fn new(prices: HashMap<String, ModelPricing>, unpriced: Unpriced) -> Self {
        Self { prices, unpriced }
    }

    pub fn is_priced(&self, model: &str) -> bool {
        self.prices.contains_key(model)
    }

    pub fn unpriced_policy(&self) -> Unpriced {
        self.unpriced
    }

    /// The dispatch gate. Called before a provider is contacted — the only
    /// place a refusal is still free.
    pub fn allows(&self, tier: &str, model: &str) -> Result<(), String> {
        if is_free_tier(tier) || self.is_priced(model) || self.unpriced == Unpriced::Free {
            return Ok(());
        }
        Err(format!(
            "{UNPRICED_ERROR} '{model}' on a '{tier}' provider, so its \
             spend cannot be metered and `budget_usd` could not enforce a cap. Add:\n\n  \
             [pricing.models.\"{model}\"]\n  input_per_1m = <usd>\n  output_per_1m = <usd>\n\n\
             or set `[pricing] unpriced = \"free\"` to serve it unmetered."
        ))
    }

    /// Calculate cost from a usage object (OpenAI shape).
    ///
    /// An unknown model yields $0 — which is only ever reached for traffic
    /// `allows` already authorised: a free tier, or an explicit `unpriced =
    /// "free"`. It is never a silent fallback for metered traffic.
    pub fn calculate(&self, model: &str, usage: Option<&serde_json::Value>) -> CostBreakdown {
        let (prompt_tokens, completion_tokens) = usage
            .map(|u| {
                (
                    u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                    u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                )
            })
            .unwrap_or((0, 0));

        let cost_usd = self
            .prices
            .get(model)
            .map(|p| {
                (prompt_tokens as f64 * p.input_per_1m / 1_000_000.0)
                    + (completion_tokens as f64 * p.output_per_1m / 1_000_000.0)
            })
            .unwrap_or(0.0);

        CostBreakdown {
            model: model.to_string(),
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cost_usd,
        }
    }

    pub fn get_pricing(&self, model: &str) -> Option<&ModelPricing> {
        self.prices.get(model)
    }

    pub fn all_prices(&self) -> &HashMap<String, ModelPricing> {
        &self.prices
    }
}

// The dispatch gate lives deep in router.rs, below every handler that would have
// to thread a &Pricer through six call sites. Set once at startup from config.
static GLOBAL: OnceCell<Pricer> = OnceCell::new();
static FALLBACK: Lazy<Pricer> = Lazy::new(Pricer::default);

pub fn init(pricer: Pricer) {
    let _ = GLOBAL.set(pricer);
}

/// The configured pricer, or a fail-closed empty one if startup never ran.
pub fn global() -> &'static Pricer {
    GLOBAL.get().unwrap_or(&FALLBACK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricer(unpriced: Unpriced) -> Pricer {
        let mut m = HashMap::new();
        m.insert("known".to_string(), ModelPricing { input_per_1m: 1.0, output_per_1m: 2.0 });
        Pricer::new(m, unpriced)
    }

    #[test]
    fn free_tiers_never_need_a_price() {
        let p = pricer(Unpriced::Refuse);
        assert!(p.allows("local", "anything-at-all").is_ok());
        assert!(p.allows("remote", "anything-at-all").is_ok());
    }

    #[test]
    fn metered_tier_refuses_an_unpriced_model() {
        let p = pricer(Unpriced::Refuse);
        let err = p.allows("cloud", "mystery").unwrap_err();
        assert!(err.contains("no price configured"));
        assert!(err.contains("mystery"), "the error must name the model to be actionable");
    }

    #[test]
    fn an_empty_tier_is_metered_not_free() {
        // The ambiguous case must not default to free; that is the fail-open.
        let p = pricer(Unpriced::Refuse);
        assert!(p.allows("", "mystery").is_err());
    }

    #[test]
    fn metered_tier_allows_a_priced_model() {
        assert!(pricer(Unpriced::Refuse).allows("cloud", "known").is_ok());
    }

    #[test]
    fn opt_out_serves_unpriced_models_unmetered() {
        assert!(pricer(Unpriced::Free).allows("cloud", "mystery").is_ok());
    }

    #[test]
    fn cost_uses_the_configured_price() {
        let usage = serde_json::json!({"prompt_tokens": 1_000_000, "completion_tokens": 1_000_000});
        let c = pricer(Unpriced::Refuse).calculate("known", Some(&usage));
        assert!((c.cost_usd - 3.0).abs() < 1e-9, "1M in @ $1 + 1M out @ $2 = $3, got {}", c.cost_usd);
    }

    #[test]
    fn no_substring_heuristics() {
        // The old code priced anything containing "cloud" at $0.50/$2.00.
        let usage = serde_json::json!({"prompt_tokens": 1_000_000, "completion_tokens": 0});
        let c = pricer(Unpriced::Refuse).calculate("something:cloud", Some(&usage));
        assert_eq!(c.cost_usd, 0.0, "a name must never imply a price");
    }

    #[test]
    fn a_default_pricer_authorises_nothing_metered() {
        assert!(Pricer::default().allows("cloud", "x").is_err());
    }

    #[test]
    fn the_refusal_is_recognisable_so_it_maps_to_403_not_502() {
        // The handler sniffs for this to distinguish a policy refusal from an
        // upstream failure. Reword the message and the status silently regresses.
        let err = pricer(Unpriced::Refuse).allows("cloud", "mystery").unwrap_err();
        assert!(is_unpriced_error(&err));
        assert!(!is_unpriced_error("Provider foo returned 500: upstream exploded"));
    }
}
