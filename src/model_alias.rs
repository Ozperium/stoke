const CODEX_PREFIX: &str = "claude-stoke-codex--";
const LOCAL_PREFIX: &str = "claude-stoke-local--";
const HERMES_CODEX_PREFIX: &str = "stoke-codex--";
const HERMES_LOCAL_PREFIX: &str = "stoke-local--";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasTarget {
    /// provider: None = the sole configured codex provider; Some(name) = that
    /// exact provider. A qualified alias must never resolve to a different
    /// provider, and an unqualified one must never guess between two.
    Codex { provider: Option<String>, model: String },
    Local { provider: String, model: String },
}

pub fn codex_alias(model: &str) -> String {
    format!("{CODEX_PREFIX}{model}")
}

/// Provider-qualified codex alias: minted whenever more than one
/// codex_subscription provider is configured, so an alias advertised for
/// provider B can never dispatch to provider A.
pub fn codex_alias_qualified(provider: &str, model: &str) -> String {
    format!("{CODEX_PREFIX}{provider}--{model}")
}

pub fn local_alias(provider: &str, model: &str) -> String {
    format!("{LOCAL_PREFIX}{provider}--{model}")
}

pub fn parse_alias(alias: &str) -> Option<AliasTarget> {
    let codex_rest = alias
        .strip_prefix(CODEX_PREFIX)
        .or_else(|| alias.strip_prefix(HERMES_CODEX_PREFIX))
        .filter(|rest| !rest.is_empty());
    if let Some(rest) = codex_rest {
        // "provider--model" (qualified) or plain "model" (unqualified).
        // A provider NAME cannot contain "--" (config identifiers are plain),
        // so the first "--" splits the pair; its absence means unqualified.
        return Some(match rest.split_once("--") {
            Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
                AliasTarget::Codex {
                    provider: Some(provider.to_string()),
                    model: model.to_string(),
                }
            }
            _ => AliasTarget::Codex {
                provider: None,
                model: rest.to_string(),
            },
        });
    }
    let rest = alias
        .strip_prefix(LOCAL_PREFIX)
        .or_else(|| alias.strip_prefix(HERMES_LOCAL_PREFIX))?;
    let (provider, model) = rest.split_once("--")?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some(AliasTarget::Local {
        provider: provider.to_string(),
        model: model.to_string(),
    })
}

/// Providers a `claude_subscription` request may fall back to when the flat
/// plan refuses traffic (usage limit / rate limit), in priority order. Zero
/// model names: the operator opts in by provider NAME and tier family, and the
/// serving model is whichever the fallback provider discovers or configures.
///
/// Priority: the named codex provider (or the sole discovery-only
/// codex_subscription provider), then local translation providers when
/// `allow_local` is set. A provider that pins an explicit `models` list is
/// excluded from codex fallback — its operator listed specific models for
/// native `/v1/responses` traffic, which is a different contract.
pub fn fallback_providers<'a>(
    providers: &'a [crate::config::ProviderConfig],
    codex_provider: Option<&str>,
    allow_local: bool,
) -> Vec<&'a crate::config::ProviderConfig> {
    let mut out = Vec::new();
    let codex = match codex_provider {
        Some(name) => providers
            .iter()
            .find(|p| Some(&p.name[..]) == Some(name) && p.r#type == "codex_subscription"),
        None => providers
            .iter()
            .find(|p| p.r#type == "codex_subscription" && p.models.is_empty()),
    };
    if let Some(codex) = codex {
        out.push(codex);
    }
    if allow_local {
        // `allow_local` means local hardware the operator owns: a `remote`
        // tier is another machine that may be billed per token, so it must
        // not silently inherit zero-marginal fallback treatment. Cloud is
        // already excluded from fallback candidates.
        out.extend(
            providers
                .iter()
                .filter(|p| p.r#type == "openai_compatible" && p.tier == "local"),
        );
    }
    out
}

pub fn aliases_for_provider(
    provider_name: &str,
    provider_type: &str,
    tier: &str,
    models: &[String],
    codex_provider_count: usize,
) -> Vec<String> {
    // Only advertise aliases /v1/messages can actually route. Local aliases
    // ride the Messages->chat-completions translation, which accepts exactly
    // openai_compatible providers on tier local|remote; a placeholder
    // "provider:*" wildcard from a degraded discovery never routes upstream.
    let wildcard = |model: &str| model.ends_with(":*") || model == "*";
    match (provider_type, tier) {
        ("codex_subscription", _) => {
            let qualified = codex_provider_count > 1;
            models
                .iter()
                .filter(|model| !wildcard(model))
                .map(|model| {
                    if qualified {
                        // Two+ codex providers: the bare alias is ambiguous
                        // (the unqualified form fails closed), so the catalog
                        // mints provider-qualified aliases only.
                        codex_alias_qualified(provider_name, model)
                    } else {
                        codex_alias(model)
                    }
                })
                .collect()
        }
        ("openai_compatible", "local" | "remote") => models
            .iter()
            .filter(|model| !wildcard(model))
            .map(|model| local_alias(provider_name, model))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_aliases_round_trip_without_changing_the_real_model_id() {
        let codex = codex_alias("gpt-5.6-sol");
        assert_eq!(codex, "claude-stoke-codex--gpt-5.6-sol");
        assert_eq!(
            parse_alias(&codex),
            Some(AliasTarget::Codex {
                provider: None,
                model: "gpt-5.6-sol".into(),
            })
        );
        assert_eq!(
            parse_alias("stoke-codex--gpt-5.6-sol"),
            Some(AliasTarget::Codex {
                provider: None,
                model: "gpt-5.6-sol".into(),
            })
        );

        let local = local_alias("ollama", "ornith:9b");
        assert_eq!(local, "claude-stoke-local--ollama--ornith:9b");
        assert_eq!(
            parse_alias(&local),
            Some(AliasTarget::Local {
                provider: "ollama".into(),
                model: "ornith:9b".into(),
            })
        );
        assert_eq!(
            parse_alias("stoke-local--ollama--ornith:9b"),
            Some(AliasTarget::Local {
                provider: "ollama".into(),
                model: "ornith:9b".into(),
            })
        );
    }

    #[test]
    fn catalog_aliases_are_only_created_for_codex_and_local_translation_providers() {
        let models = vec!["first:model".to_string(), "second.model".to_string()];
        assert_eq!(
            aliases_for_provider("chatgpt", "codex_subscription", "subscription", &models, 1),
            vec![
                "claude-stoke-codex--first:model",
                "claude-stoke-codex--second.model"
            ]
        );
        assert_eq!(
            aliases_for_provider("ollama", "openai_compatible", "local", &models, 1),
            vec![
                "claude-stoke-local--ollama--first:model",
                "claude-stoke-local--ollama--second.model"
            ]
        );
        assert!(
            aliases_for_provider("anthropic", "claude_subscription", "subscription", &models, 1)
                .is_empty()
        );
    }

    #[test]
    fn qualified_codex_aliases_are_minted_when_two_providers_are_configured() {
        // Two codex_subscription providers: a bare `claude-stoke-codex--<model>`
        // cannot say which account serves it, so the catalog mints the
        // provider-qualified form and never the ambiguous short one.
        let models = vec!["gpt-5.6-sol".to_string()];
        assert_eq!(
            aliases_for_provider("work", "codex_subscription", "subscription", &models, 2),
            vec!["claude-stoke-codex--work--gpt-5.6-sol"]
        );
        assert_eq!(
            aliases_for_provider("personal", "codex_subscription", "subscription", &models, 2),
            vec!["claude-stoke-codex--personal--gpt-5.6-sol"]
        );
    }

    #[test]
    fn qualified_codex_alias_parses_to_its_named_provider_never_another() {
        let alias = codex_alias_qualified("personal", "gpt-5.6-sol");
        assert_eq!(alias, "claude-stoke-codex--personal--gpt-5.6-sol");
        assert_eq!(
            parse_alias(&alias),
            Some(AliasTarget::Codex {
                provider: Some("personal".into()),
                model: "gpt-5.6-sol".into(),
            })
        );
        // The bare form still parses as unqualified (sole-provider case).
        assert_eq!(
            parse_alias("claude-stoke-codex--gpt-5.6-sol"),
            Some(AliasTarget::Codex {
                provider: None,
                model: "gpt-5.6-sol".into(),
            })
        );
    }

    #[test]
    fn local_aliases_are_emitted_only_for_tiers_the_messages_bridge_routes() {
        let models = vec!["some/model".to_string()];
        // /v1/messages only translates local|remote openai_compatible providers,
        // so cloud-tier providers must not advertise routable-looking aliases.
        assert!(
            aliases_for_provider("openrouter", "openai_compatible", "cloud", &models, 1).is_empty()
        );
        assert_eq!(
            aliases_for_provider("openrouter", "openai_compatible", "remote", &models, 1),
            vec!["claude-stoke-local--openrouter--some/model"]
        );
        assert_eq!(
            aliases_for_provider("ollama", "openai_compatible", "local", &models, 1),
            vec!["claude-stoke-local--ollama--some/model"]
        );
        // Codex aliases are type-gated, not tier-gated.
        assert_eq!(
            aliases_for_provider("chatgpt", "codex_subscription", "subscription", &models, 1),
            vec!["claude-stoke-codex--some/model"]
        );
    }

    #[test]
    fn discovery_placeholder_wildcards_never_become_aliases() {
        let placeholder = vec!["ollama:*".to_string()];
        assert!(
            aliases_for_provider("ollama", "openai_compatible", "local", &placeholder, 1).is_empty()
        );
        let codex_placeholder = vec!["chatgpt:*".to_string()];
        assert!(aliases_for_provider(
            "chatgpt",
            "codex_subscription",
            "subscription",
            &codex_placeholder,
            1
        )
        .is_empty());
        // A real model id that merely contains a colon still aliases.
        let real = vec!["llama3.2:3b".to_string()];
        assert_eq!(
            aliases_for_provider("ollama", "openai_compatible", "local", &real, 1),
            vec!["claude-stoke-local--ollama--llama3.2:3b"]
        );
    }

    #[test]
    fn ordinary_claude_ids_are_never_treated_as_aliases() {
        assert_eq!(parse_alias("claude-sonnet-5"), None);
        assert_eq!(parse_alias("claude-stoke-local--missing-model"), None);
        assert_eq!(parse_alias("claude-stoke-codex--"), None);
    }

    fn provider(
        name: &str,
        kind: &str,
        tier: &str,
        models: &[&str],
    ) -> crate::config::ProviderConfig {
        crate::config::ProviderConfig {
            name: name.to_string(),
            r#type: kind.to_string(),
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: String::new(),
            api_key_env: String::new(),
            models: models.iter().map(|m| m.to_string()).collect(),
            tier: tier.to_string(),
        }
    }

    #[test]
    fn fallback_prioritizes_codex_then_local_only_when_enabled() {
        let providers = vec![
            provider(
                "claude-subscription",
                "claude_subscription",
                "subscription",
                &[],
            ),
            provider(
                "codex-subscription",
                "codex_subscription",
                "subscription",
                &["gpt-5.3-codex"],
            ),
            provider("ollama", "openai_compatible", "local", &["llama3.2:3b"]),
            provider("cloud-box", "openai_compatible", "cloud", &["some/model"]),
        ];
        // Default (disabled knobs): nothing may fall back.
        assert!(fallback_providers(&providers, None, false).is_empty());
        // Codex fallback only: the pinned codex provider is excluded because
        // its operator listed explicit models for native Responses traffic.
        assert!(fallback_providers(&providers, None, false).is_empty());
        let named = fallback_providers(&providers, Some("codex-subscription"), false);
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, "codex-subscription");
        // allow_local adds local|remote translation providers, never cloud.
        let both = fallback_providers(&providers, Some("codex-subscription"), true);
        let names: Vec<&str> = both.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["codex-subscription", "ollama"]);
    }

    #[test]
    fn allow_local_fallback_admits_only_tier_local_not_remote() {
        // `allow_local` means local hardware: a `remote` tier is someone else's
        // box billed per token, so it must never inherit the zero-marginal
        // fallback treatment as an arbitrary endpoint.
        let providers = vec![
            provider("ollama", "openai_compatible", "local", &["llama3.2:3b"]),
            provider("lan-proxy", "openai_compatible", "remote", &["some/model"]),
            provider("cloud-box", "openai_compatible", "cloud", &["some/model"]),
        ];
        let picked = fallback_providers(&providers, None, true);
        let names: Vec<&str> = picked.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["ollama"]);
    }

    #[test]
    fn unnamed_codex_fallback_requires_discovery_only_provider() {
        let providers = vec![
            provider(
                "claude-subscription",
                "claude_subscription",
                "subscription",
                &[],
            ),
            provider(
                "codex-subscription",
                "codex_subscription",
                "subscription",
                &["gpt-5.3-codex"],
            ),
        ];
        // No explicit name: a codex provider that pins models is NOT a fallback
        // candidate; the operator must name it to override that.
        assert!(fallback_providers(&providers, None, false).is_empty());
        let discovery_only = vec![
            provider(
                "claude-subscription",
                "claude_subscription",
                "subscription",
                &[],
            ),
            provider(
                "codex-subscription",
                "codex_subscription",
                "subscription",
                &[],
            ),
        ];
        let resolved = fallback_providers(&discovery_only, None, false);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "codex-subscription");
    }
}
