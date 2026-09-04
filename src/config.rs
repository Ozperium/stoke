use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub routing: Option<String>,
    pub default_model: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub plugins: crate::plugins::PluginConfig,
    #[serde(default)]
    pub builtins: crate::builtins::BuiltinConfig,
    #[serde(default)]
    pub routes: Vec<crate::builtins::RouteProfile>,
    /// Per-key policy: spend caps and rate limits. Applied to the BudgetGuard
    /// at startup. The `key` values must also be present in STOKE_API_KEYS.
    #[serde(default)]
    pub keys: Vec<KeyPolicy>,
    /// Role → model assignments for `routing = "auto"`. Anything not set here
    /// is resolved from models discovered on your own nodes; Stoke itself
    /// ships with no model names.
    #[serde(default)]
    pub auto_route: AutoRouteConfig,
    /// Model prices. Stoke ships none — an unpriced model on a metered provider
    /// is refused, because spend it cannot measure is spend `budget_usd` cannot cap.
    #[serde(default)]
    pub pricing: PricingConfig,
    /// Ceilings on how much work one inbound request may fan out into.
    #[serde(default)]
    pub limits: LimitsConfig,
    /// Subscription fallback: what a `claude_subscription` provider may do when
    /// the flat plan refuses traffic. Off by default.
    #[serde(default)]
    pub subscription_fallback: SubscriptionFallbackConfig,
}

/// Operator-declared model prices.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PricingConfig {
    /// `[pricing.models."<model>"] input_per_1m = .., output_per_1m = ..`
    #[serde(default)]
    pub models: HashMap<String, crate::cost::ModelPricing>,
    /// `"refuse"` (default) or `"free"`: what to do when a metered provider is
    /// asked for a model with no declared price.
    #[serde(default = "default_unpriced")]
    pub unpriced: String,
}

fn default_unpriced() -> String {
    "refuse".to_string()
}

/// What a `claude_subscription` provider may do when the flat plan refuses
/// traffic (usage limit / rate limit). Stoke ships no model names here: the
/// fallback is described by provider NAME and family, resolved against the
/// live provider list at request time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubscriptionFallbackConfig {
    /// Off by default — a subscription serving its own plan never silently
    /// substitutes another backend.
    #[serde(default)]
    pub enabled: bool,
    /// Codex-family provider NAME to try when Anthropic returns 429. Empty
    /// means the first `codex_subscription` provider, and only when that
    /// provider has no explicit `models` list (operator opted into discovery).
    #[serde(default)]
    pub codex_provider: Option<String>,
    /// Whether local openai_compatible providers (tier local|remote) may serve
    /// as the last resort. Off by default.
    #[serde(default)]
    pub allow_local: bool,
}

impl SubscriptionFallbackConfig {
    pub fn is_default(&self) -> bool {
        !self.enabled && self.codex_provider.is_none() && !self.allow_local
    }
}

/// One inbound HTTP request can become many billed provider calls. `routing`,
/// `vote_models` and `n_samples` arrive in the request body, so without these
/// ceilings the caller — not the operator — decides how much money a request
/// may spend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Max samples for `self_consistency`, regardless of what the caller asks for.
    #[serde(default = "default_max_n_samples")]
    pub max_n_samples: usize,
    /// Max models in a voting/cascade list.
    #[serde(default = "default_max_vote_models")]
    pub max_vote_models: usize,
    /// Whether a caller may select a multi-call routing pattern in the request
    /// body. Off by default: fan-out is an operator decision, expressed as a
    /// `[[routes]]` profile or the top-level `routing` setting.
    #[serde(default)]
    pub allow_caller_routing: bool,
    /// How many output tokens to assume when a request sets no `max_tokens`, for
    /// the purpose of holding money against the cap while the request runs. Too
    /// low and a burst of concurrent streams can still overshoot; too high and
    /// requests that would have fit are refused.
    #[serde(default = "default_assumed_max_output_tokens")]
    pub assumed_max_output_tokens: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_n_samples: default_max_n_samples(),
            max_vote_models: default_max_vote_models(),
            allow_caller_routing: false,
            assumed_max_output_tokens: default_assumed_max_output_tokens(),
        }
    }
}

fn default_max_n_samples() -> usize {
    5
}
fn default_max_vote_models() -> usize {
    5
}
fn default_assumed_max_output_tokens() -> u64 {
    2048
}

/// Routing patterns that issue more than one provider call per request.
pub fn is_fanout_routing(routing: &str) -> bool {
    matches!(
        routing,
        "parallel_vote"
            | "self_consistency"
            | "deliberation"
            | "test_vote"
            | "cascade"
            | "cascade_test"
            | "stream_race"
    )
}

/// A role accepts one model or an ordered candidate list. With a list, the
/// FIRST entry is the preference (quality floor); later entries are explicitly
/// acceptable alternates the optimizer may pick for cost/latency.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RoleModels {
    One(String),
    Many(Vec<String>),
}

impl RoleModels {
    pub fn as_vec(&self) -> Vec<String> {
        match self {
            RoleModels::One(s) => vec![s.clone()],
            RoleModels::Many(v) => v.clone(),
        }
    }
}

fn role_vec(r: &Option<RoleModels>) -> Vec<String> {
    r.as_ref().map(|m| m.as_vec()).unwrap_or_default()
}

/// Explicit model choices for auto-routing roles. All optional.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AutoRouteConfig {
    /// Everyday default (chat, fallback for unresolved roles).
    #[serde(default)]
    pub fast: Option<RoleModels>,
    /// Code generation. For agent harnesses these should be models that
    /// emit structured tool_calls.
    #[serde(default)]
    pub coder: Option<RoleModels>,
    /// Step-by-step reasoning.
    #[serde(default)]
    pub reasoner: Option<RoleModels>,
    /// Oversized prompts.
    #[serde(default)]
    pub long_context: Option<RoleModels>,
    /// Quality-mode target (typically paid/cloud). Config-only — never guessed.
    /// The FIRST entry is also the counterfactual price for the savings receipts.
    #[serde(default)]
    pub quality: Option<RoleModels>,
    /// Candidates for validated (test-verified) patterns.
    #[serde(default)]
    pub vote_models: Vec<String>,
    /// Hedged dispatch: race small prompts across idle zero-marginal nodes,
    /// first token wins. Costs duplicate local compute; off by default.
    #[serde(default)]
    pub hedge: bool,
}

impl AutoRouteConfig {
    pub fn fast_vec(&self) -> Vec<String> {
        role_vec(&self.fast)
    }
    pub fn coder_vec(&self) -> Vec<String> {
        role_vec(&self.coder)
    }
    pub fn reasoner_vec(&self) -> Vec<String> {
        role_vec(&self.reasoner)
    }
    pub fn long_context_vec(&self) -> Vec<String> {
        role_vec(&self.long_context)
    }
    pub fn quality_vec(&self) -> Vec<String> {
        role_vec(&self.quality)
    }
}

/// Per-API-key enforcement policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPolicy {
    /// The API key this policy applies to (must be a configured auth key).
    pub key: String,
    /// Hard spend cap in USD. Requests are refused once cumulative spend
    /// reaches this. Omitted or 0.0 = unlimited.
    #[serde(default)]
    pub budget_usd: f64,
    /// Max requests per rolling 60s window. Omitted or 0 = unlimited.
    #[serde(default)]
    pub rate_limit_rpm: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8787
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    #[serde(default = "openai_compatible")]
    pub r#type: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub models: Vec<String>,
    /// Tier: "local" (this machine), "remote" (another machine on LAN), "cloud"
    #[serde(default)]
    pub tier: String,
}

fn openai_compatible() -> String {
    "openai_compatible".to_string()
}

impl ProviderConfig {
    /// A ChatGPT/Codex subscription passthrough provider. Its credential is
    /// the client's own ChatGPT OAuth token carried in `Authorization` — never
    /// a configured API key — so the whole api_key machinery is inapplicable.
    pub fn is_subscription(&self) -> bool {
        self.r#type == "codex_subscription"
    }

    /// Resolve the API key — direct value or from env var.
    pub fn resolve_api_key(&self) -> String {
        if !self.api_key.is_empty() {
            return self.api_key.clone();
        }
        if !self.api_key_env.is_empty() {
            return env::var(&self.api_key_env).unwrap_or_default();
        }
        String::new()
    }
}

impl Config {
    /// Load config from file, searching default locations.
    pub fn load() -> Result<Self, String> {
        let path = Self::find_config_path()?;
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))?;

        // Warn about plaintext API keys in config
        for p in &config.providers {
            if !p.api_key.is_empty() && p.api_key != "ollama-local" && !p.is_subscription() {
                eprintln!(
                    "⚠ Security: provider '{}' has plaintext api_key in {}. \
                     Use api_key_env instead (e.g. api_key_env = \"OPENAI_API_KEY\").",
                    p.name,
                    path.display()
                );
            }
        }

        config.validate()?;
        Ok(config)
    }

    /// Reject configurations whose enforcement would be silently absent.
    ///
    /// A provider with no `tier` is the dangerous case: `tier_rank` treats it as
    /// local for routing, which would exempt a cloud endpoint from pricing and
    /// let it serve unmetered traffic under a `budget_usd` cap that never trips.
    /// Fail at boot rather than at spend time.
    pub fn validate(&self) -> Result<(), String> {
        for p in &self.providers {
            if !p.is_subscription() {
                continue;
            }
            // The subscription credential is the client's own ChatGPT OAuth
            // token; a configured key here would be a second secret Stoke has
            // to guard, and a wrong one would silently break auth upstream.
            if !p.api_key.is_empty() || !p.api_key_env.is_empty() {
                return Err(format!(
                    "provider '{}' is a codex_subscription passthrough: it must not set \
                     api_key or api_key_env — the client's own ChatGPT OAuth token is the credential",
                    p.name
                ));
            }
            if p.tier != "subscription" {
                return Err(format!(
                    "provider '{}' is a codex_subscription passthrough and must declare \
                     tier = \"subscription\" (found \"{}\") so it is never metered as API dollar spend",
                    p.name, p.tier
                ));
            }
            // Exact-base check first: only the exact ChatGPT Codex backend is
            // a legal subscription target, so a lookalike path or host is
            // refused at boot.
            if let Err(reason) = crate::subscription::subscription_responses_endpoint(&p.base_url) {
                return Err(format!(
                    "provider '{}' (codex_subscription) base_url \"{}\" is refused — {reason}",
                    p.name, p.base_url
                ));
            }
            if let Err(reason) = crate::subscription::validate_oauth_destination(
                &format!("{}/responses", p.base_url.trim_end_matches('/')),
                "chatgpt.com",
            ) {
                return Err(format!(
                    "provider '{}' (codex_subscription) base_url \"{}\" may only target the \
                     exact ChatGPT backend {} — {reason}",
                    p.name,
                    p.base_url,
                    crate::subscription::CHATGPT_CODEX_BASE
                ));
            }
        }
        if crate::cost::Unpriced::parse(&self.pricing.unpriced) == crate::cost::Unpriced::Refuse {
            let untiered: Vec<&str> = self
                .providers
                .iter()
                .filter(|p| p.tier.trim().is_empty())
                .map(|p| p.name.as_str())
                .collect();
            if !untiered.is_empty() {
                return Err(format!(
                    "provider(s) {} have no `tier`. Set tier = \"local\" | \"remote\" | \"cloud\" \
                     so Stoke knows whether their traffic must be priced. \
                     (Or set `[pricing] unpriced = \"free\"` to disable metering entirely.)",
                    untiered.join(", ")
                ));
            }
        }
        if self.limits.max_n_samples == 0 || self.limits.max_vote_models == 0 {
            return Err("[limits] max_n_samples and max_vote_models must be >= 1".to_string());
        }
        for route in &self.routes {
            for tier in &route.allowed_tiers {
                if !matches!(tier.as_str(), "local" | "remote" | "cloud") {
                    return Err(format!(
                        "route '{}' has unsupported allowed_tiers value '{}'; use local, remote, or cloud",
                        route.name, tier
                    ));
                }
            }
        }
        for p in &self.providers {
            if p.r#type != "claude_subscription" {
                continue;
            }
            // A subscription provider's credential is the OAuth token store in
            // crate::anthropic_oauth, so the pinned rules are: the exact
            // first-party API host, a declared tier, and no static key that
            // could silently override the OAuth flow.
            if let Err(reason) = crate::subscription::validate_oauth_destination(
                &p.base_url,
                crate::anthropic_oauth::ANTHROPIC_API_HOST,
            ) {
                return Err(format!(
                    "provider '{}' (type claude_subscription): base_url must be exactly \
                     https://api.anthropic.com — {reason}",
                    p.name
                ));
            }
            if p.tier.trim().is_empty() {
                return Err(format!(
                    "provider '{}' (type claude_subscription) has no `tier`. \
                     Set tier = \"subscription\" so Stoke knows this traffic is subscription-metered.",
                    p.name
                ));
            }
            if !p.api_key.is_empty() || !p.api_key_env.is_empty() {
                return Err(format!(
                    "provider '{}' (type claude_subscription) must not declare api_key or \
                     api_key_env — its credential is the Anthropic OAuth token store \
                     (see `stoke` OAuth login), not a static key",
                    p.name
                ));
            }
        }
        Ok(())
    }

    /// Build the pricer this config describes.
    pub fn pricer(&self) -> crate::cost::Pricer {
        crate::cost::Pricer::new(
            self.pricing.models.clone(),
            crate::cost::Unpriced::parse(&self.pricing.unpriced),
        )
    }

    fn find_config_path() -> Result<PathBuf, String> {
        // Search order: CLI arg (TODO), ./stoke.toml, ~/.config/stoke/stoke.toml
        let candidates = [
            PathBuf::from("stoke.toml"),
            dirs::config_dir()
                .unwrap_or_default()
                .join("stoke/stoke.toml"),
        ];
        for c in &candidates {
            if c.exists() {
                return Ok(c.clone());
            }
        }
        Err(format!(
            "No config found. Looked in: {}",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Find the provider that serves the given model.
    /// If no provider lists specific models, use the first one (Ollama auto-discovers).
    pub fn provider_for_model(&self, model: &str) -> Option<&ProviderConfig> {
        // If a provider explicitly lists models, check if model is in that list
        for p in &self.providers {
            if !p.models.is_empty() && p.models.iter().any(|m| m == model) {
                return Some(p);
            }
        }
        // Fallback: first provider (typically local Ollama which serves everything)
        self.providers.first()
    }

    /// Get providers filtered by tier
    pub fn providers_by_tier(&self, tier: &str) -> Vec<&ProviderConfig> {
        self.providers.iter().filter(|p| p.tier == tier).collect()
    }

    /// Check if any provider in a tier is available (health check)
    /// Uses the shared HTTP client for connection pooling.
    pub async fn tier_available(tier_providers: &[&ProviderConfig]) -> bool {
        for p in tier_providers {
            let url = format!("{}/models", p.base_url.trim_end_matches('/'));
            let req = (&*crate::router::SHARED_CLIENT).get(&url);
            let req = if !p.resolve_api_key().is_empty() {
                req.bearer_auth(&p.resolve_api_key())
            } else {
                req
            };
            if req
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                return true;
            }
        }
        false
    }

    /// Get all known models across all providers.
    pub fn all_models(&self) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        for p in &self.providers {
            if p.models.is_empty() {
                // Auto-discover via API at startup (handled in router)
            } else {
                models.extend(p.models.clone());
            }
        }
        models
    }
}

// Minimal dirs replacement to avoid extra dep
mod dirs {
    use std::path::PathBuf;
    pub fn config_dir() -> Option<PathBuf> {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })
    }
}
#[cfg(test)]
mod validate_tests {
    use super::*;

    fn cfg(toml_src: &str) -> Config {
        toml::from_str(toml_src).expect("test config must parse")
    }

    const BASE: &str = r#"
[server]
host = "127.0.0.1"
port = 8787
"#;

    #[test]
    fn a_provider_without_a_tier_is_rejected_at_boot() {
        // tier_rank() treats "" as local, which would exempt a cloud endpoint
        // from pricing. Catch it at load, not at spend time.
        let c = cfg(&format!(
            "{BASE}\n[[providers]]\nname = \"mystery\"\nbase_url = \"https://example.com/v1\"\n"
        ));
        let err = c.validate().unwrap_err();
        assert!(
            err.contains("mystery"),
            "the error must name the provider: {err}"
        );
        assert!(err.contains("tier"));
    }

    #[test]
    fn a_declared_tier_passes() {
        let c = cfg(&format!(
            "{BASE}\n[[providers]]\nname = \"ollama\"\nbase_url = \"http://x/v1\"\ntier = \"local\"\n"
        ));
        assert!(c.validate().is_ok());
    }

    #[test]
    fn opting_out_of_metering_also_opts_out_of_the_tier_check() {
        let c = cfg(&format!(
            "{BASE}\n[pricing]\nunpriced = \"free\"\n\
             [[providers]]\nname = \"mystery\"\nbase_url = \"https://example.com/v1\"\n"
        ));
        assert!(
            c.validate().is_ok(),
            "unpriced=free means tiers no longer gate spend"
        );
    }

    #[test]
    fn an_unknown_route_tier_is_rejected_at_boot() {
        let c = cfg(&format!(
            "{BASE}\n[[routes]]\nname = \"private\"\npath = \"/v1/private/completions\"\nrouting = \"single\"\nallowed_tiers = [\"private\"]\n"
        ));
        let err = c.validate().unwrap_err();
        assert!(err.contains("private"));
        assert!(err.contains("allowed_tiers"));
    }

    #[test]
    fn limits_default_to_a_bounded_fan_out() {
        let c = cfg(BASE);
        assert_eq!(c.limits.max_n_samples, 5);
        assert_eq!(c.limits.max_vote_models, 5);
        assert!(
            !c.limits.allow_caller_routing,
            "callers must not pick fan-out by default"
        );
    }

    #[test]
    fn a_zero_limit_is_rejected() {
        let c = cfg(&format!("{BASE}\n[limits]\nmax_n_samples = 0\n"));
        assert!(c.validate().is_err());
    }

    #[test]
    fn a_codex_subscription_provider_is_valid_without_an_api_key() {
        let c = cfg(&format!(
            "{BASE}\n[[providers]]\nname = \"chatgpt\"\ntype = \"codex_subscription\"\n\
             base_url = \"https://chatgpt.com/backend-api/codex\"\ntier = \"subscription\"\n"
        ));
        assert!(
            c.validate().is_ok(),
            "no api_key is required for a subscription"
        );
        let p = &c.providers[0];
        assert!(p.is_subscription());
        assert!(p.resolve_api_key().is_empty());
    }

    #[test]
    fn a_codex_subscription_provider_must_target_the_exact_chatgpt_backend() {
        for base in [
            "https://api.openai.com/v1",
            "https://chatgpt.com/backend-api/codex.evil.test",
            "https://chatgpt.com.evil.test/backend-api/codex",
            "http://chatgpt.com/backend-api/codex",
        ] {
            let c = cfg(&format!(
                "{BASE}\n[[providers]]\nname = \"chatgpt\"\ntype = \"codex_subscription\"\n\
                 base_url = \"{base}\"\ntier = \"subscription\"\n"
            ));
            let err = c.validate().unwrap_err();
            assert!(err.contains(base), "must refuse {base}: {err}");
        }
    }

    #[test]
    fn a_codex_subscription_provider_must_declare_the_subscription_tier() {
        let c = cfg(&format!(
            "{BASE}\n[[providers]]\nname = \"chatgpt\"\ntype = \"codex_subscription\"\n\
             base_url = \"https://chatgpt.com/backend-api/codex\"\ntier = \"cloud\"\n"
        ));
        let err = c.validate().unwrap_err();
        assert!(
            err.contains("subscription"),
            "must demand tier=subscription: {err}"
        );
    }

    #[test]
    fn a_codex_subscription_provider_must_not_ship_a_plaintext_api_key() {
        let c = cfg(&format!(
            "{BASE}\n[[providers]]\nname = \"chatgpt\"\ntype = \"codex_subscription\"\n\
             base_url = \"https://chatgpt.com/backend-api/codex\"\ntier = \"subscription\"\n\
             api_key = \"some-secret\"\n"
        ));
        let err = c.validate().unwrap_err();
        assert!(
            err.contains("api_key"),
            "subscription auth is the user's own ChatGPT OAuth, not a config key: {err}"
        );
    }

    #[test]
    fn a_regular_provider_is_never_treated_as_subscription() {
        let c = cfg(&format!(
            "{BASE}\n[[providers]]\nname = \"ollama\"\nbase_url = \"http://127.0.0.1:11434/v1\"\n\
             tier = \"local\"\n[[providers]]\nname = \"openai\"\ntype = \"openai\"\n\
             base_url = \"https://api.openai.com/v1\"\ntier = \"cloud\"\n"
        ));
        for p in &c.providers {
            assert!(!p.is_subscription(), "{} must not be subscription", p.name);
        }
    }

    #[test]
    fn prices_load_from_config() {
        let c = cfg(&format!(
            "{BASE}\n[pricing.models.\"fixture-model\"]\ninput_per_1m = 3.0\noutput_per_1m = 15.0\n"
        ));
        let p = c.pricer();
        assert!(p.is_priced("fixture-model"));
        assert!(!p.is_priced("something-else"));
        assert!(p.allows("cloud", "fixture-model").is_ok());
        assert!(p.allows("cloud", "something-else").is_err());
    }

    #[test]
    fn auto_is_not_itself_a_fanout_but_can_resolve_into_one() {
        // `auto` names no fan-out, so the *pre*-resolution check waves it through.
        // That is why enforcement must run on the routing the auto-router returns
        // (auto_route::decide can answer "cascade_test"), not on what was asked for.
        assert!(!is_fanout_routing("auto"));
        assert!(
            is_fanout_routing("cascade_test"),
            "the pattern auto can resolve into"
        );
    }

    #[test]
    fn every_fan_out_pattern_is_recognised() {
        // If a pattern dispatches multiple provider calls but is missing here, a
        // caller can select it in the request body and multiply their own budget.
        for p in [
            "parallel_vote",
            "self_consistency",
            "deliberation",
            "test_vote",
            "cascade",
            "cascade_test",
            "stream_race",
        ] {
            assert!(is_fanout_routing(p), "{p} fans out but is not gated");
        }
        assert!(!is_fanout_routing("single"));
        assert!(!is_fanout_routing("auto"));
    }

    const CLAUDE_SUB: &str = r#"
[[providers]]
name = "claude-sub"
type = "claude_subscription"
base_url = "https://api.anthropic.com"
tier = "subscription"
"#;

    #[test]
    fn claude_subscription_provider_with_exact_host_passes_boot() {
        let c = cfg(&format!("{BASE}{CLAUDE_SUB}"));
        assert!(c.validate().is_ok());
        // The provider must exist as configured (slice B will consume it).
        let p = c.providers.first().expect("provider present");
        assert_eq!(p.r#type, "claude_subscription");
        assert_eq!(p.tier, "subscription");
        assert_eq!(p.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn claude_subscription_base_url_must_be_exactly_api_anthropic_com() {
        for hostile in [
            "https://api.anthropic.com.evil.test",
            "https://api.anthropic.com.evil.test/v1",
            "https://anthropic.com",
            "http://api.anthropic.com",
            "https://api.anthropic.com:444",
            "https://user@api.anthropic.com",
            "https://example.com",
        ] {
            let c = cfg(&format!(
                "{BASE}\n[[providers]]\nname = \"claude-sub\"\ntype = \"claude_subscription\"\n\
                 base_url = \"{hostile}\"\ntier = \"subscription\"\n"
            ));
            let err = c.validate().unwrap_err();
            assert!(
                err.contains("claude-sub"),
                "error must name the provider: {err}"
            );
            assert!(
                err.contains("api.anthropic.com"),
                "error must name the required base_url: {err}"
            );
        }
    }

    #[test]
    fn claude_subscription_tier_is_required() {
        // tier_rank() treats "" as local, which would exempt api.anthropic.com
        // from pricing metering — the exact failure this type must not allow.
        let c = cfg(&format!(
            "{BASE}\n[[providers]]\nname = \"claude-sub\"\ntype = \"claude_subscription\"\n\
             base_url = \"https://api.anthropic.com\"\n"
        ));
        let err = c.validate().unwrap_err();
        assert!(err.contains("claude-sub"));
        assert!(err.contains("tier"));
    }

    #[test]
    fn claude_subscription_must_not_declare_an_api_key() {
        // A subscription provider's credential is the OAuth token store; a
        // static api_key beside it would silently override the OAuth flow.
        for key_field in [
            "api_key = \"sk-ant-x\"",
            "api_key_env = \"ANTHROPIC_API_KEY\"",
        ] {
            let c = cfg(&format!(
                "{BASE}\n[[providers]]\nname = \"claude-sub\"\ntype = \"claude_subscription\"\n\
                 base_url = \"https://api.anthropic.com\"\ntier = \"subscription\"\n{key_field}\n"
            ));
            let err = c.validate().unwrap_err();
            assert!(
                err.contains("claude-sub"),
                "error must name the provider: {err}"
            );
            assert!(
                err.contains("api_key"),
                "error must explain that subscription providers take no api_key: {err}"
            );
        }
    }

    #[test]
    fn other_provider_types_are_not_subject_to_claude_subscription_rules() {
        // The anthropic type with an explicit api_key must keep working.
        let c = cfg(&format!(
            "{BASE}\n[[providers]]\nname = \"anthropic-key\"\ntype = \"anthropic\"\n\
             base_url = \"https://api.anthropic.com\"\ntier = \"cloud\"\napi_key_env = \"ANTHROPIC_API_KEY\"\n"
        ));
        assert!(c.validate().is_ok());
    }
}
