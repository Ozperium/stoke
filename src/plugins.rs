use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

/// Should this plugin webhook URL be refused?
///
/// Threat model. Plugin URLs come from `stoke.toml`, which the operator writes.
/// Anyone able to edit that file can already run code as the operator, so
/// classic SSRF hardening (blocking loopback and RFC1918) defends nothing here
/// — it only breaks the normal deployment, where a plugin is a sidecar on
/// `127.0.0.1` or a service on the LAN.
///
/// What we still refuse, as cheap defence-in-depth against a poisoned or
/// fat-fingered config: cloud instance-metadata endpoints (never a legitimate
/// plugin target, and the classic credential-theft pivot), the unspecified
/// address, and anything that isn't plain HTTP(S).
fn is_forbidden_url(url_str: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url_str) else {
        return true; // Invalid URL → refuse
    };
    let host = match url.host_str() {
        Some(h) => h,
        None => return true,
    };
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return true; // No file://, gopher://, etc.
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(v4) => {
                // 169.254.169.254 (AWS/GCP/Azure) and 169.254.170.2 (ECS task role)
                let is_metadata = v4.octets() == [169, 254, 169, 254]
                    || v4.octets() == [169, 254, 170, 2];
                if is_metadata || v4.is_unspecified() {
                    return true;
                }
            }
            std::net::IpAddr::V6(v6) => {
                // fd00:ec2::254 is the AWS IMDS IPv6 endpoint
                if v6.is_unspecified() || v6.is_multicast() || v6.segments()[0] == 0xfd00 {
                    return true;
                }
            }
        }
    } else {
        let blocked_hosts = ["metadata.google.internal", "metadata"];
        if blocked_hosts.iter().any(|h| host.eq_ignore_ascii_case(h)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod url_tests {
    use super::is_forbidden_url;

    #[test]
    fn allows_the_normal_plugin_deployments() {
        // A sidecar on loopback is THE common case and must work.
        assert!(!is_forbidden_url("http://127.0.0.1:9100/filter"));
        assert!(!is_forbidden_url("http://localhost:9100/filter"));
        assert!(!is_forbidden_url("http://[::1]:9100/filter"));
        // A service elsewhere on the LAN.
        assert!(!is_forbidden_url("http://192.168.1.50:8080/guard"));
        assert!(!is_forbidden_url("http://10.0.0.7/hook"));
        // And a hosted one.
        assert!(!is_forbidden_url("https://guard.example.com/filter"));
    }

    #[test]
    fn refuses_metadata_endpoints_and_junk() {
        assert!(is_forbidden_url("http://169.254.169.254/latest/meta-data/"));
        assert!(is_forbidden_url("http://169.254.170.2/v2/credentials"));
        assert!(is_forbidden_url("http://metadata.google.internal/computeMetadata/v1/"));
        assert!(is_forbidden_url("http://METADATA/computeMetadata"));
        assert!(is_forbidden_url("http://0.0.0.0:9100/"));
        assert!(is_forbidden_url("file:///etc/passwd"));
        assert!(is_forbidden_url("gopher://evil/"));
        assert!(is_forbidden_url("not a url"));
    }
}

/// Plugin configuration from stoke.toml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginConfig {
    /// Webhook URLs called before routing — can override model, routing, vote_models
    #[serde(default)]
    pub pre_request: Vec<String>,
    /// Webhook URLs called before any model call — can block or redact messages
    #[serde(default)]
    pub prompt_filter: Vec<String>,
    /// Webhook URLs called after response — can audit or transform output
    #[serde(default)]
    pub post_response: Vec<String>,
    /// JS/TS plugin file paths (requires `js-plugins` feature)
    #[serde(default)]
    pub scripts: Vec<String>,
}

/// Context sent to pre_request plugins
#[derive(Debug, Clone, Serialize)]
pub struct PreRequestContext<'a> {
    pub model: &'a str,
    pub routing: &'a str,
    pub messages: &'a [Value],
    pub api_key: &'a str,
    pub metadata: Value,
}

/// What a pre_request plugin can return
#[derive(Debug, Clone, Deserialize)]
pub struct PreRequestResult {
    /// Override the model (empty = keep original)
    #[serde(default)]
    pub model: Option<String>,
    /// Override the routing pattern (empty = keep original)
    #[serde(default)]
    pub routing: Option<String>,
    /// Override vote_models
    #[serde(default)]
    pub vote_models: Option<Vec<String>>,
    /// Block the request entirely with an error message
    #[serde(default)]
    pub block: Option<String>,
    /// Free-form metadata to pass forward
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// Context sent to prompt_filter plugins
#[derive(Debug, Clone, Serialize)]
pub struct PromptFilterContext<'a> {
    pub messages: &'a [Value],
    pub model: &'a str,
    pub api_key: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptFilterResult {
    /// Block the request with this error message
    #[serde(default)]
    pub block: Option<String>,
    /// Replace messages (e.g. redacted versions)
    #[serde(default)]
    pub messages: Option<Vec<Value>>,
}

/// Context sent to post_response plugins
#[derive(Debug, Clone, Serialize)]
pub struct PostResponseContext<'a> {
    pub model: &'a str,
    pub response: &'a Value,
    pub cost_usd: f64,
    pub elapsed_ms: u64,
    pub api_key: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostResponseResult {
    /// Replace the response body
    #[serde(default)]
    pub response: Option<Value>,
}

pub struct Plugins {
    config: PluginConfig,
    client: reqwest::Client,
}

impl Plugins {
    pub fn new(config: PluginConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { config, client }
    }

    pub fn has_pre_request(&self) -> bool {
        !self.config.pre_request.is_empty()
    }

    pub fn has_prompt_filter(&self) -> bool {
        !self.config.prompt_filter.is_empty()
    }

    pub fn has_post_response(&self) -> bool {
        !self.config.post_response.is_empty()
    }

    /// Call all pre_request plugins sequentially. Later plugins see earlier plugins' overrides.
    /// Returns Ok(merged result) or Err(block message).
    pub async fn pre_request(
        &self,
        model: &str,
        routing: &str,
        messages: &[Value],
        api_key: &str,
    ) -> Result<PreRequestResult, String> {
        let mut current_model = model.to_string();
        let mut current_routing = routing.to_string();
        let mut current_vote_models: Vec<String> = Vec::new();
        let mut metadata = json!({});

        for url in &self.config.pre_request {
            if is_forbidden_url(url) {
                return Err(format!("plugin {} refused: forbidden webhook URL (metadata endpoint or non-HTTP scheme)", url));
            }
            let ctx = PreRequestContext {
                model: &current_model,
                routing: &current_routing,
                messages,
                api_key,
                metadata: metadata.clone(),
            };
            let resp = self
                .client
                .post(url)
                .json(&ctx)
                .send()
                .await
                .map_err(|e| format!("plugin {} error: {}", url, e))?;

            if !resp.status().is_success() {
                return Err(format!("plugin {} returned {}", url, resp.status()));
            }

            let result: PreRequestResult = resp
                .json()
                .await
                .map_err(|e| format!("plugin {} bad response: {}", url, e))?;

            if let Some(ref block) = result.block {
                return Err(block.clone());
            }
            if let Some(ref m) = result.model {
                if !m.is_empty() {
                    current_model = m.clone();
                }
            }
            if let Some(ref r) = result.routing {
                if !r.is_empty() {
                    current_routing = r.clone();
                }
            }
            if let Some(ref vm) = result.vote_models {
                current_vote_models = vm.clone();
            }
            if let Some(ref md) = result.metadata {
                metadata = md.clone();
            }
        }

        Ok(PreRequestResult {
            model: Some(current_model),
            routing: Some(current_routing),
            vote_models: Some(current_vote_models),
            block: None,
            metadata: Some(metadata),
        })
    }

    /// Call all prompt_filter plugins. Returns Ok(possibly modified messages) or Err(block message).
    pub async fn prompt_filter(
        &self,
        messages: &[Value],
        model: &str,
        api_key: &str,
    ) -> Result<Vec<Value>, String> {
        let mut current_messages: Vec<Value> = messages.to_vec();

        for url in &self.config.prompt_filter {
            if is_forbidden_url(url) {
                return Err(format!("filter {} refused: forbidden webhook URL (metadata endpoint or non-HTTP scheme)", url));
            }
            let ctx = PromptFilterContext {
                messages: &current_messages,
                model,
                api_key,
            };
            let resp = self
                .client
                .post(url)
                .json(&ctx)
                .send()
                .await
                .map_err(|e| format!("filter {} error: {}", url, e))?;

            if !resp.status().is_success() {
                return Err(format!("filter {} returned {}", url, resp.status()));
            }

            let result: PromptFilterResult = resp
                .json()
                .await
                .map_err(|e| format!("filter {} bad response: {}", url, e))?;

            if let Some(ref block) = result.block {
                return Err(block.clone());
            }
            if let Some(ref msgs) = result.messages {
                current_messages = msgs.clone();
            }
        }

        Ok(current_messages)
    }

    /// Call all post_response plugins. Returns possibly modified response.
    pub async fn post_response(
        &self,
        model: &str,
        response: &Value,
        cost_usd: f64,
        elapsed_ms: u64,
        api_key: &str,
    ) -> Value {
        let mut current_response = response.clone();

        for url in &self.config.post_response {
            if is_forbidden_url(url) {
                tracing::warn!("post_response plugin {} refused: forbidden webhook URL (metadata endpoint or non-HTTP scheme)", url);
                continue;
            }
            let ctx = PostResponseContext {
                model,
                response: &current_response,
                cost_usd,
                elapsed_ms,
                api_key,
            };
            match self.client.post(url).json(&ctx).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(result) = resp.json::<PostResponseResult>().await {
                        if let Some(ref r) = result.response {
                            current_response = r.clone();
                        }
                    }
                }
                _ => {
                    tracing::warn!("post_response plugin {} failed, continuing", url);
                }
            }
        }

        current_response
    }
}