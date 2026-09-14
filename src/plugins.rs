use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use futures_util::StreamExt;
use std::time::Duration;

pub const HEADROOM_MAX_BYTES: usize = 1024 * 1024;
pub const HEADROOM_MAX_OUTPUTS: usize = 128;
pub const HEADROOM_TOKEN_ENV: &str = "STOKE_HEADROOM_TOKEN";

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
    /// Optional native Responses output filter.
    #[serde(default)]
    pub headroom: HeadroomConfig,
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
    headroom_client: Option<reqwest::Client>,
    headroom_token: Option<String>,
}

impl Plugins {
    pub fn new(config: PluginConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        let headroom_token = std::env::var(HEADROOM_TOKEN_ENV)
            .ok()
            .filter(|token| !token.is_empty());
        let headroom_client = if config.headroom.enabled
            && headroom_token.is_some()
            && config.headroom.url.as_deref().is_some_and(numeric_loopback_http)
        {
            reqwest::Client::builder()
                .timeout(Duration::from_millis(config.headroom.timeout_ms.max(1)))
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .ok()
        } else {
            None
        };
        Self { config, client, headroom_client, headroom_token }
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

    /// Run the optional native Responses output filter after admission.
    /// The worker receives only the eligible output strings and never the request.
    pub async fn headroom_filter(&self, body: &Value) -> HeadroomResult {
        if !self.config.headroom.enabled {
            return HeadroomResult { body: body.clone(), status: HeadroomStatus::Disabled };
        }
        let Some(token) = self.headroom_token.as_deref() else {
            return HeadroomResult { body: body.clone(), status: HeadroomStatus::Unavailable };
        };
        let originals = extract_headroom_outputs(body);
        if originals.is_empty() {
            return HeadroomResult { body: body.clone(), status: HeadroomStatus::Bypassed };
        }
        if originals.len() > HEADROOM_MAX_OUTPUTS {
            return HeadroomResult { body: body.clone(), status: HeadroomStatus::Bypassed };
        }
        let (Some(url), Some(client)) = (self.config.headroom.url.as_deref(), self.headroom_client.as_ref()) else {
            return HeadroomResult { body: body.clone(), status: HeadroomStatus::Unavailable };
        };
        let request_body = match serde_json::to_vec(&json!({ "outputs": originals })) {
            Ok(body) if body.len() <= HEADROOM_MAX_BYTES => body,
            _ => return HeadroomResult { body: body.clone(), status: HeadroomStatus::Bypassed },
        };
        let response = match headroom_request(client, url, token, request_body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return HeadroomResult { body: body.clone(), status: HeadroomStatus::Unavailable },
        };
        if !response.status().is_success() {
            return HeadroomResult { body: body.clone(), status: HeadroomStatus::Unavailable };
        }
        let response_body = match read_headroom_response(response).await {
            Ok(body) => body,
            Err(_) => return HeadroomResult { body: body.clone(), status: HeadroomStatus::Rejected },
        };
        let worker: HeadroomWorkerResponse = match serde_json::from_slice(&response_body) {
            Ok(worker) => worker,
            Err(_) => return HeadroomResult { body: body.clone(), status: HeadroomStatus::Rejected },
        };
        if worker.outputs.len() > HEADROOM_MAX_OUTPUTS || worker.outputs.len() != originals.len() {
            return HeadroomResult { body: body.clone(), status: HeadroomStatus::Rejected };
        }
        let changed = originals.iter().zip(&worker.outputs).any(|(old, new)| old != new);
        let Some(updated) = replace_headroom_outputs(body, &worker.outputs) else {
            return HeadroomResult { body: body.clone(), status: HeadroomStatus::Rejected };
        };
        HeadroomResult {
            body: updated,
            status: if changed { HeadroomStatus::Compressed } else { HeadroomStatus::Bypassed },
        }
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

/// Optional native Responses output-only Headroom worker configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadroomConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_headroom_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_headroom_timeout_ms() -> u64 {
    1_000
}

impl Default for HeadroomConfig {
    fn default() -> Self {
        Self { enabled: false, url: None, timeout_ms: default_headroom_timeout_ms() }
    }
}

impl HeadroomConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled { return Ok(()); }
        let Some(url) = self.url.as_deref() else {
            return Err("[plugins.headroom] url is required when enabled".into());
        };
        if !numeric_loopback_http(url) {
            return Err("[plugins.headroom] url must be numeric-loopback http://".into());
        }
        Ok(())
    }
}

/// The optimization outcome reported on a dispatched non-stream response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadroomStatus {
    Disabled,
    Bypassed,
    Compressed,
    Unavailable,
    Rejected,
}

impl HeadroomStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Bypassed => "bypassed",
            Self::Compressed => "compressed",
            Self::Unavailable => "unavailable",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HeadroomResult {
    pub body: Value,
    pub status: HeadroomStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadroomWorkerResponse {
    outputs: Vec<String>,
}

fn numeric_loopback_http(url_str: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url_str) else { return false };
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else { return false };
    host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Extract exactly the native function-call output strings eligible for filtering.
pub fn extract_headroom_outputs(body: &Value) -> Vec<String> {
    body.get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .filter_map(|item| item.get("output").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// Return the request with a complete candidate batch applied, or None atomically.
pub fn replace_headroom_outputs(body: &Value, candidates: &[String]) -> Option<Value> {
    let originals = extract_headroom_outputs(body);
    if originals.len() > HEADROOM_MAX_OUTPUTS
        || originals.len() != candidates.len()
        || !originals.iter().zip(candidates).all(|(old, new)| validate_headroom_candidate(old, new))
    {
        return None;
    }
    let mut updated = body.clone();
    let Some(items) = updated.get_mut("input").and_then(Value::as_array_mut) else {
        return Some(updated);
    };
    let mut candidate = candidates.iter();
    for item in items {
        if item.get("type").and_then(Value::as_str) == Some("function_call_output")
            && item.get("output").and_then(Value::as_str).is_some()
        {
            if let Some(output) = item.get_mut("output") {
                *output = Value::String(candidate.next().expect("validated count").clone());
            }
        }
    }
    Some(updated)
}

async fn read_headroom_response(response: reqwest::Response) -> Result<Vec<u8>, ()> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ())?;
        if body.len().saturating_add(chunk.len()) > HEADROOM_MAX_BYTES {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn headroom_request(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: Vec<u8>,
) -> reqwest::RequestBuilder {
    client
        .post(url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body)
}

fn outside_string_lexemes(json: &str) -> Option<Vec<String>> {
    let mut lexemes = Vec::new();
    let bytes = json.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() { i += 1; continue; }
        if bytes[i] == b'"' {
            let start = i;
            i += 1;
            let mut escaped = false;
            while i < bytes.len() {
                let byte = bytes[i];
                i += 1;
                if escaped { escaped = false; continue; }
                if byte == b'\\' { escaped = true; continue; }
                if byte == b'"' { break; }
            }
            if i > bytes.len() || bytes.get(i - 1) != Some(&b'"') || escaped {
                return None;
            }
            lexemes.push(json[start..i].to_string());
        } else {
            let start = i;
            i += 1;
            if !matches!(bytes[start], b'{' | b'}' | b'[' | b']' | b':' | b',') {
                while i < bytes.len() && !bytes[i].is_ascii_whitespace()
                    && !matches!(bytes[i], b'{' | b'}' | b'[' | b']' | b':' | b',') { i += 1; }
            }
            lexemes.push(json[start..i].to_string());
        }
    }
    Some(lexemes)
}

struct TopLevelOutputInfo {
    occurrences: usize,
    embedded_span: Option<(usize, usize, usize, usize)>,
}

fn top_level_output_info(json: &str) -> Option<TopLevelOutputInfo> {
    let value: Value = serde_json::from_str(json).ok()?;
    if !value.is_object() { return None; }
    let bytes = json.as_bytes();
    let mut i = 0;
    let skip = |i: &mut usize| while *i < bytes.len() && bytes[*i].is_ascii_whitespace() { *i += 1 };
    skip(&mut i);
    if bytes.get(i) != Some(&b'{') { return None; }
    i += 1;
    let mut occurrences = 0;
    let mut embedded_span = None;
    loop {
        skip(&mut i);
        if bytes.get(i) == Some(&b'}') { i += 1; break; }
        let key_start = i;
        let key_end = scan_string_end(bytes, i)?;
        let key: String = serde_json::from_str(&json[key_start..key_end]).ok()?;
        i = key_end;
        skip(&mut i);
        if bytes.get(i) != Some(&b':') { return None; }
        i += 1;
        skip(&mut i);
        let value_start = i;
        let value_end = scan_value_end(bytes, i)?;
        if key == "output" {
            occurrences += 1;
            if occurrences == 1 && bytes.get(value_start) == Some(&b'"') {
                let inner_start = value_start + 1;
                let inner_end = value_end.checked_sub(1)?;
                let decoded: String = serde_json::from_str(&json[value_start..value_end]).ok()?;
                if serde_json::from_str::<Value>(&decoded).is_ok() {
                    embedded_span = Some((value_start, value_end, inner_start, inner_end));
                }
            }
        }
        i = value_end;
        skip(&mut i);
        match bytes.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => { i += 1; break; },
            _ => return None,
        }
    }
    skip(&mut i);
    if i != bytes.len() { return None; }
    Some(TopLevelOutputInfo { occurrences, embedded_span })
}

fn top_level_output_span(json: &str) -> Option<(usize, usize, usize, usize)> {
    let info = top_level_output_info(json)?;
    if info.occurrences == 1 { info.embedded_span } else { None }
}

fn scan_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'"') { return None; }
    let mut i = start + 1;
    let mut escaped = false;
    while i < bytes.len() {
        let byte = bytes[i]; i += 1;
        if escaped { escaped = false; continue; }
        if byte == b'\\' { escaped = true; continue; }
        if byte == b'"' { return Some(i); }
    }
    None
}

fn scan_value_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) == Some(&b'"') { return scan_string_end(bytes, start); }
    let mut i = start;
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_string {
            if escaped { escaped = false; } else if byte == b'\\' { escaped = true; } else if byte == b'"' { in_string = false; }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => stack.push(byte),
                b'}' if stack.is_empty() => return Some(i),
                b'}' => { if stack.pop() != Some(b'{') { return None; } if stack.is_empty() { return Some(i + 1); } },
                b']' => { if stack.pop() != Some(b'[') { return None; } if stack.is_empty() { return Some(i + 1); } },
                b',' if stack.is_empty() => return Some(i),
                c if c.is_ascii_whitespace() && stack.is_empty() => return Some(i),
                _ => {}
            }
        }
        i += 1;
    }
    if stack.is_empty() { Some(i) } else { None }
}

/// Validate one worker candidate without changing any JSON data or string bytes.
pub fn validate_headroom_candidate(original: &str, candidate: &str) -> bool {
    if original == candidate { return true; }
    if candidate.len() >= original.len() { return false; }
    if top_level_output_info(original).is_some_and(|info| info.occurrences > 1)
        || top_level_output_info(candidate).is_some_and(|info| info.occurrences > 1)
    {
        return false;
    }
    if let (Some((_, _, old_start, old_end)), Some((_, _, new_inner_start, new_inner_end))) =
        (top_level_output_span(original), top_level_output_span(candidate))
    {
        let old_prefix = &original[..old_start];
        let old_suffix = &original[old_end..];
        let new_prefix = &candidate[..new_inner_start];
        let new_suffix = &candidate[new_inner_end..];
        if old_prefix != new_prefix || old_suffix != new_suffix { return false; }
        let old_inner: String = match serde_json::from_str(&original[old_start - 1..old_end + 1]) {
            Ok(value) => value,
            Err(_) => return false,
        };
        let new_inner: String = match serde_json::from_str(&candidate[new_inner_start - 1..new_inner_end + 1]) {
            Ok(value) => value,
            Err(_) => return false,
        };
        return validate_direct_json(&old_inner, &new_inner);
    }
    validate_direct_json(original, candidate)
}

fn validate_direct_json(original: &str, candidate: &str) -> bool {
    if serde_json::from_str::<Value>(original).is_err() || serde_json::from_str::<Value>(candidate).is_err() { return false; }
    outside_string_lexemes(original) == outside_string_lexemes(candidate)
}

#[cfg(test)]
mod headroom_contract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_only_whitespace_minification_and_preserves_precision() {
        assert!(validate_headroom_candidate(
            r#"{"output": "plain", "rows": [{"id": 1}, {"id": 2}]}"#,
            r#"{"output":"plain","rows":[{"id":1},{"id":2}]}"#,
        ));
        assert!(validate_headroom_candidate(
            r#"{"n": 9007199254740993, "ok": true, "s": "  \\u0020 "}"#,
            r#"{"n":9007199254740993,"ok":true,"s":"  \\u0020 "}"#,
        ));
        assert!(!validate_headroom_candidate(r#"{"n": 1}"#, r#"{"n": 1.0}"#));
        assert!(!validate_headroom_candidate(r#"{"ok": true}"#, r#"{"ok": 1}"#));
        assert!(!validate_headroom_candidate(
            r#"{"s":"quoted  whitespace"}"#,
            r#"{"s":"quoted whitespace"}"#,
        ));
    }

    #[test]
    fn validates_json_inside_output_envelope_without_touching_metadata_bytes() {
        let original = r#"{ "meta": 1.0, "output": "{\"x\": 1, \"text\": \"a b\"}", "tail": true }"#;
        let candidate = r#"{ "meta": 1.0, "output": "{\"x\":1,\"text\":\"a b\"}", "tail": true }"#;
        assert!(validate_headroom_candidate(original, candidate));
        assert!(!validate_headroom_candidate(
            original,
            r#"{ "meta": 2.0, "output": "{\"x\":1,\"text\":\"a b\"}", "tail": true }"#,
        ));
        assert!(!validate_headroom_candidate(
            original,
            r#"{ "meta": 1.0, "output": "{\"x\":1,\"text\":\"a b\"}", "tail": true, "output": "{}" }"#,
        ));
        assert!(!validate_headroom_candidate(
            r#"{ "output": "{\"x\": 1}", "output": "{\"x\": 1}" }"#,
            r#"{"output":"{\"x\":1}","output":"{\"x\":1}"}"#,
        ));
    }

    #[test]
    fn extracts_only_function_call_output_strings_and_replaces_atomically() {
        let body = json!({
            "model": "kept",
            "input": [
                {"type":"message", "content":"not eligible"},
                {"type":"function_call_output", "call_id":"call-1", "output": "{ \"a\": 1 }"},
                {"type":"function_call_output", "call_id":"call-2", "output":42},
                {"type":"function_call_output", "call_id":"call-3", "output":"plain"}
            ],
            "tools": [{"name":"kept"}]
        });
        assert_eq!(extract_headroom_outputs(&body), vec![r#"{ "a": 1 }"#, "plain"]);
        let candidates = vec![r#"{"a":1}"#.to_string(), "plain".to_string()];
        let updated = replace_headroom_outputs(&body, &candidates).expect("valid batch");
        assert_eq!(updated["model"], "kept");
        assert_eq!(updated["tools"][0]["name"], "kept");
        assert_eq!(updated["input"][1]["output"], "{\"a\":1}");
        assert_eq!(updated["input"][3]["output"], "plain");
        assert!(replace_headroom_outputs(&body, &["bad".into()]).is_none());
    }

    #[test]
    fn headroom_worker_auth_and_shared_bounds_are_explicit() {
        assert_eq!(HEADROOM_TOKEN_ENV, "STOKE_HEADROOM_TOKEN");
        assert_eq!(HEADROOM_MAX_BYTES, 1024 * 1024);
        assert_eq!(HEADROOM_MAX_OUTPUTS, 128);
        let client = reqwest::Client::new();
        let request = headroom_request(&client, "http://127.0.0.1:9100/compress", "worker-secret", b"{}".to_vec())
            .build()
            .expect("worker request builds");
        assert_eq!(request.headers()["authorization"], "Bearer worker-secret");
        assert!(request.headers().get("x-stoke-key").is_none());
        assert!(request.headers().get("chatgpt-account-id").is_none());
        let oversized = vec!["x".repeat(HEADROOM_MAX_BYTES); HEADROOM_MAX_OUTPUTS];
        let serialized = serde_json::to_vec(&json!({ "outputs": oversized })).unwrap();
        assert!(serialized.len() > HEADROOM_MAX_BYTES);
    }

    #[test]
    fn oversized_output_batches_are_bypassed_atomically() {
        let body = json!({
            "model": "kept",
            "input": (0..=HEADROOM_MAX_OUTPUTS)
                .map(|_| json!({"type":"function_call_output", "output":"{}"}))
                .collect::<Vec<_>>(),
            "tools": [{"name":"kept"}],
            "opaque": {"keep": true}
        });
        let originals = extract_headroom_outputs(&body);
        assert_eq!(originals.len(), HEADROOM_MAX_OUTPUTS + 1);
        assert!(replace_headroom_outputs(&body, &originals).is_none());
    }

    #[test]
    fn headroom_defaults_and_target_validation_are_narrow() {
        let defaults = HeadroomConfig::default();
        assert!(!defaults.enabled);
        assert_eq!(defaults.timeout_ms, 1000);
        assert!(HeadroomConfig { enabled: true, url: Some("http://127.0.0.1:9100/compress".into()), timeout_ms: 1000 }.validate().is_ok());
        for url in ["http://localhost:9100/compress", "https://127.0.0.1/compress", "http://192.168.1.2/compress", "http://127.0.0.1.evil/compress"] {
            assert!(HeadroomConfig { enabled: true, url: Some(url.into()), timeout_ms: 1000 }.validate().is_err());
        }
        assert!(HeadroomConfig { enabled: true, url: None, timeout_ms: 1000 }.validate().is_err());
    }
}