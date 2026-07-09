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
    pub fn fast_vec(&self) -> Vec<String> { role_vec(&self.fast) }
    pub fn coder_vec(&self) -> Vec<String> { role_vec(&self.coder) }
    pub fn reasoner_vec(&self) -> Vec<String> { role_vec(&self.reasoner) }
    pub fn long_context_vec(&self) -> Vec<String> { role_vec(&self.long_context) }
    pub fn quality_vec(&self) -> Vec<String> { role_vec(&self.quality) }
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
            if !p.api_key.is_empty() && p.api_key != "ollama-local" {
                eprintln!(
                    "⚠ Security: provider '{}' has plaintext api_key in {}. \
                     Use api_key_env instead (e.g. api_key_env = \"OPENAI_API_KEY\").",
                    p.name,
                    path.display()
                );
            }
        }

        Ok(config)
    }

    fn find_config_path() -> Result<PathBuf, String> {
        // Search order: CLI arg (TODO), ./stoke.toml, ~/.config/stoke/stoke.toml
        let candidates = [
            PathBuf::from("stoke.toml"),
            dirs::config_dir().unwrap_or_default().join("stoke/stoke.toml"),
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
            if req.send().await.map(|r| r.status().is_success()).unwrap_or(false) {
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
            .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config")))
    }
}