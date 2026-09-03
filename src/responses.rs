//! Native OpenAI Responses API endpoint (`/v1/responses`).

use axum::{
    body::Body,
    extract::State,
    http::{header::CONTENT_TYPE, HeaderMap, HeaderValue, StatusCode},
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

/// Stoke gateway auth for this endpoint, split from the upstream credential.
///
/// The inbound `x-stoke-key` header is the gateway identity and is
/// authoritative when present; `Authorization` carries the *provider*
/// credential (e.g. the client's ChatGPT OAuth token) and is only consulted as
/// legacy Stoke auth when `x-stoke-key` is absent. Header values are never
/// logged or echoed — only the validated key name flows back to the budget
/// meter.
pub fn validate_gateway_headers(auth: &crate::budget::Auth, headers: &HeaderMap) -> Option<String> {
    let stoke_key = headers
        .get("x-stoke-key")
        .or_else(|| headers.get("x-api-key"))
        .and_then(|header| header.to_str().ok());
    let authorization = headers
        .get("authorization")
        .and_then(|header| header.to_str().ok());
    auth.validate_gateway(stoke_key, authorization)
}

/// Subscription admission bypass, decided once for the whole request.
///
/// A `codex_subscription` provider bills through the operator's flat ChatGPT
/// plan, so the dollar machinery does not apply to it: no pricing admission,
/// no spend reservation, no `record_spend`/`record_spend_estimated`, no stream
/// meter, and no `x-stoke-cost` response header. Everything else — auth,
/// budget check, rate limits, loop detection, OAuth destination pinning —
/// still runs exactly as for regular providers.
///
/// This is an explicit handler-level bypass, deliberately NOT expressed via
/// `cost::is_free_tier`: a subscription is not owned hardware, and classing it
/// as free would let stream_fusion treat subscription traffic as local.
pub fn subscription_bypasses_spend_accounting(provider: &ProviderConfig) -> bool {
    provider.is_subscription()
}

/// Whether a successful response for this provider carries `x-stoke-cost`.
///
/// Subscription responses bill zero API dollars, so reporting a dollar figure
/// would be a lie; regular responses keep the header.
pub fn response_emits_cost_header(provider: &ProviderConfig) -> bool {
    !subscription_bypasses_spend_accounting(provider)
}

/// The billing-mode header on successful subscription responses. Absent for
/// regular providers, which are metered in dollars and say so via
/// `x-stoke-cost`.
pub const BILLING_MODE_HEADER: &str = "x-stoke-billing-mode";
pub const SUBSCRIPTION_BILLING_MODE: &str = "chatgpt_subscription";

/// Record a dashboard decision for a finished `/v1/responses` request.
///
/// `DashboardEvent.cost_usd` is a mandatory f64 that the dashboard renders as
/// dollar spend. Subscription traffic bills through the flat plan — its only
/// true cost figure would be $0.00, which must never be presented as API
/// spend — so until the event schema can express a non-dollar billing mode,
/// subscription requests get a zero-marginal receipt (request-count metadata)
/// and no dollar-valued decision event at all.
fn record_metered_decision(
    dashboard: &crate::dashboard::Dashboard,
    provider: &ProviderConfig,
    outcome: crate::dashboard::Outcome,
    model: &str,
    reason: impl Into<String>,
    cost_usd: f64,
    elapsed_ms: u64,
) {
    if subscription_bypasses_spend_accounting(provider) {
        return;
    }
    // Same event `crate::record_decision` would emit for this endpoint; the
    // dashboard handle is passed directly so the skip-decision branch above
    // is testable without a full AppState.
    dashboard.record(crate::dashboard::EventInput {
        outcome,
        model: model.to_string(),
        route: "responses".to_string(),
        provider: provider.name.clone(),
        reason: reason.into(),
        cost_usd,
        elapsed_ms,
    });
}

pub async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let api_key = match validate_gateway_headers(&state.auth, &headers) {
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

    let provider = match responses_provider_for_model(&state.config.providers, &model) {
        Some(provider) => provider,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "No OpenAI-compatible provider configured for model",
            )
                .into_response()
        }
    };
    // Price admission only gates metered dollar spend. A subscription provider
    // bills through the flat ChatGPT plan, so it is admitted without a price.
    if !subscription_bypasses_spend_accounting(provider) {
        if let Err(reason) = crate::cost::global().allows(&provider.tier, &model) {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
    }

    let max_output_tokens = body
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(state.config.limits.assumed_max_output_tokens);
    let reservation = if subscription_bypasses_spend_accounting(provider) {
        None
    } else {
        match state.budget.try_reserve(
            &api_key,
            crate::cost::global().max_cost(
                &model,
                (prompt_text.len() / 4) as u64,
                max_output_tokens,
            ),
        ) {
            Ok(reservation) => reservation,
            Err(reason) => return (StatusCode::TOO_MANY_REQUESTS, reason).into_response(),
        }
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
    let url = match dispatch_url(provider) {
        Ok(url) => url,
        Err(reason) => return (StatusCode::FORBIDDEN, reason).into_response(),
    };
    let request = if provider.is_subscription() {
        if let Err(reason) = crate::subscription::validate_oauth_destination(&url, "chatgpt.com") {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
        subscription_request(provider, &url, &body, &headers)
    } else {
        openai_request(provider, &url, &body, &headers)
    };
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Failed,
                &model,
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
    // Dollar accounting only applies to metered providers. Subscription
    // traffic bills through the flat plan — no spend recorded, no dollar
    // header; it announces its billing mode instead.
    let cost_usd = if subscription_bypasses_spend_accounting(provider) {
        0.0
    } else {
        let cost = crate::cost::global()
            .calculate(&model, usage.as_ref())
            .cost_usd;
        state.budget.record_spend(&api_key, cost);
        cost
    };
    state
        .budget
        .record_receipt(subscription_bypasses_spend_accounting(provider), cost_usd);
    record_metered_decision(
        &state.dashboard,
        provider,
        if status.is_success() {
            crate::dashboard::Outcome::Allowed
        } else {
            crate::dashboard::Outcome::Failed
        },
        &model,
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
    if response_emits_cost_header(provider) {
        if let Ok(value) = format!("{cost_usd:.6}").parse() {
            output.headers_mut().insert("x-stoke-cost", value);
        }
    } else if status.is_success() {
        if let Ok(value) = SUBSCRIPTION_BILLING_MODE.parse() {
            output.headers_mut().insert(BILLING_MODE_HEADER, value);
        }
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
    let url = match dispatch_url(provider) {
        Ok(url) => url,
        Err(reason) => return (StatusCode::FORBIDDEN, reason).into_response(),
    };
    if provider.is_subscription() {
        if let Err(reason) = crate::subscription::validate_oauth_destination(&url, "chatgpt.com") {
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Failed,
                model,
                format!("Responses stream refused: {reason}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
    }
    let request = if provider.is_subscription() {
        subscription_request(provider, &url, body, headers)
    } else {
        openai_request(provider, &url, body, headers)
    };
    match request.send().await {
        Ok(response) if response.status().is_success() => {
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Allowed,
                model,
                "Responses API stream opened",
                0.0,
                started.elapsed().as_millis() as u64,
            );
            let mut reservation = reservation;
            // Subscription streams bill through the flat plan: no meter, no
            // reservation hold, no record_spend. The old `is_free_tier` gate
            // here is exactly what let stream_fusion race subscription as
            // local, so the bypass is stated explicitly instead.
            let mut meter =
                (!subscription_bypasses_spend_accounting(provider)).then(|| ResponsesStreamMeter {
                    budget: state.budget.clone(),
                    api_key: api_key.to_string(),
                    model: model.to_string(),
                    usage: crate::sse::UsageScanner::new(crate::sse::Wire::Responses),
                    prompt_tokens_est: (extract_prompt_text(body).len() / 4) as u64,
                    _reservation: reservation.take(),
                });
            let upstream_content_type = response.headers().get(CONTENT_TYPE).cloned();
            let stream = response.bytes_stream().map(move |chunk| {
                if let (Ok(bytes), Some(meter)) = (&chunk, meter.as_mut()) {
                    meter.usage.feed(bytes);
                }
                chunk
            });
            Response::builder()
                .header(
                    "content-type",
                    stream_content_type(provider.is_subscription(), upstream_content_type.as_ref()),
                )
                .header("cache-control", "no-cache")
                .body(Body::from_stream(stream))
                .unwrap()
        }
        Ok(response) => {
            let status =
                StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let text = response.text().await.unwrap_or_default();
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Failed,
                model,
                format!("Responses API upstream returned {status}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            (status, text).into_response()
        }
        Err(error) => {
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Failed,
                model,
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

/// Build the upstream request for a regular (API-key) provider.
///
/// The client's own credential and account identity never travel: the
/// Authorization header is replaced with the configured provider key and the
/// client's ChatGPT account header is dropped. Codex/OpenAI feature metadata
/// (openai-*, x-openai-*, x-codex-*, originator/version) is preserved.
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
        if name == "authorization" || name == "chatgpt-account-id" || name == "x-stoke-key" {
            continue;
        }
        if name.starts_with("openai-")
            || name.starts_with("x-openai-")
            || name.starts_with("x-codex-")
            || name == "originator"
            || name == "version"
        {
            request = request.header(name, value);
        }
    }
    request
}

/// Build the upstream request for a `codex_subscription` provider.
///
/// Dispatched on `subscription::OAUTH_CLIENT` (redirects disabled) with the
/// client's own ChatGPT OAuth `Authorization` and `ChatGPT-Account-ID` passed
/// through unchanged, alongside the Codex feature metadata. The Stoke gateway
/// key (`x-stoke-key`) never leaves the building, and neither credential is
/// logged, persisted, hashed, or echoed.
fn subscription_request(
    provider: &ProviderConfig,
    url: &str,
    body: &Value,
    inbound: &HeaderMap,
) -> reqwest::RequestBuilder {
    debug_assert!(provider.is_subscription());
    let mut request = (&*crate::subscription::OAUTH_CLIENT)
        .post(url)
        .header("content-type", "application/json")
        .json(body);

    for (name, value) in inbound {
        let name = name.as_str();
        if name == "x-stoke-key" {
            continue;
        }
        if name == "authorization"
            || name == "chatgpt-account-id"
            || name.starts_with("openai-")
            || name.starts_with("x-openai-")
            || name.starts_with("x-codex-")
            || name == "originator"
            || name == "version"
        {
            request = request.header(name, value);
        }
    }
    request
}

/// Select a Responses provider without pinning Stoke to a model catalog.
/// Explicit model declarations always win. A live-discovered Codex model may
/// be newer than the static config, so unclaimed GPT/Codex namespace IDs route
/// to codex_subscription. Everything else keeps the existing first-provider
/// fallback, excluding Anthropic-only providers.
fn responses_provider_for_model<'a>(
    providers: &'a [ProviderConfig],
    model: &str,
) -> Option<&'a ProviderConfig> {
    providers
        .iter()
        .find(|p| p.r#type != "anthropic" && p.models.iter().any(|m| m == model))
        .or_else(|| {
            if model.starts_with("gpt-") || model.starts_with("codex-") {
                providers.iter().find(|p| p.r#type == "codex_subscription")
            } else {
                None
            }
        })
        .or_else(|| providers.iter().find(|p| p.r#type != "anthropic"))
}

/// A `codex_subscription` provider may only target the exact ChatGPT Codex
/// backend; anything else is refused. Regular providers keep the generic
/// base-url logic.
fn dispatch_url(provider: &ProviderConfig) -> Result<String, String> {
    if provider.is_subscription() {
        crate::subscription::subscription_responses_endpoint(&provider.base_url)
    } else {
        Ok(responses_url(provider))
    }
}

/// The content-type for a proxied SSE stream. Subscription passthrough keeps
/// the upstream's own content type; regular providers keep the historical
/// fixed `text/event-stream`.
fn stream_content_type(is_subscription: bool, upstream: Option<&HeaderValue>) -> HeaderValue {
    if is_subscription {
        upstream
            .filter(|value| value.to_str().is_ok())
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("text/event-stream"))
    } else {
        HeaderValue::from_static("text/event-stream")
    }
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
    use axum::http::{HeaderMap, HeaderValue};
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

    fn subscription_provider() -> ProviderConfig {
        ProviderConfig {
            name: "chatgpt-subscription".into(),
            r#type: "codex_subscription".into(),
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            api_key: String::new(),
            api_key_env: String::new(),
            models: vec!["gpt-test".into()],
            tier: "subscription".into(),
        }
    }

    #[test]
    fn live_codex_models_route_to_subscription_but_explicit_models_win() {
        let local = provider("http://127.0.0.1:11434/v1");
        let subscription = subscription_provider();
        let providers = vec![local, subscription];

        assert_eq!(
            responses_provider_for_model(&providers, "gpt-future")
                .unwrap()
                .r#type,
            "codex_subscription"
        );
        assert_eq!(
            responses_provider_for_model(&providers, "codex-auto-review")
                .unwrap()
                .r#type,
            "codex_subscription"
        );
        assert_eq!(
            responses_provider_for_model(&providers, "gpt-test")
                .unwrap()
                .name,
            "openai"
        );
    }

    #[test]
    fn subscription_request_keeps_chatgpt_identity_and_never_leaks_gateway_key() {
        let mut inbound = HeaderMap::new();
        inbound.insert(
            "authorization",
            "Bearer chatgpt-oauth-token".parse().unwrap(),
        );
        inbound.insert("chatgpt-account-id", "acct-1".parse().unwrap());
        inbound.insert("x-stoke-key", "stk-gateway-key".parse().unwrap());
        inbound.insert("openai-beta", "responses=experimental".parse().unwrap());
        inbound.insert("x-openai-trace", "trace-1".parse().unwrap());
        inbound.insert("x-codex-turn-metadata", "turn-1".parse().unwrap());
        inbound.insert("originator", "codex_desktop".parse().unwrap());
        inbound.insert("version", "1.2.3".parse().unwrap());

        let request = subscription_request(
            &subscription_provider(),
            "https://chatgpt.com/backend-api/codex/responses",
            &json!({"model": "gpt-test", "input": "hello"}),
            &inbound,
        )
        .build()
        .unwrap();

        let headers = request.headers();
        assert_eq!(headers["authorization"], "Bearer chatgpt-oauth-token");
        assert_eq!(headers["chatgpt-account-id"], "acct-1");
        assert_eq!(headers["originator"], "codex_desktop");
        assert_eq!(headers["version"], "1.2.3");
        assert_eq!(headers["openai-beta"], "responses=experimental");
        assert_eq!(headers["x-openai-trace"], "trace-1");
        assert_eq!(headers["x-codex-turn-metadata"], "turn-1");
        assert!(
            headers.get("x-stoke-key").is_none(),
            "the gateway key must never reach ChatGPT"
        );
        assert!(
            !headers
                .iter()
                .any(|(name, value)| name.as_str() != "authorization"
                    && value
                        .to_str()
                        .map(|v| v.contains("stk-gateway-key"))
                        .unwrap_or(false)),
            "the gateway key must not ride along under any other header name"
        );
    }

    #[test]
    fn subscription_dispatch_url_is_exact_or_refused() {
        assert_eq!(
            dispatch_url(&subscription_provider()).unwrap(),
            "https://chatgpt.com/backend-api/codex/responses"
        );

        let mut hostile = subscription_provider();
        for base in [
            "https://api.openai.com/v1",
            "https://chatgpt.com/backend-api/codex.evil.test",
            "https://chatgpt.com.evil.test/backend-api/codex",
        ] {
            hostile.base_url = base.into();
            assert!(dispatch_url(&hostile).is_err(), "must refuse {base}");
        }

        // Non-subscription providers keep the generic URL logic.
        assert_eq!(
            dispatch_url(&provider("https://api.openai.com/v1")).unwrap(),
            "https://api.openai.com/v1/responses"
        );
    }

    #[test]
    fn subscription_stream_preserves_the_upstream_content_type() {
        let upstream = HeaderValue::from_static("text/event-stream; charset=utf-8");
        assert_eq!(
            stream_content_type(true, Some(&upstream)),
            HeaderValue::from_static("text/event-stream; charset=utf-8")
        );
        assert_eq!(
            stream_content_type(true, None),
            HeaderValue::from_static("text/event-stream")
        );
        // Regular providers keep the historical fixed value, upstream or not.
        let other = HeaderValue::from_static("application/x-anything");
        assert_eq!(
            stream_content_type(false, Some(&other)),
            HeaderValue::from_static("text/event-stream")
        );
    }

    #[test]
    fn regular_provider_strips_client_auth_account_and_gateway_key() {
        let mut inbound = HeaderMap::new();
        inbound.insert("authorization", "Bearer stoke-client-key".parse().unwrap());
        inbound.insert("x-stoke-key", "stk-gateway-key".parse().unwrap());
        inbound.insert("chatgpt-account-id", "acct-1".parse().unwrap());

        let request = openai_request(
            &provider("https://api.openai.com"),
            "https://api.openai.com/v1/responses",
            &json!({"model": "gpt-test"}),
            &inbound,
        )
        .build()
        .unwrap();

        assert_eq!(request.headers()["authorization"], "Bearer upstream-key");
        assert!(request.headers().get("chatgpt-account-id").is_none());
        assert!(request.headers().get("x-stoke-key").is_none());
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

    fn auth_with_keys(keys: &str) -> crate::budget::Auth {
        // No process-global env mutation: `Auth::with_keys` seeds the private
        // key lock directly, so this cannot race concurrently running tests.
        crate::budget::Auth::with_keys(
            &keys
                .split(',')
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn gateway_auth_reads_x_stoke_key_not_an_unrelated_authorization() {
        let auth = auth_with_keys("gateway-key");

        // A valid x-stoke-key is selected even though Authorization carries an
        // unrelated upstream credential (e.g. a ChatGPT OAuth token).
        let mut headers = HeaderMap::new();
        headers.insert("x-stoke-key", HeaderValue::from_static("gateway-key"));
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer upstream-subscription-token"),
        );
        assert_eq!(
            validate_gateway_headers(&auth, &headers).as_deref(),
            Some("gateway-key"),
            "the Stoke key must be selected over an unrelated Authorization"
        );
    }

    #[test]
    fn missing_or_invalid_x_stoke_key_never_falls_back_to_authorization() {
        let auth = auth_with_keys("gateway-key");

        // No x-stoke-key at all: the legacy Authorization path is consulted.
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer gateway-key"),
        );
        assert_eq!(
            validate_gateway_headers(&auth, &headers).as_deref(),
            Some("gateway-key")
        );

        // Missing x-stoke-key where Authorization is NOT a gateway key: reject.
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer upstream-subscription-token"),
        );
        assert!(
            validate_gateway_headers(&auth, &headers).is_none(),
            "an upstream credential must not authenticate as Stoke"
        );

        // An invalid x-stoke-key must never fall back to a valid Authorization.
        let mut headers = HeaderMap::new();
        headers.insert("x-stoke-key", HeaderValue::from_static("wrong-key"));
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer gateway-key"),
        );
        assert!(
            validate_gateway_headers(&auth, &headers).is_none(),
            "an explicit but invalid x-stoke-key must not fall back to Authorization"
        );
    }

    #[test]
    fn subscription_admission_bypasses_only_the_spend_machinery() {
        let sub = subscription_provider();
        let regular = provider("https://api.openai.com");

        assert!(subscription_bypasses_spend_accounting(&sub));
        assert!(!subscription_bypasses_spend_accounting(&regular));

        // The bypass must come from the provider kind, NOT the free-tier
        // shortcut — otherwise stream_fusion races subscription as local.
        assert!(!crate::cost::is_free_tier(&sub.tier));
        assert!(regular.tier != sub.tier || !crate::cost::is_free_tier(&regular.tier));

        // Unpriced models are refused on metered tiers but admitted on a
        // subscription provider, whose admission skips the pricing gate.
        assert!(crate::cost::global()
            .allows(&regular.tier, "unpriced-model")
            .is_err());
        assert!(
            crate::cost::global()
                .allows(&sub.tier, "unpriced-model")
                .is_err(),
            "the pricing gate itself stays strict; the bypass is handler-level"
        );
    }

    #[test]
    fn response_cost_header_policy_omits_it_for_subscription() {
        let sub = subscription_provider();
        let regular = provider("https://api.openai.com");

        assert!(
            !response_emits_cost_header(&sub),
            "a subscription bills no API dollars, so x-stoke-cost would be a lie"
        );
        assert!(response_emits_cost_header(&regular));
        assert_eq!(BILLING_MODE_HEADER, "x-stoke-billing-mode");
        assert_eq!(SUBSCRIPTION_BILLING_MODE, "chatgpt_subscription");
    }

    #[test]
    fn subscription_never_enters_the_dollar_decision_path() {
        let sub = subscription_provider();
        let regular = provider("https://api.openai.com");
        let dashboard = crate::dashboard::Dashboard::new(16);

        // Regular providers produce a dollar decision event; a subscription
        // must not — even on failure reasons that would carry only $0.00.
        for outcome in [
            crate::dashboard::Outcome::Allowed,
            crate::dashboard::Outcome::Failed,
        ] {
            assert!(
                metered_decision_for_test(&dashboard, &sub, outcome).is_none(),
                "subscription traffic must not be recorded as $0.00 API spend"
            );
            assert!(metered_decision_for_test(&dashboard, &regular, outcome).is_some());
        }

        let regular_event =
            metered_decision_for_test(&dashboard, &regular, crate::dashboard::Outcome::Allowed)
                .unwrap();
        assert_eq!(regular_event.route, "responses");
        assert_eq!(regular_event.provider, "openai");
        assert_eq!(regular_event.cost_usd, 0.25);
    }

    fn metered_decision_for_test(
        dashboard: &crate::dashboard::Dashboard,
        provider: &ProviderConfig,
        outcome: crate::dashboard::Outcome,
    ) -> Option<crate::dashboard::DashboardEvent> {
        // Drive the helper with a real Dashboard so the assertion covers the
        // full path, not just which branch the helper takes. Only events added
        // by THIS call count: the dashboard accumulates across calls.
        let before = dashboard.snapshot().len();
        record_metered_decision(
            dashboard,
            provider,
            outcome,
            "gpt-test",
            format!("reason for {outcome:?}"),
            0.25,
            12,
        );
        let snapshot = dashboard.snapshot();
        assert_eq!(
            snapshot.len(),
            before + usize::from(!subscription_bypasses_spend_accounting(provider)),
            "regular providers record a decision; subscription does not"
        );
        snapshot.get(before).cloned()
    }
}
