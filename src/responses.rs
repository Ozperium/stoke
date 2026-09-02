//! Native OpenAI Responses API endpoint (`/v1/responses`).

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
use std::time::Instant;

use crate::config::ProviderConfig;
use crate::router::SHARED_CLIENT;
use crate::AppState;

pub async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let api_key = match state.auth.validate(
        headers
            .get("authorization")
            .and_then(|header| header.to_str().ok()),
    ) {
        Some(key) => key,
        None => return (StatusCode::UNAUTHORIZED, "Invalid or missing API key").into_response(),
    };
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if model.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "Responses request requires a model",
        )
            .into_response();
    }
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let prompt_text = extract_prompt_text(&body);
    let prompt_hash = {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(model.as_bytes());
        hash.update(prompt_text.as_bytes());
        hex::encode(hash.finalize())
    };
    if let Err(reason) = state
        .budget
        .check_with_prompt(&api_key, &prompt_hash, &prompt_text)
        .await
    {
        return (StatusCode::TOO_MANY_REQUESTS, reason).into_response();
    }

    let provider = match state.config.provider_for_model(&model) {
        Some(provider) if provider.r#type != "anthropic" => provider,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "No OpenAI-compatible provider configured for model",
            )
                .into_response()
        }
    };
    if let Err(reason) = crate::cost::global().allows(&provider.tier, &model) {
        return (StatusCode::FORBIDDEN, reason).into_response();
    }

    let max_output_tokens = body
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(state.config.limits.assumed_max_output_tokens);
    let reservation = match state.budget.try_reserve(
        &api_key,
        crate::cost::global().max_cost(&model, (prompt_text.len() / 4) as u64, max_output_tokens),
    ) {
        Ok(reservation) => reservation,
        Err(reason) => return (StatusCode::TOO_MANY_REQUESTS, reason).into_response(),
    };

    if stream {
        return forward_stream(
            &state,
            &api_key,
            provider,
            &model,
            &body,
            &headers,
            reservation,
        )
        .await;
    }
    let _hold = reservation;

    let started = Instant::now();
    let response = match openai_request(provider, &responses_url(provider), &body, &headers)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            crate::record_decision(
                &state,
                crate::dashboard::Outcome::Failed,
                &model,
                "responses",
                &provider.name,
                format!("Responses request failed: {error}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("Responses request failed: {error}"),
            )
                .into_response();
        }
    };
    let status = response.status();
    let response_body: Value = match response.json().await {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("Invalid Responses API response: {error}"),
            )
                .into_response();
        }
    };
    let usage = response_body.get("usage").map(|usage| {
        let input = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        serde_json::json!({
            "prompt_tokens": input,
            "completion_tokens": output,
            "total_tokens": input + output,
        })
    });
    let cost_usd = crate::cost::global()
        .calculate(&model, usage.as_ref())
        .cost_usd;
    state.budget.record_spend(&api_key, cost_usd);
    crate::record_decision(
        &state,
        if status.is_success() {
            crate::dashboard::Outcome::Allowed
        } else {
            crate::dashboard::Outcome::Failed
        },
        &model,
        "responses",
        &provider.name,
        if status.is_success() {
            "Responses API response"
        } else {
            "Responses API upstream rejected the request"
        },
        cost_usd,
        started.elapsed().as_millis() as u64,
    );

    let mut output = Json(response_body).into_response();
    *output.status_mut() = status;
    if let Ok(value) = format!("{cost_usd:.6}").parse() {
        output.headers_mut().insert("x-stoke-cost", value);
    }
    if let Ok(value) = provider.name.parse() {
        output.headers_mut().insert("x-stoke-node", value);
    }
    output
}

struct ResponsesStreamMeter {
    budget: Arc<crate::budget::BudgetGuard>,
    api_key: String,
    model: String,
    usage: crate::sse::UsageScanner,
    prompt_tokens_est: u64,
    _reservation: Option<crate::budget::SpendReservation>,
}

impl Drop for ResponsesStreamMeter {
    fn drop(&mut self) {
        let (usage, measured) = match self.usage.usage() {
            Some(usage) => (usage, true),
            None => (
                crate::sse::Usage {
                    prompt_tokens: self.prompt_tokens_est,
                    completion_tokens: self.usage.frames(),
                },
                false,
            ),
        };
        let cost = crate::cost::global()
            .calculate(&self.model, Some(&usage.to_openai_json()))
            .cost_usd;
        if measured {
            self.budget.record_spend(&self.api_key, cost);
        } else {
            self.budget.record_spend_estimated(&self.api_key, cost);
        }
    }
}

async fn forward_stream(
    state: &AppState,
    api_key: &str,
    provider: &ProviderConfig,
    model: &str,
    body: &Value,
    headers: &HeaderMap,
    reservation: Option<crate::budget::SpendReservation>,
) -> Response {
    let started = Instant::now();
    match openai_request(provider, &responses_url(provider), body, headers)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Allowed,
                model,
                "responses",
                &provider.name,
                "Responses API stream opened",
                0.0,
                started.elapsed().as_millis() as u64,
            );
            let mut reservation = reservation;
            let mut meter =
                (!crate::cost::is_free_tier(&provider.tier)).then(|| ResponsesStreamMeter {
                    budget: state.budget.clone(),
                    api_key: api_key.to_string(),
                    model: model.to_string(),
                    usage: crate::sse::UsageScanner::new(crate::sse::Wire::Responses),
                    prompt_tokens_est: (extract_prompt_text(body).len() / 4) as u64,
                    _reservation: reservation.take(),
                });
            let stream = response.bytes_stream().map(move |chunk| {
                if let (Ok(bytes), Some(meter)) = (&chunk, meter.as_mut()) {
                    meter.usage.feed(bytes);
                }
                chunk
            });
            Response::builder()
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(Body::from_stream(stream))
                .unwrap()
        }
        Ok(response) => {
            let status =
                StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let text = response.text().await.unwrap_or_default();
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "responses",
                &provider.name,
                format!("Responses API upstream returned {status}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            (status, text).into_response()
        }
        Err(error) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "responses",
                &provider.name,
                format!("Responses API stream failed: {error}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            (
                StatusCode::BAD_GATEWAY,
                format!("Responses API stream failed: {error}"),
            )
                .into_response()
        }
    }
}

fn openai_request(
    provider: &ProviderConfig,
    url: &str,
    body: &Value,
    inbound: &HeaderMap,
) -> reqwest::RequestBuilder {
    let mut request = (&*SHARED_CLIENT)
        .post(url)
        .bearer_auth(provider.resolve_api_key())
        .header("content-type", "application/json")
        .json(body);

    for (name, value) in inbound {
        let name = name.as_str();
        if name.starts_with("openai-")
            || name.starts_with("x-openai-")
            || name.starts_with("x-codex-")
            || name == "originator"
        {
            request = request.header(name, value);
        }
    }
    request
}

fn responses_url(provider: &ProviderConfig) -> String {
    let base = provider.base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/responses")
    } else {
        format!("{base}/v1/responses")
    }
}

fn extract_prompt_text(body: &Value) -> String {
    fn collect(value: &Value, parts: &mut Vec<String>) {
        match value {
            Value::String(text) => parts.push(text.clone()),
            Value::Array(items) => {
                for item in items {
                    collect(item, parts);
                }
            }
            Value::Object(object) => {
                for key in ["text", "content", "output"] {
                    if let Some(value) = object.get(key) {
                        collect(value, parts);
                    }
                }
            }
            _ => {}
        }
    }

    let mut parts = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        parts.push(instructions.to_string());
    }
    if let Some(input) = body.get("input") {
        collect(input, &mut parts);
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use serde_json::json;

    fn provider(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            name: "openai".into(),
            r#type: "openai".into(),
            base_url: base_url.into(),
            api_key: "upstream-key".into(),
            api_key_env: String::new(),
            models: vec!["gpt-test".into()],
            tier: "cloud".into(),
        }
    }

    #[test]
    fn url_appends_v1_responses_once() {
        assert_eq!(
            responses_url(&provider("https://api.openai.com")),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            responses_url(&provider("https://api.openai.com/v1")),
            "https://api.openai.com/v1/responses"
        );
    }

    #[test]
    fn codex_headers_reach_openai_without_client_authorization() {
        let mut inbound = HeaderMap::new();
        inbound.insert("authorization", "Bearer stoke-client-key".parse().unwrap());
        inbound.insert("openai-beta", "responses=experimental".parse().unwrap());
        inbound.insert("originator", "codex_cli_rs".parse().unwrap());
        inbound.insert("x-codex-turn-metadata", "turn-1".parse().unwrap());

        let request = openai_request(
            &provider("https://api.openai.com"),
            "https://api.openai.com/v1/responses",
            &json!({"model": "gpt-test", "input": "hello"}),
            &inbound,
        )
        .build()
        .unwrap();

        assert_eq!(request.headers()["authorization"], "Bearer upstream-key");
        assert_eq!(request.headers()["openai-beta"], "responses=experimental");
        assert_eq!(request.headers()["originator"], "codex_cli_rs");
        assert_eq!(request.headers()["x-codex-turn-metadata"], "turn-1");
    }

    #[test]
    fn extracts_codex_instructions_and_input_text_for_loop_detection() {
        let body = json!({
            "instructions": "You are a coding agent",
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "Fix the parser"}]
            }]
        });
        assert_eq!(
            extract_prompt_text(&body),
            "You are a coding agent\nFix the parser"
        );
    }
}
