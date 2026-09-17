//! Built-in plugins — prompt harness, PII redaction, code formatter, audit logger.
//!
//! These run in-process (no HTTP webhook needed). Configured in stoke.toml under [builtins].
//! They implement the same 3-hook contract as webhook plugins but with zero network overhead.

use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Configuration for built-in plugins.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BuiltinConfig {
    /// Prompt harness: inject system prompts by task type
    #[serde(default)]
    pub prompt_harness: Option<HarnessConfig>,
    /// PII redaction: strip secrets/API keys from prompts
    #[serde(default)]
    pub pii_redact: Option<PiiConfig>,
    /// Code formatter: format code blocks in responses
    #[serde(default)]
    pub code_formatter: Option<FormatterConfig>,
    /// Audit logger: log all requests to a file
    #[serde(default)]
    pub audit_log: Option<AuditConfig>,
}

// ─── Prompt Harness ──────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HarnessConfig {
    /// System prompts by task type: "code", "reasoning", "chat", "default"
    #[serde(default)]
    pub prompts: HashMap<String, String>,
    /// Whether to prepend (before user messages) or append (after)
    #[serde(default = "default_harness_mode")]
    pub mode: String,
}

fn default_harness_mode() -> String {
    "prepend".to_string()
}

// ─── PII Redaction ──────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PiiConfig {
    /// Custom regex patterns to redact (in addition to built-ins)
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Replacement text for redacted content
    #[serde(default = "default_redact_text")]
    pub replacement: String,
}

fn default_redact_text() -> String {
    "[REDACTED]".to_string()
}

impl Default for PiiConfig {
    fn default() -> Self {
        Self {
            patterns: Vec::new(),
            replacement: default_redact_text(),
        }
    }
}

// ─── Code Formatter ─────────────────────────────────────────────

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FormatterConfig {
    /// Languages to format: "python", "rust", "json", "javascript"
    #[serde(default)]
    pub languages: Vec<String>,
}

// ─── Audit Logger ───────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditConfig {
    /// Log file path (relative to cwd or absolute)
    pub path: String,
    /// Whether to log request bodies (false = just metadata)
    #[serde(default = "default_log_body")]
    pub log_body: bool,
}

fn default_log_body() -> bool {
    false
}

// ─── Built-in Plugins Runner ─────────────────────────────────────

pub struct Builtins {
    config: BuiltinConfig,
    // Pre-compiled regex patterns for PII redaction
    pii_patterns: Vec<(Regex, String)>,
    // Audit log writer (appends to file)
    audit_path: Option<std::path::PathBuf>,
}

impl Builtins {
    pub fn new(config: BuiltinConfig) -> Self {
        // Compile PII patterns
        let mut pii_patterns = Vec::new();

        // Built-in patterns (always active if pii_redact is enabled)
        let builtin_pii = vec![
            // OpenAI API keys (new format: sk-proj-..., legacy: sk-...)
            (r"sk-(?:proj-)?[A-Za-z0-9_-]{20,}", "API_KEY"),
            // Anthropic API keys
            (r"sk-ant-[A-Za-z0-9_-]{20,}", "API_KEY"),
            // Generic Bearer tokens
            (r"Bearer\s+[A-Za-z0-9._-]{20,}", "BEARER_TOKEN"),
            // AWS access keys
            (r"AKIA[0-9A-Z]{16}", "AWS_KEY"),
            // AWS secret keys (40 chars of base64)
            (
                r#"(?i)aws_secret_access_key["\s:=]+["']?[A-Za-z0-9/+]{40}["']?"#,
                "AWS_SECRET",
            ),
            // Google API keys
            (r"AIza[A-zA-Z0-9_-]{35}", "GOOGLE_API_KEY"),
            // Generic API key patterns in env/config
            (
                r#"(?i)(api_key|apikey|token|secret|password)["\s:=]+["']?[A-Za-z0-9_+/=-]{20,}["']?"#,
                "API_KEY",
            ),
            // Email addresses
            (r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}", "EMAIL"),
            // GitHub tokens
            (r"gh[ps]_[A-Za-z0-9]{36,}", "GITHUB_TOKEN"),
            // Slack tokens
            (r"xox[bp]-[A-Za-z0-9-]{10,}", "SLACK_TOKEN"),
            // Private keys (PEM)
            (
                r"(?s)-----BEGIN (?:[A-Z]+ )?PRIVATE KEY-----.*?-----END (?:[A-Z]+ )?PRIVATE KEY-----",
                "PRIVATE_KEY",
            ),
        ];

        if config.pii_redact.is_some() {
            for (pattern, label) in &builtin_pii {
                if let Ok(re) = Regex::new(pattern) {
                    let repl = config
                        .pii_redact
                        .as_ref()
                        .map(|c| c.replacement.clone())
                        .unwrap_or_else(|| "[REDACTED]".to_string());
                    pii_patterns.push((re, format!("{}:{}", label, repl)));
                }
            }
            // Custom patterns
            if let Some(ref pii) = config.pii_redact {
                for pattern in &pii.patterns {
                    if let Ok(re) = Regex::new(pattern) {
                        pii_patterns.push((re.clone(), format!("CUSTOM:{}", pii.replacement)));
                    }
                }
            }
        }

        let audit_path = config
            .audit_log
            .as_ref()
            .map(|a| std::path::PathBuf::from(&a.path));

        Self {
            config,
            pii_patterns,
            audit_path,
        }
    }

    /// pre_request hook: prompt harness injects system prompts
    pub async fn pre_request(
        &self,
        model: &str,
        routing: &str,
        messages: &[Value],
        _api_key: &str,
    ) -> Result<Option<Value>, String> {
        let mut result_messages = None;

        // Prompt harness: inject system prompt based on task type
        if let Some(ref harness) = self.config.prompt_harness {
            // Classify the prompt
            let combined: String = messages
                .iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();

            let task_type = if combined.contains("def ")
                || combined.contains("function ")
                || combined.contains(r#"```"#)
                || combined.contains("implement")
                || combined.contains("rust")
                || combined.contains("python")
            {
                "code"
            } else if combined.contains("prove")
                || combined.contains("solve")
                || combined.contains("calculate")
                || combined.contains("derive")
                || combined.contains("step by step")
            {
                "reasoning"
            } else if combined.len() < 200 {
                "chat"
            } else {
                "default"
            };

            if let Some(system_prompt) = harness.prompts.get(task_type) {
                let system_msg = json!({
                    "role": "system",
                    "content": system_prompt
                });

                let mut new_messages = Vec::with_capacity(messages.len() + 1);
                if harness.mode == "append" {
                    new_messages.extend_from_slice(messages);
                    new_messages.push(system_msg);
                } else {
                    new_messages.push(system_msg);
                    new_messages.extend_from_slice(messages);
                }
                result_messages = Some(json!(new_messages));
            }
        }

        Ok(result_messages)
    }

    /// prompt_filter hook: PII redaction
    pub async fn prompt_filter(
        &self,
        messages: &[Value],
        _model: &str,
        _api_key: &str,
    ) -> Result<Option<Vec<Value>>, String> {
        if self.config.pii_redact.is_none() {
            return Ok(None);
        }

        let mut modified = false;
        let mut new_messages: Vec<Value> = Vec::with_capacity(messages.len());

        for msg in messages {
            let mut new_msg = msg.clone();
            if let Some(content) = new_msg.get_mut("content") {
                modified |= self.redact_value(content);
            }
            new_messages.push(new_msg);
        }

        if modified {
            Ok(Some(new_messages))
        } else {
            Ok(None)
        }
    }

    /// Redact every string below a message's content field without changing
    /// arrays, objects, or non-string JSON values.
    fn redact_value(&self, value: &mut Value) -> bool {
        match value {
            Value::String(text) => {
                let mut redacted = text.clone();
                for (re, label_repl) in &self.pii_patterns {
                    let replacement = label_repl
                        .split_once(':')
                        .map(|(_, replacement)| replacement)
                        .unwrap_or("[REDACTED]");
                    redacted = re.replace_all(&redacted, replacement).into_owned();
                }
                let changed = redacted != *text;
                if changed {
                    *text = redacted;
                }
                changed
            }
            Value::Array(values) => values
                .iter_mut()
                .map(|value| self.redact_value(value))
                .any(|changed| changed),
            Value::Object(object) => object
                .values_mut()
                .map(|value| self.redact_value(value))
                .any(|changed| changed),
            _ => false,
        }
    }

    /// post_response hook: code formatting + audit logging
    pub async fn post_response(
        &self,
        model: &str,
        response: &Value,
        cost_usd: f64,
        elapsed_ms: u64,
        api_key: &str,
    ) -> Option<Value> {
        let mut result = response.clone();

        // Code formatter: format code blocks in responses
        if self.config.code_formatter.is_some() {
            result = self.format_code_blocks(&result);
        }

        // Audit logger: append to file
        if self.config.audit_log.is_some() {
            self.audit_log(model, response, cost_usd, elapsed_ms, api_key);
        }

        Some(result)
    }

    fn format_code_blocks(&self, response: &Value) -> Value {
        let mut resp = response.clone();

        // Get choices array
        let choices = match resp.get("choices").and_then(|c| c.as_array()) {
            Some(c) => c.clone(),
            None => return resp,
        };

        let new_choices: Vec<Value> = choices
            .iter()
            .map(|choice| {
                let mut c = choice.clone();
                if let Some(content) = c
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
                {
                    let formatted = self.format_inline(&content);
                    if let Some(msg) = c.get_mut("message").and_then(|m| m.as_object_mut()) {
                        msg.insert("content".to_string(), json!(formatted));
                    }
                }
                c
            })
            .collect();

        if let Some(obj) = resp.as_object_mut() {
            obj.insert("choices".to_string(), json!(new_choices));
        }

        resp
    }

    /// Simple inline code formatting: normalize whitespace in code blocks
    fn format_inline(&self, content: &str) -> String {
        let languages: Vec<String> = self
            .config
            .code_formatter
            .as_ref()
            .map(|f| f.languages.clone())
            .unwrap_or_default();

        // Find code blocks: triple backtick + optional lang + newline + code + triple backtick
        let re = Regex::new(r#"`{3}(\w+)?\n([\s\S]*?)`{3}"#).unwrap();
        let tbt = "```";
        re.replace_all(content, |caps: &regex::Captures| {
            let lang = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let code = caps.get(2).map(|m| m.as_str()).unwrap_or("");

            // Only format if language is in the list (or list is empty = all)
            if !languages.is_empty() && !languages.iter().any(|l| l == lang) {
                return format!("{}{}\n{}{}", tbt, lang, code, tbt);
            }

            let formatted = match lang {
                "json" => self.format_json(code),
                "python" | "py" => self.format_python(code),
                _ => self.format_generic(code),
            };

            format!("{}{}\n{}{}", tbt, lang, formatted, tbt)
        })
        .to_string()
    }

    fn format_json(&self, code: &str) -> String {
        match serde_json::from_str::<Value>(code.trim()) {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| code.to_string()),
            Err(_) => code.to_string(),
        }
    }

    fn format_python(&self, code: &str) -> String {
        // Simple: strip trailing whitespace, normalize indentation
        code.lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn format_generic(&self, code: &str) -> String {
        // Strip trailing whitespace per line
        code.lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn audit_log(
        &self,
        model: &str,
        response: &Value,
        cost_usd: f64,
        elapsed_ms: u64,
        api_key: &str,
    ) {
        let path = match &self.audit_path {
            Some(p) => p,
            None => return,
        };

        let timestamp = chrono::Utc::now().to_rfc3339();
        let key_prefix = &api_key[..8.min(api_key.len())];

        let log_entry = if self
            .config
            .audit_log
            .as_ref()
            .map(|a| a.log_body)
            .unwrap_or(false)
        {
            json!({
                "ts": timestamp,
                "model": model,
                "cost_usd": cost_usd,
                "elapsed_ms": elapsed_ms,
                "key": key_prefix,
                "response": response,
            })
            .to_string()
        } else {
            json!({
                "ts": timestamp,
                "model": model,
                "cost_usd": cost_usd,
                "elapsed_ms": elapsed_ms,
                "key": key_prefix,
            })
            .to_string()
        };

        // Append to file (non-blocking, best-effort). Concurrent writers may
        // still interleave records; changing that behavior is outside this fix.
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // `mode` applies only when the file is newly created.
            options.mode(0o600);
        }
        if let Ok(mut file) = options.open(path) {
            let _ = writeln!(file, "{}", log_entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn pii_builtins() -> Builtins {
        Builtins::new(BuiltinConfig {
            pii_redact: Some(PiiConfig::default()),
            ..BuiltinConfig::default()
        })
    }

    #[tokio::test]
    async fn redacts_multiline_pem_and_nested_content_leaves() {
        let builtins = pii_builtins();
        let messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": format!("-----BEGIN {}-----\nsecret\n-----END {}-----", "PRIVATE KEY", "PRIVATE KEY")},
                {"type": "image", "image": {"url": "https://example.test/image", "detail": 2}},
                42,
                true
            ]
        })];

        let filtered = builtins
            .prompt_filter(&messages, "model", "key")
            .await
            .unwrap();
        let filtered = filtered.expect("PEM should be redacted");
        assert_eq!(filtered[0]["content"][0]["type"], "text");
        assert_eq!(filtered[0]["content"][0]["text"], "[REDACTED]");
        assert_eq!(filtered[0]["content"][1]["image"]["detail"], 2);
        assert_eq!(filtered[0]["content"][2], 42);
        assert_eq!(filtered[0]["content"][3], true);
    }

    #[tokio::test]
    async fn pii_redaction_is_opt_in() {
        let builtins = Builtins::new(BuiltinConfig::default());
        let messages = vec![json!({"role": "user", "content": "x@example.com"})];
        assert!(builtins
            .prompt_filter(&messages, "model", "key")
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn audit_log_defaults_to_metadata_only() {
        let config: AuditConfig = toml::from_str("path = 'audit.jsonl'").unwrap();
        assert!(!config.log_body);
        assert!(!default_log_body());
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_audit_log_is_owner_only() {
        let path = std::env::temp_dir().join(format!("stoke-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let builtins = Builtins {
            config: BuiltinConfig::default(),
            pii_patterns: Vec::new(),
            audit_path: Some(path.clone()),
        };
        builtins.audit_log("model", &json!({"secret": "body"}), 0.0, 1, "key");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let contents = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600);
        assert!(!contents.contains("response"));
    }
}

// ─── Multi-Endpoint Route Profiles ────────────────────────────────

/// A named route profile with its own model, pattern, plugins, and budget.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RouteProfile {
    /// Name of the profile (for logging)
    pub name: String,
    /// URL path, e.g. "/v1/code/completions"
    pub path: String,
    /// Routing pattern: "auto", "single", "test_vote", "cascade_test", "self_consistency"
    #[serde(default = "default_route_pattern")]
    pub routing: String,
    /// Fixed model (empty = auto-route picks)
    #[serde(default)]
    pub model: String,
    /// Fixed vote_models list
    #[serde(default)]
    pub vote_models: Vec<String>,
    /// Budget cap in USD for this profile (0 = unlimited)
    #[serde(default)]
    pub budget_usd: f64,
    /// Rate limit (requests per minute, 0 = unlimited)
    #[serde(default)]
    pub rate_limit: u32,
    /// Built-in plugins to enable for this profile
    #[serde(default)]
    pub builtins: Vec<String>,
    /// Provider tiers this route may use. Empty permits every configured tier.
    #[serde(default)]
    pub allowed_tiers: Vec<String>,
    /// Whether to enable streaming for this profile
    #[serde(default = "default_true")]
    pub stream: bool,
    /// Optional route-local response cache policy. Omitted preserves the
    /// gateway's legacy exact+semantic cache behavior.
    #[serde(default)]
    pub response_cache: Option<ResponseCachePolicy>,
    /// Coalesce cold identical requests on this named route. Requires an
    /// explicit exact response cache policy; disabled by default.
    #[serde(default)]
    pub coalesce: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResponseCachePolicy {
    /// `off` disables response-cache lookup and storage; `exact` enables only
    /// full-request identity matching for this route.
    pub mode: String,
    /// Required and positive for `exact`. Route retention never extends the
    /// existing global retention; the effective TTL is their minimum.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

fn default_route_pattern() -> String {
    "auto".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RoutesConfig {
    #[serde(default)]
    pub profiles: Vec<RouteProfile>,
}

/// Resolve a route profile by path.
pub fn find_profile<'a>(profiles: &'a [RouteProfile], path: &str) -> Option<&'a RouteProfile> {
    profiles.iter().find(|p| p.path == path)
}
