//! Native Anthropic Messages API endpoint (`/v1/messages`).
//!
//! Lets Claude Code (and anything speaking the Anthropic Messages API) sit
//! behind Stoke's enforcement: point `ANTHROPIC_BASE_URL` at the gateway.
//! The same auth / budget / rate-limit / loop-detection checks run before the
//! request is forwarded to a configured Anthropic-type provider. Enforcement is
//! format-agnostic — only the passthrough wire format differs from /v1/chat.
//! Streamed responses are billed from the usage Anthropic reports as the stream
//! passes; the bytes the client sees are unchanged.
//!
//! Scope: this is a policy-enforcing passthrough to Anthropic (or any
//! Anthropic-compatible upstream). Translating Anthropic <-> OpenAI so Claude
//! Code can hit local Ollama models is a separate, larger piece (roadmap).

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::Arc;

use crate::config::ProviderConfig;
use crate::router::SHARED_CLIENT;
use crate::AppState;

const ANTHROPIC_VERSION: &str = "2023-06-01";

pub async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    // Auth: the global middleware already gated this, but we re-validate to
    // recover the key string for per-key budget/loop accounting.
    let api_key = match state.auth.validate(
        headers
            .get("authorization")
            .and_then(|h| h.to_str().ok()),
    ) {
        Some(k) => k,
        None => return (StatusCode::UNAUTHORIZED, "Invalid or missing API key").into_response(),
    };

    let model = req.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let stream = req.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    // Enforcement (same guard as /v1/chat/completions): loop detection needs a
    // stable hash of the prompt plus the raw text for semantic similarity.
    let prompt_text = extract_prompt_text(&req);
    let prompt_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(model.as_bytes());
        h.update(prompt_text.as_bytes());
        hex::encode(h.finalize())
    };
    if let Err(reason) = state
        .budget
        .check_with_prompt(&api_key, &prompt_hash, &prompt_text)
        .await
    {
        return (StatusCode::TOO_MANY_REQUESTS, reason).into_response();
    }

    // Resolve the upstream Anthropic provider.
    let provider = match state.config.providers.iter().find(|p| p.r#type == "anthropic") {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "No Anthropic provider configured. Add a provider with type = \"anthropic\" \
                 and base_url = \"https://api.anthropic.com\" (api_key_env for the key).",
            )
                .into_response();
        }
    };

    // The same dispatch gate router::call_provider_hop applies. This path does not
    // go through the router, so without it an Anthropic model with no configured
    // price would be forwarded and metered at $0 — the budget cap would never move
    // for the one client this endpoint exists to serve.
    if let Err(reason) = crate::cost::global().allows(&provider.tier, &model) {
        return (StatusCode::FORBIDDEN, reason).into_response();
    }

    // Hold the money this request could cost, so concurrent requests on the same
    // key are admitted against a figure that includes each other. Claude Code
    // always sends max_tokens, which makes the hold exact rather than assumed.
    let max_tokens = req
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(state.config.limits.assumed_max_output_tokens);
    let reservation = match state.budget.try_reserve(
        &api_key,
        crate::cost::global().max_cost(&model, (prompt_text.len() / 4) as u64, max_tokens),
    ) {
        Ok(r) => r,
        Err(reason) => return (StatusCode::TOO_MANY_REQUESTS, reason).into_response(),
    };

    if stream {
        forward_stream(&state, &api_key, provider, &model, &req, reservation).await
    } else {
        let _hold = reservation; // released when this handler returns
        forward_once(&state, &api_key, provider, &model, &req).await
    }
}

/// Non-streaming: forward, record spend from the usage block, return the
/// Anthropic response verbatim with cost/node surfaced in response headers
/// (the body stays a clean Anthropic payload the client expects).
async fn forward_once(
    state: &AppState,
    api_key: &str,
    provider: &ProviderConfig,
    model: &str,
    req: &Value,
) -> Response {
    let url = anthropic_url(provider);
    let resp = match anthropic_request(provider, &url, req).send().await {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("Anthropic request failed: {}", e))
                .into_response()
        }
    };
    let status = resp.status();
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("Invalid Anthropic response: {}", e))
                .into_response()
        }
    };

    // Anthropic usage → OpenAI-shaped usage for the shared Pricer.
    let cost_usd = body
        .get("usage")
        .map(|u| {
            let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let usage = serde_json::json!({
                "prompt_tokens": input,
                "completion_tokens": output,
                "total_tokens": input + output
            });
            crate::cost::global().calculate(model, Some(&usage)).cost_usd
        })
        .unwrap_or(0.0);
    state.budget.record_spend(api_key, cost_usd);
    tracing::info!("/v1/messages: model={} provider={} cost=${:.6}", model, provider.name, cost_usd);

    let mut out = Json(body).into_response();
    *out.status_mut() = status;
    if let Ok(v) = format!("{:.6}", cost_usd).parse() {
        out.headers_mut().insert("x-stoke-cost", v);
    }
    if let Ok(v) = provider.name.parse() {
        out.headers_mut().insert("x-stoke-node", v);
    }
    out
}

/// Bills an Anthropic SSE stream when it ends — or when the client walks away
/// mid-stream, which costs the same. Anthropic reports usage without being
/// asked: `message_start` carries the input tokens, `message_delta` the running
/// output count. The bytes reach the client untouched.
struct AnthropicStreamMeter {
    budget: Arc<crate::budget::BudgetGuard>,
    api_key: String,
    model: String,
    usage: crate::sse::UsageScanner,
    prompt_tokens_est: u64,
    /// Released once the stream has been charged. The stream outlives the
    /// handler, so the hold has to travel with it.
    _reservation: Option<crate::budget::SpendReservation>,
}

impl AnthropicStreamMeter {
    fn on_chunk(&mut self, bytes: &[u8]) {
        self.usage.feed(bytes);
    }
}

impl Drop for AnthropicStreamMeter {
    fn drop(&mut self) {
        let (usage, measured) = match self.usage.usage() {
            Some(u) => (u, true),
            None => {
                // Anthropic reveals its input tokens in `message_start`, so even a
                // stream the client abandoned mid-answer tells us the real prompt
                // cost. Use it; guess only the part we could not observe.
                let partial = self.usage.partial();
                (
                    crate::sse::Usage {
                        prompt_tokens: partial
                            .map(|u| u.prompt_tokens)
                            .filter(|&t| t > 0)
                            .unwrap_or(self.prompt_tokens_est),
                        completion_tokens: partial
                            .map(|u| u.completion_tokens)
                            .unwrap_or(0)
                            .max(self.usage.frames()),
                    },
                    false,
                )
            }
        };
        let cost = crate::cost::global()
            .calculate(&self.model, Some(&usage.to_openai_json()))
            .cost_usd;
        if measured {
            self.budget.record_spend(&self.api_key, cost);
            tracing::info!(
                "/v1/messages stream billed: model={} tokens={}+{} cost=${:.6}",
                self.model, usage.prompt_tokens, usage.completion_tokens, cost
            );
        } else {
            self.budget.record_spend_estimated(&self.api_key, cost);
            tracing::warn!(
                "/v1/messages stream billed from an ESTIMATE: model={} reported no usage; \
                 charged ${:.6}. The cap is working from a guess for this key.",
                self.model, cost
            );
        }
    }
}

/// Streaming: the SSE bytes reach the client unchanged, while a passive tap reads
/// the usage Anthropic reports and charges the key when the stream ends.
async fn forward_stream(
    state: &AppState,
    api_key: &str,
    provider: &ProviderConfig,
    model: &str,
    req: &Value,
    reservation: Option<crate::budget::SpendReservation>,
) -> Response {
    let url = anthropic_url(provider);
    match anthropic_request(provider, &url, req).send().await {
        Ok(resp) if resp.status().is_success() => {
            // A free-tier Anthropic-compatible upstream (someone's local proxy)
            // costs nothing per token; do not pretend to bill it.
            let mut reservation = reservation;
            let mut meter = (!crate::cost::is_free_tier(&provider.tier)).then(|| AnthropicStreamMeter {
                budget: state.budget.clone(),
                api_key: api_key.to_string(),
                model: model.to_string(),
                usage: crate::sse::UsageScanner::new(crate::sse::Wire::Anthropic),
                prompt_tokens_est: (extract_prompt_text(req).len() / 4) as u64,
                _reservation: reservation.take(),
            });
            let stream = resp.bytes_stream().map(move |chunk| {
                if let (Ok(bytes), Some(m)) = (&chunk, meter.as_mut()) {
                    m.on_chunk(bytes);
                }
                chunk
            });
            Response::builder()
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .body(Body::from_stream(stream))
                .unwrap()
        }
        Ok(resp) => {
            let code = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            let text = resp.text().await.unwrap_or_default();
            (code, text).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("Anthropic stream failed: {}", e)).into_response(),
    }
}

fn anthropic_url(provider: &ProviderConfig) -> String {
    // base_url is the API root (e.g. https://api.anthropic.com); the Messages
    // path is always /v1/messages. Tolerate a base that already includes /v1.
    let base = provider.base_url.trim_end_matches('/').trim_end_matches("/v1");
    format!("{}/v1/messages", base)
}

fn anthropic_request(
    provider: &ProviderConfig,
    url: &str,
    body: &Value,
) -> reqwest::RequestBuilder {
    (&*SHARED_CLIENT)
        .post(url)
        .header("x-api-key", provider.resolve_api_key())
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(body)
}

/// Best-effort prompt text for loop detection. Anthropic content may be a
/// string or an array of typed blocks; pull text out of both, plus `system`.
pub fn extract_prompt_text(req: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(sys) = req.get("system") {
        push_content_text(sys, &mut parts);
    }
    if let Some(msgs) = req.get("messages").and_then(|m| m.as_array()) {
        for m in msgs {
            if let Some(c) = m.get("content") {
                push_content_text(c, &mut parts);
            }
        }
    }
    parts.join("\n")
}

fn push_content_text(content: &Value, out: &mut Vec<String>) {
    match content {
        Value::String(s) => out.push(s.clone()),
        Value::Array(blocks) => {
            for b in blocks {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    out.push(t.to_string());
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_string_and_block_content_and_system() {
        let req = json!({
            "model": "fixture-model",
            "system": "you are terse",
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "hi"},
                    {"type": "tool_use", "id": "t1", "name": "read", "input": {}}
                ]}
            ]
        });
        let text = extract_prompt_text(&req);
        assert!(text.contains("you are terse"));
        assert!(text.contains("hello"));
        assert!(text.contains("hi"));
    }

    #[test]
    fn url_appends_v1_messages_once() {
        let mut p = ProviderConfig {
            name: "anthropic".into(),
            r#type: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            api_key: String::new(),
            api_key_env: String::new(),
            models: vec![],
            tier: "cloud".into(),
        };
        assert_eq!(anthropic_url(&p), "https://api.anthropic.com/v1/messages");
        p.base_url = "https://api.anthropic.com/v1".into();
        assert_eq!(anthropic_url(&p), "https://api.anthropic.com/v1/messages");
    }
}
