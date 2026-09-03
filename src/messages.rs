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
    http::{header::CONTENT_TYPE, HeaderMap, StatusCode},
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

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The beta header Anthropic requires on OAuth subscription requests. Always
/// present upstream; the client's own betas are preserved alongside it.
const SUBSCRIPTION_OAUTH_BETA: &str = "oauth-2025-04-20";

/// Stoke gateway auth for this endpoint, split from the upstream credential.
///
/// The inbound `x-stoke-key` header is the gateway identity and is
/// authoritative when present; `Authorization` carries the *provider*
/// credential (e.g. the client's Claude OAuth token) and is only consulted as
/// legacy Stoke auth when `x-stoke-key` is absent. Header values are never
/// logged or echoed — only the validated key name flows back to the budget
/// meter.
pub fn validate_gateway_headers(auth: &crate::budget::Auth, headers: &HeaderMap) -> Option<String> {
    let stoke_key = headers
        .get("x-stoke-key")
        .and_then(|header| header.to_str().ok());
    let authorization = headers
        .get("authorization")
        .and_then(|header| header.to_str().ok());
    auth.validate_gateway(stoke_key, authorization)
}

/// Subscription admission bypass, decided once for the whole request.
///
/// A `claude_subscription` provider bills through the operator's flat Claude
/// plan, so the dollar machinery does not apply to it: no pricing admission,
/// no spend reservation, no `record_spend`/`record_spend_estimated`, no stream
/// meter, no `x-stoke-cost` header, and no dollar-valued dashboard decision.
/// Everything else — auth, budget check, rate limits, loop detection, OAuth
/// destination pinning — still runs exactly as for regular providers.
///
/// This is an explicit handler-level bypass, deliberately NOT expressed via
/// `cost::is_free_tier` (which stays local|remote only): a subscription is not
/// owned hardware, and classing it as free would let stream_fusion treat
/// subscription traffic as local. Gate is on the provider type string so this
/// slice stays mergeable with the sibling branch that owns config.rs.
pub fn subscription_bypasses_spend_accounting(provider: &ProviderConfig) -> bool {
    provider.r#type == "claude_subscription"
}

/// The billing-mode header on successful subscription responses. Absent for
/// regular providers, which are metered in dollars and say so via
/// `x-stoke-cost`.
pub const BILLING_MODE_HEADER: &str = "x-stoke-billing-mode";
pub const CLAUDE_SUBSCRIPTION_BILLING_MODE: &str = "claude_subscription";

/// Record a dashboard decision for a finished `/v1/messages` request.
///
/// `DashboardEvent.cost_usd` is a mandatory f64 that the dashboard renders as
/// dollar spend. Subscription traffic bills through the flat plan — its only
/// true cost figure would be $0.00, which must never be presented as API
/// spend — so subscription requests get no dollar-valued decision event at
/// all.
/// Regular providers use OpenAI-compatible chat/completions, so their tier
/// strings come from config. An OpenAI-compatible local provider can serve
/// /v1/messages via translation; Anthropic-type and subscription providers
/// cannot and must not.
fn provider_accepts_messages_translation(provider: &ProviderConfig) -> bool {
    provider.r#type == "openai_compatible" && matches!(provider.tier.as_str(), "local" | "remote")
}

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
        route: "anthropic".to_string(),
        provider: provider.name.clone(),
        reason: reason.into(),
        cost_usd,
        elapsed_ms,
    });
}

fn message_outcome(status: StatusCode) -> crate::dashboard::Outcome {
    if status.is_success() {
        crate::dashboard::Outcome::Allowed
    } else {
        crate::dashboard::Outcome::Failed
    }
}

/// The credential a `claude_subscription` dispatch carries upstream.
///
/// Slice A (the Claude OAuth token store) will mint these; until it lands, the
/// handler passes the client's own `Authorization` through opaquely, and tests
/// construct the struct directly to inject a token. The token is never logged,
/// hashed, persisted, or echoed.
pub struct SubscriptionCredential {
    pub access_token: String,
}

impl SubscriptionCredential {
    /// Opaque passthrough: take the client's Bearer token as-is. No parsing
    /// beyond the scheme prefix, no validation, no storage.
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let value = headers.get("authorization")?.to_str().ok()?;
        let token = value
            .strip_prefix("Bearer ")
            .map(str::trim)
            .unwrap_or_else(|| value.trim());
        if token.is_empty() {
            return None;
        }
        Some(Self {
            access_token: token.to_string(),
        })
    }
}

/// How the subscription path obtains its upstream Bearer token.
///
/// Default is the Stoke-held Anthropic OAuth store (`~/.stoke/anthropic_oauth.json`,
/// refreshed by slice A); tests inject a closure returning a canned token so
/// the handler can be exercised without token material on disk. The resolved
/// token is never logged, hashed, persisted, or echoed.
/// How the subscription path obtains its upstream Bearer token.
///
/// Default is the Stoke-held Anthropic OAuth store (`~/.stoke/anthropic_oauth.json`,
/// refreshed by slice A); tests inject a closure returning a canned token so
/// the handler can be exercised without token material on disk. The resolved
/// token is never logged, hashed, persisted, or echoed.
pub type SubscriptionTokenResolver = std::sync::Arc<
    dyn Fn() -> futures_util::future::BoxFuture<'static, Result<String, String>> + Send + Sync,
>;

/// The production resolver: a shared, store-backed `TokenStore`. The store
/// caches tokens in memory and refreshes under one lock, so concurrent
/// requests produce one refresh, not a stampede against Anthropic.
pub fn default_subscription_token_resolver() -> SubscriptionTokenResolver {
    static STORE: once_cell::sync::Lazy<crate::anthropic_oauth::TokenStore> =
        once_cell::sync::Lazy::new(crate::anthropic_oauth::TokenStore::new);
    std::sync::Arc::new(
        || -> futures_util::future::BoxFuture<'static, Result<String, String>> {
            Box::pin(async { STORE.get_valid_access_token().await })
        },
    )
}

/// The credential a subscription dispatch resolves before forwarding.
///
/// `None` means "the store is not usable" (not logged in, malformed, or a
/// failed refresh) — the handler must fail closed with a clear error and
/// without forwarding anything.
struct SubscriptionCredentialResult(Result<SubscriptionCredential, String>);

async fn resolve_subscription_credential(
    resolver: &SubscriptionTokenResolver,
) -> SubscriptionCredentialResult {
    match resolver().await {
        Ok(access_token) if !access_token.is_empty() => {
            SubscriptionCredentialResult(Ok(SubscriptionCredential { access_token }))
        }
        Ok(_) => SubscriptionCredentialResult(Err(
            "claude_subscription credential is unavailable: the Anthropic OAuth store returned \
             an empty token"
                .to_string(),
        )),
        Err(reason) => SubscriptionCredentialResult(Err(format!(
            "claude_subscription credential is unavailable: {reason}"
        ))),
    }
}

/// Build the client-facing error for an unusable subscription credential:
/// fail-closed, no token material, no panic.
fn subscription_credential_error(reason: &str) -> Response {
    tracing::error!(
        "claude_subscription credential refused: {}",
        reason
            .split("token store returned")
            .next()
            .unwrap_or(reason)
            .trim()
    );
    let status = if reason.contains("not logged in") {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::BAD_GATEWAY
    };
    (status, reason.to_string()).into_response()
}

/// Union the client's `anthropic-beta` with the OAuth beta the official
/// subscription client must send, comma-separated, no duplicates (Anthropic's
/// documented joining rule). Client betas are preserved verbatim in order.
fn merge_subscription_beta(inbound: &HeaderMap) -> Option<String> {
    let client_beta = inbound
        .get("anthropic-beta")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    match client_beta {
        None => Some(SUBSCRIPTION_OAUTH_BETA.to_string()),
        Some(client) => {
            let mut parts: Vec<String> = client
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect();
            if !parts.iter().any(|p| p == SUBSCRIPTION_OAUTH_BETA) {
                parts.push(SUBSCRIPTION_OAUTH_BETA.to_string());
            }
            Some(parts.join(","))
        }
    }
}

/// The upstream URL for a `/v1/messages` dispatch.
///
/// A `claude_subscription` provider may only target the exact Anthropic API
/// root; anything else is refused. Regular providers keep the generic
/// base-url logic.
fn dispatch_url(provider: &ProviderConfig) -> Result<String, String> {
    if subscription_bypasses_spend_accounting(provider) {
        crate::subscription::claude_subscription_messages_endpoint(&provider.base_url)
    } else {
        Ok(anthropic_url(provider))
    }
}

pub async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    // Auth: split the gateway identity from the upstream credential. The
    // global middleware already gated this, but we re-validate to recover the
    // key string for per-key budget/loop accounting.
    let api_key = match validate_gateway_headers(&state.auth, &headers) {
        Some(k) => k,
        None => return (StatusCode::UNAUTHORIZED, "Invalid or missing API key").into_response(),
    };

    let model = req
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let stream = req.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    // Resolve the upstream Anthropic provider. A pure config read — every
    // enforcement gate below still runs before any upstream request.
    let provider = match state.config.providers.iter().find(|p| {
        p.r#type == "anthropic"
            || p.r#type == "claude_subscription"
            || provider_accepts_messages_translation(p)
    }) {
        Some(p) => p,
        None => {
            crate::record_decision(
                &state,
                crate::dashboard::Outcome::Blocked,
                &model,
                "anthropic",
                "",
                "No Anthropic provider configured",
                0.0,
                0,
            );
            return (
                StatusCode::BAD_REQUEST,
                "No Anthropic provider configured. Add a provider with type = \"anthropic\" \
                 and base_url = \"https://api.anthropic.com\" (api_key_env for the key), \
                 or a passthrough provider with type = \"claude_subscription\".",
            )
                .into_response();
        }
    };
    let subscription = subscription_bypasses_spend_accounting(provider);
    // The upstream credential for a subscription dispatch comes from the
    // Stoke-held OAuth store, NOT the client: the client authenticated to the
    // gateway with its Stoke key, and the operator's flat-plan token rides
    // only on the upstream request. Resolve once, before enforcement, so a
    // dead credential fails closed without spending anything downstream.
    let credential = if subscription {
        match resolve_subscription_credential(&state.subscription_token_resolver).await {
            SubscriptionCredentialResult(Ok(credential)) => Some(credential),
            SubscriptionCredentialResult(Err(reason)) => {
                record_metered_decision(
                    &state.dashboard,
                    provider,
                    crate::dashboard::Outcome::Blocked,
                    &model,
                    reason.clone(),
                    0.0,
                    0,
                );
                return subscription_credential_error(&reason);
            }
        }
    } else {
        None
    };

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
        if !subscription {
            crate::record_decision(
                &state,
                crate::dashboard::Outcome::Blocked,
                &model,
                "anthropic",
                "",
                reason.clone(),
                0.0,
                0,
            );
        }
        return (StatusCode::TOO_MANY_REQUESTS, reason).into_response();
    }

    // Price admission only gates metered dollar spend. A subscription provider
    // bills through the flat Claude plan, so it is admitted without a price.
    if !subscription {
        // The same dispatch gate router::call_provider_hop applies. This path does not
        // go through the router, so without it an Anthropic model with no configured
        // price would be forwarded and metered at $0 — the budget cap would never move
        // for the one client this endpoint exists to serve.
        if let Err(reason) = crate::cost::global().allows(&provider.tier, &model) {
            crate::record_decision(
                &state,
                crate::dashboard::Outcome::Blocked,
                &model,
                "anthropic",
                &provider.name,
                reason.clone(),
                0.0,
                0,
            );
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
    }

    // Hold the money this request could cost, so concurrent requests on the same
    // key are admitted against a figure that includes each other. Claude Code
    // always sends max_tokens, which makes the hold exact rather than assumed.
    // Subscription traffic bills no API dollars, so it holds nothing.
    let max_tokens = req
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(state.config.limits.assumed_max_output_tokens);
    let reservation = if subscription {
        None
    } else {
        match state.budget.try_reserve(
            &api_key,
            crate::cost::global().max_cost(&model, (prompt_text.len() / 4) as u64, max_tokens),
        ) {
            Ok(r) => Some(r),
            Err(reason) => {
                crate::record_decision(
                    &state,
                    crate::dashboard::Outcome::Blocked,
                    &model,
                    "anthropic",
                    &provider.name,
                    reason.clone(),
                    0.0,
                    0,
                );
                return (StatusCode::TOO_MANY_REQUESTS, reason).into_response();
            }
        }
    };

    if stream {
        forward_stream(
            &state,
            &api_key,
            provider,
            &model,
            &req,
            &headers,
            reservation.flatten(),
            credential.as_ref(),
        )
        .await
    } else {
        let _hold = reservation.flatten(); // released when this handler returns
        forward_once(
            &state,
            &api_key,
            provider,
            &model,
            &req,
            &headers,
            credential.as_ref(),
        )
        .await
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
    headers: &HeaderMap,
    credential: Option<&SubscriptionCredential>,
) -> Response {
    if subscription_bypasses_spend_accounting(provider) {
        return forward_once_subscription(state, provider, model, req, headers, credential).await;
    }
    // OpenAI-compatible local provider: translate Anthropic -> OpenAI, dispatch
    // to {base_url}/chat/completions, translate the response back. Unsupported
    // features fail closed with a clear 400 before any upstream request.
    if provider_accepts_messages_translation(provider) {
        return forward_once_openai(state, api_key, provider, model, req).await;
    }
    let url = anthropic_url(provider);
    let started = Instant::now();
    let resp = match anthropic_request(provider, &url, req, headers).send().await {
        Ok(r) => r,
        Err(e) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "anthropic",
                &provider.name,
                format!("Anthropic request failed: {e}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("Anthropic request failed: {}", e),
            )
                .into_response();
        }
    };
    let status = resp.status();
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "anthropic",
                &provider.name,
                format!("Invalid Anthropic response: {e}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("Invalid Anthropic response: {}", e),
            )
                .into_response();
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
            crate::cost::global()
                .calculate(model, Some(&usage))
                .cost_usd
        })
        .unwrap_or(0.0);
    state.budget.record_spend(api_key, cost_usd);
    crate::record_decision(
        state,
        message_outcome(status),
        model,
        "anthropic",
        &provider.name,
        if status.is_success() {
            "Anthropic response"
        } else {
            "Anthropic upstream rejected the request"
        },
        cost_usd,
        started.elapsed().as_millis() as u64,
    );
    tracing::info!(
        "/v1/messages: model={} provider={} cost=${:.6}",
        model,
        provider.name,
        cost_usd
    );

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

/// Non-streaming subscription passthrough: the upstream status, bytes, and
/// content-type reach the client verbatim — including non-JSON errors, which
/// must not be re-wrapped. No dollar accounting of any kind runs.
async fn forward_once_subscription(
    state: &AppState,
    provider: &ProviderConfig,
    model: &str,
    req: &Value,
    headers: &HeaderMap,
    credential: Option<&SubscriptionCredential>,
) -> Response {
    let started = Instant::now();
    // The credential was resolved (or failed closed) in the handler before
    // dispatch; an absent one here is a wiring bug, not a client problem.
    let credential = match credential {
        Some(credential) => credential,
        None => {
            return (
                StatusCode::BAD_GATEWAY,
                "claude_subscription credential was not resolved for this request",
            )
                .into_response()
        }
    };
    let url = match dispatch_url(provider) {
        Ok(url) => url,
        Err(reason) => return (StatusCode::FORBIDDEN, reason).into_response(),
    };
    if let Err(reason) =
        crate::subscription::validate_oauth_destination(&url, crate::subscription::CLAUDE_API_HOST)
    {
        return (StatusCode::FORBIDDEN, reason).into_response();
    }
    let resp = match subscription_request(&url, req, headers, credential)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Failed,
                model,
                format!("Anthropic request failed: {e}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("Anthropic request failed: {}", e),
            )
                .into_response();
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp.headers().get(CONTENT_TYPE).cloned();
    let body = resp.bytes().await.unwrap_or_default();
    record_metered_decision(
        &state.dashboard,
        provider,
        message_outcome(status),
        model,
        if status.is_success() {
            "Anthropic response"
        } else {
            "Anthropic upstream rejected the request"
        },
        0.0,
        started.elapsed().as_millis() as u64,
    );
    let mut out = verbatim_response(status, content_type, Body::from(body));
    if status.is_success() {
        if let Ok(v) = CLAUDE_SUBSCRIPTION_BILLING_MODE.parse() {
            out.headers_mut().insert(BILLING_MODE_HEADER, v);
        }
    }
    if let Ok(v) = provider.name.parse() {
        out.headers_mut().insert("x-stoke-node", v);
    }
    out
}

/// Non-streaming dispatch to an OpenAI-compatible local provider.
///
/// Translate Anthropic -> OpenAI, POST to `{base_url}/chat/completions` with
/// the provider's API key (Bearer, as the shared router does), then translate
/// the OpenAI response back into an Anthropic Messages body. Unsupported
/// request features fail closed with a clear 400 before any upstream call.
/// Dollar accounting runs exactly as the Anthropic path: usage-derived
/// `record_spend`, decision event, `x-stoke-cost`.
async fn forward_once_openai(
    state: &AppState,
    api_key: &str,
    provider: &ProviderConfig,
    model: &str,
    req: &Value,
) -> Response {
    let started = Instant::now();
    let openai_req = match crate::anthropic_translate::translate_request(req) {
        Ok(v) => v,
        Err(reason) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Blocked,
                model,
                "anthropic",
                &provider.name,
                reason.clone(),
                0.0,
                0,
            );
            return (StatusCode::BAD_REQUEST, reason).into_response();
        }
    };
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let resp = match (&*SHARED_CLIENT)
        .post(&url)
        .bearer_auth(provider.resolve_api_key())
        .json(&openai_req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "anthropic",
                &provider.name,
                format!("Provider request failed: {e}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("Provider request failed: {}", e),
            )
                .into_response();
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "anthropic",
                &provider.name,
                format!("Invalid provider response: {e}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("Invalid provider response: {}", e),
            )
                .into_response();
        }
    };
    // OpenAI usage -> the shared Pricer, identical to the Anthropic path's
    // accounting so translation never bypasses the meter.
    let cost_usd = crate::cost::global()
        .calculate(model, body.get("usage"))
        .cost_usd;
    state.budget.record_spend(api_key, cost_usd);
    crate::record_decision(
        state,
        message_outcome(status),
        model,
        "anthropic",
        &provider.name,
        if status.is_success() {
            "Provider response"
        } else {
            "Provider upstream rejected the request"
        },
        cost_usd,
        started.elapsed().as_millis() as u64,
    );
    tracing::info!(
        "/v1/messages (openai translation): model={} provider={} cost=${:.6}",
        model,
        provider.name,
        cost_usd
    );

    let out = match crate::anthropic_translate::translate_response(&body, model) {
        translated => Json(translated).into_response(),
    };
    let mut out = out;
    *out.status_mut() = status;
    if let Ok(v) = format!("{:.6}", cost_usd).parse() {
        out.headers_mut().insert("x-stoke-cost", v);
    }
    if let Ok(v) = provider.name.parse() {
        out.headers_mut().insert("x-stoke-node", v);
    }
    out
}

/// Streaming dispatch to an OpenAI-compatible local provider.
///
/// Consumes the upstream OpenAI SSE stream, translates each chunk into
/// Anthropic SSE events, and meters the translated stream with the regular
/// `AnthropicStreamMeter` tap so billing is identical to the Anthropic path.
/// Upstream errors reach the client with their original status.
async fn forward_stream_openai(
    state: &AppState,
    api_key: &str,
    provider: &ProviderConfig,
    model: &str,
    req: &Value,
    reservation: Option<crate::budget::SpendReservation>,
) -> Response {
    let started = Instant::now();
    let openai_req = match crate::anthropic_translate::translate_request(req) {
        Ok(v) => v,
        Err(reason) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Blocked,
                model,
                "anthropic",
                &provider.name,
                reason.clone(),
                0.0,
                0,
            );
            return (StatusCode::BAD_REQUEST, reason).into_response();
        }
    };
    // Local models cost nothing per token; asking for a usage frame still makes
    // the meter exact, and the translation layer never alters a caller's own
    // stream_options if they had one.
    let openai_req =
        crate::sse::request_stream_usage(&openai_req, &provider.tier, &provider.r#type);
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let resp = match (&*SHARED_CLIENT)
        .post(&url)
        .bearer_auth(provider.resolve_api_key())
        .json(&openai_req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "anthropic",
                &provider.name,
                format!("Provider stream failed: {e}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            return (
                StatusCode::BAD_GATEWAY,
                format!("Provider stream failed: {}", e),
            )
                .into_response();
        }
    };
    if !resp.status().is_success() {
        let code = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let text = resp.text().await.unwrap_or_default();
        crate::record_decision(
            state,
            crate::dashboard::Outcome::Failed,
            model,
            "anthropic",
            &provider.name,
            format!("Provider upstream returned {code}"),
            0.0,
            started.elapsed().as_millis() as u64,
        );
        return (code, text).into_response();
    }
    crate::record_decision(
        state,
        crate::dashboard::Outcome::Allowed,
        model,
        "anthropic",
        &provider.name,
        "Provider stream opened",
        0.0,
        started.elapsed().as_millis() as u64,
    );

    let mut meter = (!crate::cost::is_free_tier(&provider.tier)).then(|| AnthropicStreamMeter {
        budget: state.budget.clone(),
        api_key: api_key.to_string(),
        model: model.to_string(),
        usage: crate::sse::UsageScanner::new(crate::sse::Wire::Anthropic),
        prompt_tokens_est: (extract_prompt_text(req).len() / 4) as u64,
        _reservation: reservation,
    });
    let mut translator = crate::anthropic_translate::StreamTranslator::new(model);
    let stream = resp.bytes_stream().flat_map(move |chunk| {
        let events = match &chunk {
            Ok(bytes) => translator.feed_bytes(bytes),
            Err(_) => Vec::new(),
        };
        if let (Ok(bytes), Some(m)) = (&chunk, meter.as_mut()) {
            m.on_chunk(bytes);
        }
        futures_util::stream::iter(
            events
                .into_iter()
                .map(|event| Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(event))),
        )
    });
    Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(Body::from_stream(stream))
        .unwrap()
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
                self.model,
                usage.prompt_tokens,
                usage.completion_tokens,
                cost
            );
        } else {
            self.budget.record_spend_estimated(&self.api_key, cost);
            tracing::warn!(
                "/v1/messages stream billed from an ESTIMATE: model={} reported no usage; \
                 charged ${:.6}. The cap is working from a guess for this key.",
                self.model,
                cost
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
    headers: &HeaderMap,
    reservation: Option<crate::budget::SpendReservation>,
    credential: Option<&SubscriptionCredential>,
) -> Response {
    if subscription_bypasses_spend_accounting(provider) {
        return forward_stream_subscription(state, provider, model, req, headers, credential).await;
    }
    // OpenAI-compatible local provider: translate the request, dispatch to
    // {base_url}/chat/completions, and emit Anthropic SSE events. Metering is
    // preserved — the translated Anthropic events are tapped by the usual
    // AnthropicStreamMeter as they flow to the client.
    if provider_accepts_messages_translation(provider) {
        return forward_stream_openai(state, api_key, provider, model, req, reservation).await;
    }
    let url = anthropic_url(provider);
    let started = Instant::now();
    match anthropic_request(provider, &url, req, headers).send().await {
        Ok(resp) if resp.status().is_success() => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Allowed,
                model,
                "anthropic",
                &provider.name,
                "Anthropic stream opened",
                0.0,
                started.elapsed().as_millis() as u64,
            );
            // A free-tier Anthropic-compatible upstream (someone's local proxy)
            // costs nothing per token; do not pretend to bill it.
            let mut reservation = reservation;
            let mut meter =
                (!crate::cost::is_free_tier(&provider.tier)).then(|| AnthropicStreamMeter {
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
            let code =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let text = resp.text().await.unwrap_or_default();
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "anthropic",
                &provider.name,
                format!("Anthropic upstream returned {code}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            (code, text).into_response()
        }
        Err(e) => {
            crate::record_decision(
                state,
                crate::dashboard::Outcome::Failed,
                model,
                "anthropic",
                &provider.name,
                format!("Anthropic stream failed: {e}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            (
                StatusCode::BAD_GATEWAY,
                format!("Anthropic stream failed: {}", e),
            )
                .into_response()
        }
    }
}

/// Streaming subscription passthrough: the SSE bytes pass through verbatim, the
/// upstream status and content-type are preserved, no usage tap runs, and no
/// dollar accounting of any kind happens. A success announces its billing mode.
async fn forward_stream_subscription(
    state: &AppState,
    provider: &ProviderConfig,
    model: &str,
    req: &Value,
    headers: &HeaderMap,
    credential: Option<&SubscriptionCredential>,
) -> Response {
    let started = Instant::now();
    // The credential was resolved (or failed closed) in the handler before
    // dispatch; an absent one here is a wiring bug, not a client error.
    let credential = match credential {
        Some(credential) => credential,
        None => {
            return (
                StatusCode::BAD_GATEWAY,
                "claude_subscription credential was not resolved for this request",
            )
                .into_response()
        }
    };
    let url = match dispatch_url(provider) {
        Ok(url) => url,
        Err(reason) => return (StatusCode::FORBIDDEN, reason).into_response(),
    };
    if let Err(reason) =
        crate::subscription::validate_oauth_destination(&url, crate::subscription::CLAUDE_API_HOST)
    {
        record_metered_decision(
            &state.dashboard,
            provider,
            crate::dashboard::Outcome::Failed,
            model,
            format!("Anthropic stream refused: {reason}"),
            0.0,
            started.elapsed().as_millis() as u64,
        );
        return (StatusCode::FORBIDDEN, reason).into_response();
    }
    match subscription_request(&url, req, headers, credential)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Allowed,
                model,
                "Anthropic stream opened",
                0.0,
                started.elapsed().as_millis() as u64,
            );
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            let content_type = resp.headers().get(CONTENT_TYPE).cloned();
            let mut out =
                verbatim_response(status, content_type, Body::from_stream(resp.bytes_stream()));
            if let Ok(v) = CLAUDE_SUBSCRIPTION_BILLING_MODE.parse() {
                out.headers_mut().insert(BILLING_MODE_HEADER, v);
            }
            if let Ok(v) = provider.name.parse() {
                out.headers_mut().insert("x-stoke-node", v);
            }
            out
        }
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let content_type = resp.headers().get(CONTENT_TYPE).cloned();
            let body = resp.bytes().await.unwrap_or_default();
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Failed,
                model,
                format!("Anthropic upstream returned {status}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            verbatim_response(status, content_type, Body::from(body))
        }
        Err(e) => {
            record_metered_decision(
                &state.dashboard,
                provider,
                crate::dashboard::Outcome::Failed,
                model,
                format!("Anthropic stream failed: {e}"),
                0.0,
                started.elapsed().as_millis() as u64,
            );
            (
                StatusCode::BAD_GATEWAY,
                format!("Anthropic stream failed: {}", e),
            )
                .into_response()
        }
    }
}

/// Rebuild an upstream response with its exact status, content-type, and body —
/// subscription errors can be non-JSON and must never be rewritten.
fn verbatim_response(
    status: StatusCode,
    content_type: Option<axum::http::HeaderValue>,
    body: Body,
) -> Response {
    let mut builder = Response::builder().status(status);
    if let Some(ct) = content_type {
        builder = builder.header(CONTENT_TYPE, ct);
    }
    builder.body(body).unwrap()
}

fn anthropic_url(provider: &ProviderConfig) -> String {
    // base_url is the API root (e.g. https://api.anthropic.com); the Messages
    // path is always /v1/messages. Tolerate a base that already includes /v1.
    let base = provider
        .base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1");
    format!("{}/v1/messages", base)
}

fn anthropic_request(
    provider: &ProviderConfig,
    url: &str,
    body: &Value,
    inbound: &HeaderMap,
) -> reqwest::RequestBuilder {
    let mut request = (&*SHARED_CLIENT)
        .post(url)
        .header("x-api-key", provider.resolve_api_key())
        .header("content-type", "application/json")
        .json(body);

    let mut has_version = false;
    for (name, value) in inbound {
        let name_str = name.as_str();
        if name_str == "anthropic-version" {
            has_version = true;
        }
        if name_str.starts_with("anthropic-") || name_str.starts_with("x-claude-code-") {
            request = request.header(name, value);
        }
    }
    if !has_version {
        request = request.header("anthropic-version", ANTHROPIC_VERSION);
    }
    request
}

/// Build the upstream request for a `claude_subscription` provider.
///
/// Dispatched on `subscription::OAUTH_CLIENT` (redirects disabled) with the
/// Stoke-held OAuth store's access token as `Bearer <access_token>`, alongside
/// the Anthropic protocol headers (`anthropic-version`, `anthropic-beta`, …)
/// preserved verbatim — plus `oauth-2025-04-20`, which the official
/// subscription client must send and which is unioned with any client betas.
/// The Stoke gateway key (`x-stoke-key`) and the client's own Authorization
/// never leave the building, no `x-api-key` is ever set, and the access token
/// is never logged, persisted, hashed, or echoed.
fn subscription_request(
    url: &str,
    body: &Value,
    inbound: &HeaderMap,
    credential: &SubscriptionCredential,
) -> reqwest::RequestBuilder {
    let mut request = (&*crate::subscription::OAUTH_CLIENT)
        .post(url)
        .bearer_auth(&credential.access_token)
        .header("content-type", "application/json")
        .json(body);

    let mut has_version = false;
    for (name, value) in inbound {
        let name_str = name.as_str();
        match name_str {
            // The client's raw Authorization and the gateway key never travel:
            // the credential rides only in the bearer_auth set above. No
            // x-api-key is ever sent on this path.
            "authorization" | "x-stoke-key" | "x-api-key" | "host" | "content-length" => continue,
            "anthropic-version" => has_version = true,
            "anthropic-beta" => continue, // merged below with the OAuth beta
            _ => {}
        }
        if name_str.starts_with("anthropic-") || name_str.starts_with("x-claude-code-") {
            request = request.header(name, value);
        }
    }
    if let Some(beta) = merge_subscription_beta(inbound) {
        request = request.header("anthropic-beta", beta);
    }
    if !has_version {
        request = request.header("anthropic-version", ANTHROPIC_VERSION);
    }
    request
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
    use axum::http::HeaderValue;
    use serde_json::json;

    #[test]
    fn status_outcomes_distinguish_provider_success_from_failure() {
        assert_eq!(
            message_outcome(StatusCode::OK),
            crate::dashboard::Outcome::Allowed
        );
        assert_eq!(
            message_outcome(StatusCode::BAD_GATEWAY),
            crate::dashboard::Outcome::Failed
        );
    }

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
    fn only_openai_compatible_local_providers_take_the_translation_path() {
        fn provider(ty: &str, tier: &str) -> ProviderConfig {
            ProviderConfig {
                name: "p".into(),
                r#type: ty.into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                api_key: String::new(),
                api_key_env: String::new(),
                models: vec![],
                tier: tier.into(),
            }
        }
        assert!(provider_accepts_messages_translation(&provider(
            "openai_compatible",
            "local"
        )));
        assert!(provider_accepts_messages_translation(&provider(
            "openai_compatible",
            "remote"
        )));
        // Cloud OpenAI-compatible providers are NOT translated here: this
        // slice targets local/remote only.
        assert!(!provider_accepts_messages_translation(&provider(
            "openai_compatible",
            "cloud"
        )));
        assert!(!provider_accepts_messages_translation(&provider(
            "anthropic",
            "cloud"
        )));
        assert!(!provider_accepts_messages_translation(&provider(
            "claude_subscription",
            "subscription"
        )));
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

    #[test]
    fn claude_gateway_headers_reach_anthropic_without_client_authorization() {
        let provider = ProviderConfig {
            name: "anthropic".into(),
            r#type: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            api_key: "upstream-key".into(),
            api_key_env: String::new(),
            models: vec![],
            tier: "cloud".into(),
        };
        let mut inbound = HeaderMap::new();
        inbound.insert("authorization", "Bearer stoke-client-key".parse().unwrap());
        inbound.insert("anthropic-version", "2024-01-01".parse().unwrap());
        inbound.insert("anthropic-beta", "tool-search-2025-10-19".parse().unwrap());
        inbound.insert("x-claude-code-version", "2.1.0".parse().unwrap());

        let request = anthropic_request(
            &provider,
            "https://api.anthropic.com/v1/messages",
            &json!({"model": "claude-test"}),
            &inbound,
        )
        .build()
        .unwrap();

        assert_eq!(request.headers()["x-api-key"], "upstream-key");
        assert_eq!(request.headers()["anthropic-version"], "2024-01-01");
        assert_eq!(
            request.headers()["anthropic-beta"],
            "tool-search-2025-10-19"
        );
        assert_eq!(request.headers()["x-claude-code-version"], "2.1.0");
        assert!(request.headers().get("authorization").is_none());
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
        // unrelated upstream credential (e.g. a Claude OAuth token).
        let mut headers = HeaderMap::new();
        headers.insert("x-stoke-key", HeaderValue::from_static("gateway-key"));
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer claude-oauth-token"),
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
            HeaderValue::from_static("Bearer claude-oauth-token"),
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

    fn subscription_provider() -> ProviderConfig {
        ProviderConfig {
            name: "claude-subscription".into(),
            r#type: "claude_subscription".into(),
            base_url: "https://api.anthropic.com".into(),
            api_key: String::new(),
            api_key_env: String::new(),
            models: vec![],
            tier: "subscription".into(),
        }
    }

    fn regular_provider() -> ProviderConfig {
        ProviderConfig {
            name: "anthropic".into(),
            r#type: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            api_key: "upstream-key".into(),
            api_key_env: String::new(),
            models: vec![],
            tier: "cloud".into(),
        }
    }

    #[test]
    fn subscription_request_carries_client_credential_and_preserves_anthropic_headers() {
        let mut inbound = HeaderMap::new();
        inbound.insert(
            "authorization",
            "Bearer sk-ant-oat01-injected-token".parse().unwrap(),
        );
        inbound.insert("x-stoke-key", "stk-gateway-key".parse().unwrap());
        inbound.insert("anthropic-version", "2023-06-01".parse().unwrap());
        inbound.insert("anthropic-beta", "tool-search-2025-10-19".parse().unwrap());
        inbound.insert("x-claude-code-version", "2.1.0".parse().unwrap());

        let credential = SubscriptionCredential {
            access_token: "injected-access-token".into(),
        };
        let request = subscription_request(
            "https://api.anthropic.com/v1/messages",
            &json!({"model": "claude-test", "max_tokens": 16}),
            &inbound,
            &credential,
        )
        .build()
        .unwrap();

        let headers = request.headers();
        assert_eq!(headers["authorization"], "Bearer injected-access-token");
        // anthropic-version must survive verbatim; anthropic-beta is the
        // union of the client's betas and the OAuth beta the official
        // subscription client must send (client betas keep their order).
        assert_eq!(headers["anthropic-version"], "2023-06-01");
        assert_eq!(
            headers["anthropic-beta"],
            "tool-search-2025-10-19,oauth-2025-04-20"
        );
        assert_eq!(headers["x-claude-code-version"], "2.1.0");
        assert!(
            headers.get("x-stoke-key").is_none(),
            "the gateway key must never reach Anthropic"
        );
        assert!(
            !headers
                .iter()
                .any(|(name, value)| name.as_str() != "authorization"
                    && value
                        .to_str()
                        .map(|v| v.contains("injected-access-token"))
                        .unwrap_or(false)),
            "the access token must not ride along under any other header name"
        );
    }

    #[test]
    fn subscription_credential_is_an_opaque_passthrough_of_the_client_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer sk-ant-oat01-abc.def"),
        );
        let credential = SubscriptionCredential::from_headers(&headers).unwrap();
        assert_eq!(credential.access_token, "sk-ant-oat01-abc.def");

        // A bare (scheme-less) Authorization still passes through untouched.
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("raw-token"));
        assert_eq!(
            SubscriptionCredential::from_headers(&headers)
                .unwrap()
                .access_token,
            "raw-token"
        );

        // No Authorization at all: no credential.
        assert!(SubscriptionCredential::from_headers(&HeaderMap::new()).is_none());
    }

    #[test]
    fn subscription_dispatch_url_is_exact_or_refused() {
        assert_eq!(
            dispatch_url(&subscription_provider()).unwrap(),
            "https://api.anthropic.com/v1/messages"
        );

        let mut hostile = subscription_provider();
        for base in [
            "https://api.anthropic.com.evil.test",
            "https://anthropic.evil.test/api.anthropic.com",
            "http://api.anthropic.com",
            "https://chatgpt.com/backend-api/codex",
        ] {
            hostile.base_url = base.into();
            assert!(dispatch_url(&hostile).is_err(), "must refuse {base}");
        }

        // Non-subscription providers keep the generic URL logic.
        assert_eq!(
            dispatch_url(&regular_provider()).unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn subscription_admission_bypasses_only_the_spend_machinery() {
        let sub = subscription_provider();
        let regular = regular_provider();

        assert!(subscription_bypasses_spend_accounting(&sub));
        assert!(!subscription_bypasses_spend_accounting(&regular));

        // The bypass must come from the provider kind, NOT the free-tier
        // shortcut — otherwise stream_fusion races subscription as local.
        assert!(!crate::cost::is_free_tier(&sub.tier));
        assert!(!crate::cost::is_free_tier(&regular.tier));
    }

    #[test]
    fn subscription_never_enters_the_dollar_decision_path() {
        let sub = subscription_provider();
        let regular = regular_provider();
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
        assert_eq!(regular_event.route, "anthropic");
        assert_eq!(regular_event.provider, "anthropic");
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
            "claude-test",
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

    #[test]
    fn subscription_success_announces_billing_mode_not_cost() {
        assert_eq!(BILLING_MODE_HEADER, "x-stoke-billing-mode");
        assert_eq!(CLAUDE_SUBSCRIPTION_BILLING_MODE, "claude_subscription");

        let mut response = verbatim_response(
            StatusCode::OK,
            Some(HeaderValue::from_static("text/event-stream")),
            Body::empty(),
        );
        if let Ok(v) = CLAUDE_SUBSCRIPTION_BILLING_MODE.parse() {
            response.headers_mut().insert(BILLING_MODE_HEADER, v);
        }
        assert!(response.headers().get("x-stoke-cost").is_none());
        assert_eq!(
            response.headers()[BILLING_MODE_HEADER],
            "claude_subscription"
        );
    }

    #[test]
    fn verbatim_response_preserves_status_and_content_type() {
        let response = verbatim_response(
            StatusCode::SERVICE_UNAVAILABLE,
            Some(HeaderValue::from_static("text/plain; charset=utf-8")),
            Body::from("upstream exploded"),
        );
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers()[CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );

        // No upstream content-type: none is invented.
        let response = verbatim_response(StatusCode::OK, None, Body::empty());
        assert!(response.headers().get(CONTENT_TYPE).is_none());
    }
}
