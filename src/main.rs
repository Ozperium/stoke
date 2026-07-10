mod auto_route;
mod budget;
mod builtins;
mod config;
mod cost;
mod cache;
mod failover;
mod messages;
mod nodes;
mod plugins;
mod router;
mod sse;
mod stream_fusion;
mod ttft;

#[cfg(feature = "js-plugins")]
mod js_plugins;

pub use config::Config;
pub use config::ProviderConfig;
pub use router::*;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{Json, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tracing_subscriber;

use futures_util::StreamExt;
use router::{
    call_provider_hop, cascade, cascade_test_models,
    self_consistency, test_vote_models, ProviderResult, SHARED_CLIENT,
};
use cache::ResponseCache;
use budget::{Auth, BudgetGuard};
use ttft::TtftTracker;

#[derive(Clone)]
pub struct AppState {
    config: Arc<Config>,
    cache: Arc<ResponseCache>,
    ttft: Arc<TtftTracker>,
    auth: Arc<Auth>,
    budget: Arc<BudgetGuard>,
    nodes: Arc<nodes::NodeRegistry>,
    plugins: Arc<plugins::Plugins>,
    builtins: Arc<builtins::Builtins>,
    #[cfg(feature = "js-plugins")]
    js_plugins: Arc<js_plugins::JsPlugins>,
}

/// `stoke` takes no subcommands — it serves. But an unrecognised argv used to
/// fall through and silently bind a port, so `stoke --version` started a
/// gateway instead of answering. Handle the three flags a person actually
/// types, and refuse anything else rather than daemonising by surprise.
fn handle_flags() {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("stoke {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                eprintln!(
                    "stoke {} — the gateway daemon.\n\n\
                     Usage: stoke\n\n\
                     Reads stoke.toml from the current directory or ~/.config/stoke/,\n\
                     then serves. It takes no options; use stoke-cli to manage config.\n\n\
                     Options:\n  \
                       -V, --version   print version and exit\n  \
                       -h, --help      print this help and exit\n\n\
                     Environment:\n  \
                       STOKE_API_KEYS  comma-separated keys; required unless STOKE_DEV=1\n  \
                       STOKE_DEV       set to 1 to allow unauthenticated local requests\n\n\
                     Docs: https://stokegate.com",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("stoke: unknown option: {other}");
                eprintln!("Try 'stoke --help'. To manage config, use 'stoke-cli'.");
                std::process::exit(2);
            }
        }
    }
}

#[tokio::main]
async fn main() {
    handle_flags();
    tracing_subscriber::fmt::init();

    let config = Config::load().unwrap_or_else(|e| {
        tracing::error!("Config error: {}", e);
        std::process::exit(1);
    });

    // Prices are operator config, never guessed. Publish them before the first
    // request can reach the dispatch gate in router::call_provider_hop.
    cost::init(config.pricer());
    if config.pricing.models.is_empty()
        && cost::Unpriced::parse(&config.pricing.unpriced) == cost::Unpriced::Refuse
        && config.providers.iter().any(|p| !cost::is_free_tier(&p.tier))
    {
        tracing::warn!(
            "no [pricing.models] configured, so metered providers will refuse every model. \
             Declare prices, or set [pricing] unpriced = \"free\" to serve them unmetered."
        );
    }

    tracing::info!(
        "Stoke starting on {}:{} with {} provider(s)",
        config.server.host,
        config.server.port,
        config.providers.len()
    );

    let plugins_config = config.plugins.clone();
    let builtins_config = config.builtins.clone();

    #[cfg(feature = "js-plugins")]
    let js_plugins = {
        let script_paths = &config.plugins.scripts;
        if !script_paths.is_empty() {
            match js_plugins::JsPlugins::load(script_paths) {
                Ok(p) => {
                    tracing::info!("JS plugins loaded");
                    Arc::new(p)
                }
                Err(e) => {
                    tracing::error!("Failed to load JS plugins: {}", e);
                    Arc::new(js_plugins::JsPlugins::load(&[]).unwrap())
                }
            }
        } else {
            Arc::new(js_plugins::JsPlugins::load(&[]).unwrap())
        }
    };

    let node_registry = Arc::new(nodes::NodeRegistry::from_config(&config));
    nodes::spawn_poller(node_registry.clone());

    // Apply per-key enforcement policy from [[keys]] config to the budget guard.
    let budget = BudgetGuard::new();
    for policy in &config.keys {
        if policy.budget_usd > 0.0 {
            budget.set_budget(&policy.key, policy.budget_usd);
            tracing::info!(
                "policy: key {}… budget ${:.2}",
                &policy.key[..8.min(policy.key.len())],
                policy.budget_usd
            );
        }
        if policy.rate_limit_rpm > 0 {
            budget.set_rate_limit(&policy.key, policy.rate_limit_rpm);
            tracing::info!(
                "policy: key {}… rate {} rpm",
                &policy.key[..8.min(policy.key.len())],
                policy.rate_limit_rpm
            );
        }
    }

    let state = AppState {
        config: Arc::new(config),
        cache: Arc::new(ResponseCache::new(3600, 0.92, std::env::var("STOKE_SEMANTIC_CACHE").is_ok())),
        ttft: Arc::new(TtftTracker::new()),
        auth: Arc::new(Auth::new()),
        budget: Arc::new(budget),
        nodes: node_registry,
        plugins: Arc::new(plugins::Plugins::new(
            plugins_config,
        )),
        builtins: Arc::new(builtins::Builtins::new(builtins_config)),
        #[cfg(feature = "js-plugins")]
        js_plugins,
    };

    let addr = format!("{}:{}", state.config.server.host, state.config.server.port);
    tracing::info!("Listening on {}", addr);

    let app = {
        let base_router = Router::new()
            .route("/health", get(health))
            .route("/v1/models", get(list_models))
            .route("/v1/pricing", get(list_pricing))
            .route("/v1/cache", get(cache_stats))
            .route("/v1/ttft", get(ttft_stats))
            .route("/v1/budget", get(budget_stats))
            .route("/v1/nodes", get(nodes_status))
            .route("/v1/chat/completions", post(chat_completions))
            .route("/v1/messages", post(messages::messages))
            .route("/v1/routes", get(list_routes));

        // Register dynamic route profiles
        // Each profile with path="/v1/xxx/completions" gets its own endpoint
        let router = state.config.routes.iter().fold(base_router, |acc, profile| {
            tracing::info!("Route profile: {} -> {} ({})", profile.name, profile.path, profile.routing);
            acc.route(&profile.path, post(chat_completions))
        });

        // Fail-closed everywhere: every endpoint except /health requires auth
        // when keys are configured. Status endpoints leak model inventory and
        // load — they are part of the protected surface, not an exception.
        let router = router.layer(middleware::from_fn_with_state(state.clone(), require_auth));

        router.with_state(state)
    };

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "stoke" }))
}

/// Auth gate for every endpoint except /health (liveness probes stay open).
/// Mirrors the fail-closed rules of `Auth::validate`: no keys + no STOKE_DEV
/// rejects everything; configured keys require a matching Bearer token.
async fn require_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    if state.auth.validate(auth_header.as_deref()).is_none() {
        return (StatusCode::UNAUTHORIZED, "Invalid or missing API key").into_response();
    }
    next.run(req).await
}

async fn cache_stats(State(state): State<AppState>) -> Json<Value> {
    let stats = state.cache.stats();
    Json(json!({
        "entries": stats.entries,
        "hits": stats.hits,
    }))
}

async fn ttft_stats(State(state): State<AppState>) -> Json<Value> {
    let stats = state.ttft.stats();
    let providers: Vec<Value> = stats.iter().map(|(name, (ms, errors))| {
        json!({"provider": name, "avg_ttft_ms": ms, "consecutive_errors": errors})
    }).collect();
    Json(json!({"providers": providers}))
}

async fn budget_stats(State(state): State<AppState>) -> Json<Value> {
    let stats = state.budget.stats();
    let keys: Vec<Value> = stats.iter().map(|(key, spend, limit, recent_rpm, estimated)| {
        json!({
            "key": &key[..8.min(key.len())],
            "spend_usd": spend,
            "limit_usd": limit,
            "recent_requests": recent_rpm,
            // The part of spend_usd that Stoke had to estimate because a metered
            // provider streamed without reporting usage. Counted against the cap.
            "estimated_usd": estimated,
        })
    }).collect();
    let auth_enabled = state.auth.is_auth_enabled();
    let (requests, zero_marginal, avoided) = state.budget.receipts();
    Json(json!({
        "auth_enabled": auth_enabled,
        "keys": keys,
        // Honest receipts: "avoided" is the LIST-PRICE counterfactual of the
        // configured [auto_route].quality model for auto-routed requests that
        // were served at zero marginal cost. An estimate — never a
        // quality-equivalence claim. Zero when no quality model is configured.
        "receipts": {
            "requests": requests,
            "zero_marginal_requests": zero_marginal,
            "zero_marginal_pct": if requests > 0 { (zero_marginal as f64 / requests as f64 * 100.0).round() } else { 0.0 },
            "cloud_listprice_avoided_usd_est": (avoided * 10_000.0).round() / 10_000.0,
        }
    }))
}

async fn nodes_status(State(state): State<AppState>) -> Json<Value> {
    Json(state.nodes.snapshot())
}

/// All providers, minus federated Stoke gateways when the hop guard is active.
fn eligible_providers(config: &Config, exclude_stoke: bool) -> Vec<&ProviderConfig> {
    config
        .providers
        .iter()
        .filter(|p| !(exclude_stoke && p.r#type == "stoke"))
        .collect()
}

/// `Config::provider_for_model` with the hop-guard filter applied: a request
/// that already crossed a Stoke gateway must never select another one.
fn provider_for_model_filtered<'a>(
    config: &'a Config,
    model: &str,
    exclude_stoke: bool,
) -> Option<&'a ProviderConfig> {
    let candidates = eligible_providers(config, exclude_stoke);
    candidates
        .iter()
        .find(|p| !p.models.is_empty() && p.models.iter().any(|m| m == model))
        .copied()
        .or_else(|| candidates.first().copied())
}

/// Measures a live SSE stream: time-to-first-token (connect time already
/// includes prefill for Ollama; we add the delta to the first body chunk),
/// tokens/sec (SSE `data:` events ≈ tokens), and — for providers that charge —
/// the token usage they report on the final frame. Records into the registry and
/// the budget on Drop, which also fires on client disconnect, so a stream that is
/// abandoned halfway still teaches the predictor and still bills the caller.
/// Owns the in-flight guard for the stream's lifetime.
struct StreamMeter {
    _guard: Option<nodes::InflightGuard>,
    registry: Arc<nodes::NodeRegistry>,
    node: String,
    model: String,
    connect_ms: u64,
    started: std::time::Instant,
    first_chunk_ms: Option<f64>,
    last_chunk_ms: f64,
    events: u64,
    /// Reads the provider's usage report out of the stream as it passes.
    usage: sse::UsageScanner,
    /// Set only for providers whose tokens cost money. A free tier needs no
    /// accounting, and is never asked to report usage in the first place.
    billing: Option<StreamBilling>,
}

/// What the meter needs in order to charge for a stream once it ends.
struct StreamBilling {
    budget: Arc<BudgetGuard>,
    api_key: String,
    /// Fallback when the provider never reports usage. An estimate, and labelled one.
    prompt_tokens_est: u64,
}

impl StreamMeter {
    fn new(
        guard: Option<nodes::InflightGuard>,
        registry: Arc<nodes::NodeRegistry>,
        node: String,
        model: String,
        connect_ms: u64,
        billing: Option<StreamBilling>,
    ) -> Self {
        Self {
            _guard: guard,
            registry,
            node,
            model,
            connect_ms,
            started: std::time::Instant::now(),
            first_chunk_ms: None,
            last_chunk_ms: 0.0,
            events: 0,
            usage: sse::UsageScanner::new(sse::Wire::OpenAi),
            billing,
        }
    }

    fn on_chunk(&mut self, bytes: &[u8]) {
        let now_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        if self.first_chunk_ms.is_none() {
            self.first_chunk_ms = Some(now_ms);
        }
        self.last_chunk_ms = now_ms;
        self.events += bytes.windows(5).filter(|w| *w == &b"data:"[..]).count() as u64;
        if self.billing.is_some() {
            self.usage.feed(bytes);
        }
    }
}

impl Drop for StreamMeter {
    fn drop(&mut self) {
        if let Some(first) = self.first_chunk_ms {
            let ttft_ms = self.connect_ms as f64 + first;
            let gen_ms = (self.last_chunk_ms - first).max(0.0);
            // subtract the [DONE] terminator from the token approximation
            let tokens = self.events.saturating_sub(1);
            self.registry
                .record_stream_stats(&self.node, &self.model, ttft_ms, tokens, gen_ms);
        }

        let Some(billing) = self.billing.take() else { return };

        let (usage, measured) = match self.usage.usage() {
            Some(u) => (u, true),
            None => {
                // No final tally: either the provider never reported, or the
                // stream ended before the frame carrying it. Recording $0 is the
                // exact bug this path exists to fix, so estimate — using whatever
                // partial truth the provider did give us, and label it a guess.
                let partial = self.usage.partial();
                (
                    sse::Usage {
                        prompt_tokens: partial
                            .map(|u| u.prompt_tokens)
                            .filter(|&t| t > 0)
                            .unwrap_or(billing.prompt_tokens_est),
                        completion_tokens: partial
                            .map(|u| u.completion_tokens)
                            .unwrap_or(0)
                            .max(self.usage.frames()),
                    },
                    false,
                )
            }
        };

        let cost = cost::global()
            .calculate(&self.model, Some(&usage.to_openai_json()))
            .cost_usd;

        if measured {
            billing.budget.record_spend(&billing.api_key, cost);
            tracing::info!(
                "stream billed: model={} node={} tokens={}+{} cost=${:.6}",
                self.model, self.node, usage.prompt_tokens, usage.completion_tokens, cost
            );
        } else {
            billing.budget.record_spend_estimated(&billing.api_key, cost);
            tracing::warn!(
                "stream billed from an ESTIMATE: model={} node={} reported no usage; \
                 charged ${:.6} for ~{}+{} tokens{}. The cap is working from a guess for this key.",
                self.model, self.node, cost, usage.prompt_tokens, usage.completion_tokens,
                if self.usage.lost_data() { "; a stream line exceeded the buffer" } else { "" }
            );
        }
    }
}

async fn list_pricing() -> Json<Value> {
    let pricer = cost::global();
    let prices: Vec<Value> = pricer
        .all_prices()
        .iter()
        .map(|(model, p)| {
            json!({
                "model": model,
                "input_per_1m": p.input_per_1m,
                "output_per_1m": p.output_per_1m,
                "local": p.input_per_1m == 0.0 && p.output_per_1m == 0.0,
            })
        })
        .collect();
    Json(json!({ "pricing": prices }))
}

async fn list_models(State(state): State<AppState>) -> Json<Value> {
    // For now, list models from config. TODO: live discovery from providers.
    let models: Vec<Value> = state
        .config
        .providers
        .iter()
        .flat_map(|p| {
            if p.models.is_empty() {
                vec![json!({ "id": format!("{}:*", p.name), "provider": p.name })]
            } else {
                p.models
                    .iter()
                    .map(|m| json!({ "id": m, "provider": p.name }))
                    .collect()
            }
        })
        .collect();

    Json(json!({ "object": "list", "data": models }))
}

async fn list_routes(State(state): State<AppState>) -> Json<Value> {
    let routes: Vec<Value> = state.config.routes.iter().map(|r| {
        json!({
            "name": r.name,
            "path": r.path,
            "routing": r.routing,
            "model": r.model,
            "vote_models": r.vote_models,
            "builtins": r.builtins,
            "stream": r.stream,
            "budget_usd": r.budget_usd,
            "rate_limit": r.rate_limit,
        })
    }).collect();
    Json(json!({ "routes": routes }))
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
    Json(mut req): Json<ChatCompletionRequest>,
) -> Response {
    // Auth check: if STOKE_API_KEYS is set, validate the Bearer token
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let api_key = match state.auth.validate(auth_header.as_deref()) {
        Some(k) => k,
        None => {
            return (StatusCode::UNAUTHORIZED, "Invalid or missing API key").into_response();
        }
    };

    // Compute prompt hash for loop detection (model + messages + temp)
    let prompt_text: String = req.messages.iter()
        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt_hash = {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(req.model.as_bytes());
        hasher.update(prompt_text.as_bytes());
        hasher.update(req.temperature.unwrap_or(0.0).to_be_bytes());
        hex::encode(hasher.finalize())
    };

    // Budget + rate limit check + loop detection (exact + semantic)
    if let Err(reason) = state.budget.check_with_prompt(&api_key, &prompt_hash, &prompt_text).await {
        return (StatusCode::TOO_MANY_REQUESTS, reason).into_response();
    }

    // Federation hop count: >= 1 means this request already crossed a Stoke
    // gateway. Federated (type = "stoke") providers are then excluded from
    // every routing path so requests can never loop between gateways.
    let hop: u32 = headers
        .get("x-stoke-hop")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())
        .map(|v| v.min(16)) // clamp: forged huge values must not overflow hop+1 math
        .unwrap_or(0);
    let exclude_stoke = hop >= 1;

    // Resolve route profile by path (multi-endpoint routing)
    let path = uri.path();
    let route_profile = state.config.routes.iter().find(|r| r.path == path);

    let mut model = req.model.clone();

    // Determine routing: route profile > request extra > config default.
    //
    // A fan-out pattern turns one inbound request into N billed provider calls,
    // so choosing one is an operator decision. Enforcement happens below, on the
    // *resolved* routing — `auto` names no fan-out here but can resolve into one.
    let mut routing_from_caller = false;
    let mut routing = if let Some(profile) = route_profile {
        profile.routing.clone()
    } else {
        match req.extra.get("routing").and_then(|v| v.as_str()) {
            Some(asked) => {
                routing_from_caller = true;
                asked.to_string()
            }
            None => state.config.routing.clone().unwrap_or_else(|| "single".to_string()),
        }
    };

    // Determine vote_models: route profile > request extra
    let mut vote_models: Vec<String> = if let Some(profile) = route_profile {
        profile.vote_models.clone()
    } else {
        req.extra.get("vote_models")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    };

    // Apply route profile model if set
    if let Some(profile) = route_profile {
        if !profile.model.is_empty() {
            req.model = profile.model.clone();
            model = profile.model.clone();
        }
    }

    // Plugin: pre_request — can override model, routing, vote_models, or block
    let plugin_overrides = if state.plugins.has_pre_request() {
        match state
            .plugins
            .pre_request(&req.model, &routing, &req.messages, &api_key)
            .await
        {
            Ok(result) => {
                if let Some(ref m) = result.model {
                    if !m.is_empty() {
                        req.model = m.clone();
                        model = m.clone();
                    }
                }
                if let Some(ref r) = result.routing {
                    if !r.is_empty() {
                        routing = r.clone();
                    }
                }
                if let Some(ref vm) = result.vote_models {
                    vote_models = vm.clone();
                }
                true
            }
            Err(block_msg) => {
                return (StatusCode::FORBIDDEN, block_msg).into_response();
            }
        }
    } else {
        false
    };

    // JS plugin: pre_request — can override model, routing, vote_models, or block.
    // JsRuntime is !Send, so we must use spawn_blocking to avoid holding the mutex
    // across .await points (which would make the handler future !Send).
    #[cfg(feature = "js-plugins")]
    if !state.js_plugins.is_empty() {
        let js_result = tokio::task::spawn_blocking({
            let js_plugins = state.js_plugins.clone();
            let model = req.model.clone();
            let routing = routing.clone();
            let messages = req.messages.clone();
            let api_key = api_key.clone();
            move || js_plugins.pre_request(&model, &routing, &messages, &api_key)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("JS pre_request join error: {}", e);
            Err("JS plugin interrupted".to_string())
        });

        match js_result {
            Ok((js_model, js_routing, js_vote_models, js_block)) => {
                if let Some(reason) = js_block {
                    return (StatusCode::FORBIDDEN, reason).into_response();
                }
                if let Some(m) = js_model {
                    if !m.is_empty() {
                        req.model = m.clone();
                        model = m.clone();
                    }
                }
                if let Some(r) = js_routing {
                    if !r.is_empty() {
                        routing = r.clone();
                    }
                }
                if let Some(vm) = js_vote_models {
                    vote_models = vm.clone();
                }
            }
            Err(e) => {
                tracing::error!("JS pre_request plugin error: {}", e);
            }
        }
    };

    // Built-in: pre_request (prompt harness) — inject system prompts
    if let Some(ref profile) = route_profile {
        if profile.builtins.contains(&"prompt_harness".to_string()) {
            if let Ok(Some(msgs)) = state.builtins.pre_request(&model, &routing, &req.messages, &api_key).await {
                if let Some(arr) = msgs.as_array() {
                    req.messages = arr.clone();
                }
            }
        }
    }

    // Plugin: prompt_filter — can block or redact messages
    if state.plugins.has_prompt_filter() {
        match state
            .plugins
            .prompt_filter(&req.messages, &model, &api_key)
            .await
        {
            Ok(filtered) => {
                req.messages = filtered;
            }
            Err(block_msg) => {
                return (StatusCode::FORBIDDEN, block_msg).into_response();
            }
        }
    }

    // Built-in: prompt_filter (PII redaction) — strip secrets from messages
    if let Some(ref profile) = route_profile {
        if profile.builtins.contains(&"pii_redact".to_string()) {
            if let Ok(Some(filtered)) = state.builtins.prompt_filter(&req.messages, &model, &api_key).await {
                req.messages = filtered;
                tracing::info!("pii_redact: redacted sensitive data from prompt");
            }
        }
    }

    // JS plugin: prompt_filter — can block or redact messages
    #[cfg(feature = "js-plugins")]
    if !state.js_plugins.is_empty() {
        let js_result = tokio::task::spawn_blocking({
            let js_plugins = state.js_plugins.clone();
            let messages = req.messages.clone();
            let model = model.clone();
            let api_key = api_key.clone();
            move || js_plugins.prompt_filter(&messages, &model, &api_key)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("JS prompt_filter join error: {}", e);
            Err("JS plugin interrupted".to_string())
        });

        match js_result {
            Ok((Some(filtered), None)) => {
                req.messages = filtered;
            }
            Ok((_, Some(block_reason))) => {
                return (StatusCode::FORBIDDEN, block_reason).into_response();
            }
            Ok((None, None)) => {}
            Err(e) => {
                tracing::error!("JS prompt_filter plugin error: {}", e);
            }
        }
    }

    // For test_vote: the test harness code and entry point function name
    let mut test_code = req.extra.get("test_code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut entry_point = req.extra.get("entry_point")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // For self_consistency: number of samples and temperature. The caller names a
    // number; the operator sets the ceiling. Unclamped, `n_samples` is a direct
    // multiplier on the bill (router.rs loops over it, one provider call each).
    let n_samples = (req.extra.get("n_samples")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize)
        .clamp(1, state.config.limits.max_n_samples);
    let sc_temperature = req.extra.get("temperature")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.7) as f32;

    // Save extra fields for auto-routing before cleanup
    let original_extra = req.extra.clone();

    // Clean up our extra fields before forwarding to provider
    req.extra.remove("routing");
    req.extra.remove("vote_models");
    req.extra.remove("test_code");
    req.extra.remove("entry_point");
    req.extra.remove("n_samples");
    req.extra.remove("sc_temperature");

    // Auto-routing: classify prompt and pick best pattern + model
    // Auto v2: scorer receipts + counterfactual, carried into the response
    // and the savings ledger.
    let mut auto_note: Option<Value> = None;
    let mut auto_counterfactual_usd: f64 = 0.0;
    let mut auto_routed = false;

    // Array-aware prompt size (handles OpenAI content-parts form; the
    // loop-detection prompt_text only sees string content). Used by the auto
    // scorer and the hedge size gate — both must not undersize array prompts.
    let prompt_chars = auto_route::extract_text(&req.messages).len();

    if routing == "auto" {
        // Sync resolution: explicit [auto_route] config first, then models
        // discovered on the user's own nodes. No built-in names, no probing.
        let opts = auto_route::RouteOpts::resolve(&state.config, &state.nodes, &original_extra);
        let facts = auto_route::RequestFacts {
            prompt_tokens_est: (prompt_chars / 4) as u64,
            gen_tokens_est: req.max_tokens.map(|m| m.min(4096) as u64).unwrap_or(512),
            has_tools: req.extra.contains_key("tools"),
            mode: auto_route::Mode::from_model_name(&req.model),
        };
        let decision = auto_route::decide(
            &req.messages,
            &opts,
            &state.nodes.discovered(),
            cost::global(),
            &facts,
        );
        tracing::info!(
            "auto-route: class={:?} -> pattern={}, model={}, reason={}",
            decision.class, decision.pattern, decision.model, decision.reason
        );
        if decision.model.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "auto routing could not resolve a model ({}). Set default_model or an \
                     [auto_route] section in stoke.toml, or ensure at least one model is \
                     discoverable on a configured node (see GET /v1/nodes).",
                    decision.reason
                ),
            )
                .into_response();
        }

        let cand_json = |c: &auto_route::ScoredCandidate| {
            json!({
                "model": c.model,
                "node": c.node,
                "warm": c.warm,
                "est_cost_usd": (c.est_cost_usd * 1e6).round() / 1e6,
                "predicted_ms": c.predicted_ms.round(),
                "excluded": c.excluded,
            })
        };
        auto_note = Some(json!({
            "mode": facts.mode.label(),
            "class": format!("{:?}", decision.class),
            "chosen": decision.chosen.as_ref().map(cand_json),
            "not_taken": decision.not_taken.iter().map(cand_json).collect::<Vec<_>>(),
            "counterfactual_usd_est": (decision.counterfactual_usd * 1e6).round() / 1e6,
        }));
        auto_counterfactual_usd = decision.counterfactual_usd;
        auto_routed = true;

        routing = decision.pattern.clone();
        model = decision.model.clone();
        req.model = decision.model.clone();
        if !decision.vote_models.is_empty() {
            vote_models = decision.vote_models.clone();
        }
        if decision.pattern == "cascade_test" || decision.pattern == "test_vote" {
            test_code = opts.test_code.clone();
            entry_point = opts.entry_point.clone();
        }
    }

    // ─── Fan-out enforcement, on the RESOLVED routing ────────────────────────
    // Everything that can name a routing pattern has now spoken: the route
    // profile, the request body, the config default, the auto-router, and the
    // pre_request plugins. Checking earlier missed the interesting case —
    // `routing: "auto"` is not itself a fan-out, but `decide()` can resolve it
    // into `cascade_test`, using `test_code`/`entry_point` taken straight from
    // the request body. A caller could pick a fan-out without ever naming one.
    if config::is_fanout_routing(&routing)
        && routing_from_caller
        && !state.config.limits.allow_caller_routing
    {
        return (
            StatusCode::FORBIDDEN,
            format!(
                "routing resolved to \"{routing}\", which issues multiple provider calls per \
                 request, and the caller selected it. Pin it in a [[routes]] profile or the \
                 top-level `routing` setting, or set [limits] allow_caller_routing = true."
            ),
        )
            .into_response();
    }

    // Ceiling on the fan-out width, whatever its source — including the
    // auto-router, which reassigns vote_models wholesale from [auto_route].
    if vote_models.len() > state.config.limits.max_vote_models {
        tracing::warn!(
            "vote_models truncated from {} to the [limits] max_vote_models of {}",
            vote_models.len(),
            state.config.limits.max_vote_models
        );
        vote_models.truncate(state.config.limits.max_vote_models);
    }

    tracing::info!("Request: model={}, routing={}, vote_models={:?}", model, routing, vote_models);

    // Streaming: supported for single routing (direct passthrough) and stream_race.
    // Fusion patterns aggregate multiple responses and can't stream.
    // stream_race: race multiple providers/models, first to connect wins.
    if req.stream.unwrap_or(false) && (routing == "single" || routing == "stream_race") {
        let body = serde_json::to_value(&req).unwrap_or_default();

        // Each branch resolves to (guard, winning node, connect ms, response, the
        // model that actually served). The served model is not always the one the
        // caller named: a vote race rewrites it per leg, and the bill must be
        // priced against what ran, not against what was asked for.
        let race_result: Result<(Option<nodes::InflightGuard>, String, u64, reqwest::Response, String), (StatusCode, String)> =
        if routing == "stream_race" {
            // Race multiple models on the same provider (or across providers if vote_models given)
            if !vote_models.is_empty() {
                // always exclude federated gateways from races: stream_fusion
                // doesn't propagate x-stoke-hop, and racing a gateway amplifies load
                let provider = provider_for_model_filtered(&state.config, &model, true)
                    .ok_or_else(|| (StatusCode::NOT_FOUND, format!("No provider for model: {}", model)));
                match provider {
                    Ok(p) => stream_fusion::stream_race_models(p, &vote_models, &body)
                        .await
                        .map(|(pname, mname, resp)| {
                            tracing::info!("stream_race winner: {}/{}", pname, mname);
                            let guard = state.nodes.begin(&pname);
                            (guard, pname, 0, resp, mname)
                        })
                        .map_err(|e| (StatusCode::BAD_GATEWAY, e)),
                    Err(e) => Err(e),
                }
            } else {
                let providers: Vec<_> = eligible_providers(&state.config, true);
                stream_fusion::stream_race(providers, &body)
                    .await
                    .map(|(pname, resp)| {
                        let guard = state.nodes.begin(&pname);
                        (guard, pname, 0, resp, model.clone())
                    })
                    .map_err(|e| (StatusCode::BAD_GATEWAY, e))
            }
        } else {
            // Single routing with failover — candidates ordered by node placement
            // (warm > cold > unknown; ties by tier, in-flight, latency EWMA)
            let (ranked, explain) = state.nodes.rank(&model, &state.config.providers, exclude_stoke);
            tracing::info!("stream placement: {}", explain.join("; "));
            if ranked.is_empty() {
                return (
                    StatusCode::NOT_FOUND,
                    format!("No provider for model: {} ({})", model, explain.join("; ")),
                ).into_response();
            }
            // Hedged dispatch (opt-in): small prompt + top-2 candidates both
            // zero-marginal and verifiably holding the model → race them,
            // first past prefill wins. Duplicate local compute buys tail latency.
            let hedge_pair = if state.config.auto_route.hedge
                && prompt_chars / 4 < 1500
                && ranked.len() >= 2
                && ranked[..2].iter().all(|p| p.tier != "cloud")
            {
                let discovered = state.nodes.discovered();
                let holds = |node: &str| discovered.iter().any(|d| d.node == node && d.matches(&model));
                (holds(&ranked[0].name) && holds(&ranked[1].name)).then(|| (ranked[0], ranked[1]))
            } else {
                None
            };
            let win = if let Some((a, b)) = hedge_pair {
                tracing::info!("hedging {} across {} + {}", model, a.name, b.name);
                failover::stream_hedged(a, b, &body, &state.nodes, hop).await
            } else {
                failover::stream_with_failover(ranked, &body, &state.nodes, hop).await
            };
            win.map(|w| (w.guard, w.provider_name, w.connect_ms, w.response, model.clone()))
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))
        };

        match race_result {
            Ok((guard, node_name, connect_ms, provider_resp, served_model)) => {
                // Receipts at dispatch (streamed usage is estimated): zero-
                // marginal when the winning node isn't a cloud provider.
                let zero_marginal = state
                    .config
                    .providers
                    .iter()
                    .find(|p| p.name == node_name)
                    .map(|p| p.tier != "cloud")
                    .unwrap_or(false);
                state.budget.record_receipt(
                    zero_marginal,
                    if auto_routed && zero_marginal { auto_counterfactual_usd } else { 0.0 },
                );
                if let Some(note) = &auto_note {
                    tracing::info!("stoke_auto (stream): {}", note);
                }
                // The meter owns the guard: it measures TTFT + tokens/sec into
                // the registry and keeps in-flight accurate for the stream's
                // whole life (client disconnect included).
                // Bill the stream only where tokens cost money. A provider on a
                // free tier is your own hardware: its usage report, if any, would
                // price at $0 anyway, and it was never asked for one.
                let serving_tier = state
                    .config
                    .providers
                    .iter()
                    .find(|p| p.name == node_name)
                    .map(|p| p.tier.clone())
                    .unwrap_or_default();
                let billing = (!cost::is_free_tier(&serving_tier)).then(|| StreamBilling {
                    budget: state.budget.clone(),
                    api_key: api_key.clone(),
                    // Only ever used if the provider reports nothing. ~4 chars/token.
                    prompt_tokens_est: (auto_route::extract_text(&req.messages).len() / 4) as u64,
                });

                let mut meter = StreamMeter::new(
                    guard,
                    state.nodes.clone(),
                    node_name,
                    served_model,
                    connect_ms,
                    billing,
                );
                let byte_stream = provider_resp.bytes_stream().map(move |chunk| {
                    if let Ok(bytes) = &chunk {
                        meter.on_chunk(bytes);
                    }
                    chunk
                });
                let body = axum::body::Body::from_stream(byte_stream);
                return Response::builder()
                    .header("Content-Type", "text/event-stream")
                    .header("Cache-Control", "no-cache")
                    .header("Connection", "keep-alive")
                    .body(body)
                    .unwrap();
            }
            Err((code, msg)) => {
                // Same policy-vs-upstream distinction as the non-streaming path.
                let code = if cost::is_unpriced_error(&msg) { StatusCode::FORBIDDEN } else { code };
                return (code, msg).into_response();
            }
        }
    }

    // Check cache for single routing at temp=0 (deterministic requests only)
    // Two-layer: exact match (hash) + semantic match (embedding similarity).
    // Scoped to (api_key, path): a cached response is a response the caller was
    // already authorised to receive, and nobody else.
    let cache_scope = ResponseCache::scope_of(&api_key, path);
    let cache_key = if routing == "single" && !req.stream.unwrap_or(false) {
        ResponseCache::cache_key(&cache_scope, &model, &req.messages, req.temperature, req.max_tokens)
    } else {
        None
    };

    let cache_prompt = ResponseCache::extract_prompt(&req.messages);

    if let Some(ref key) = cache_key {
        if let Some((_matched_key, cached)) = state.cache.get_smart(key, &cache_scope, &cache_prompt).await {
            tracing::info!("cache hit: key={}", &key[..8]);
            let mut response_json = cached;
            if let Some(obj) = response_json.as_object_mut() {
                obj.insert("stoke_cache".into(), json!("hit"));
            }
            return Json(response_json).into_response();
        }
    }

    // Placement decision detail for the response's stoke_route field (single routing)
    let mut route_note: Option<Value> = None;

    let result: Result<ProviderResult, (StatusCode, String)> = async {
    match routing.as_str() {
        "test_vote" => {
            if vote_models.is_empty() {
                return Err((StatusCode::BAD_REQUEST, "test_vote requires 'vote_models' field".to_string()));
            }
            if test_code.is_empty() || entry_point.is_empty() {
                return Err((StatusCode::BAD_REQUEST, "test_vote requires 'test_code' and 'entry_point' fields".to_string()));
            }
            let provider = provider_for_model_filtered(&state.config, &model, exclude_stoke)
                .ok_or_else(|| (StatusCode::NOT_FOUND, format!("No provider for model: {}", model)))?;
            test_vote_models(provider, &vote_models, &req, &test_code, &entry_point)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))
        }
        "cascade" => {
            let providers: Vec<_> = eligible_providers(&state.config, exclude_stoke);
            cascade(providers, &req)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))
        }
        "cascade_test" => {
            if vote_models.is_empty() {
                return Err((StatusCode::BAD_REQUEST, "cascade_test requires 'vote_models' field".to_string()));
            }
            if test_code.is_empty() || entry_point.is_empty() {
                return Err((StatusCode::BAD_REQUEST, "cascade_test requires 'test_code' and 'entry_point' fields".to_string()));
            }
            let provider = provider_for_model_filtered(&state.config, &model, exclude_stoke)
                .ok_or_else(|| (StatusCode::NOT_FOUND, format!("No provider for model: {}", model)))?;
            cascade_test_models(provider, &vote_models, &req, &test_code, &entry_point)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))
        }
        "self_consistency" => {
            let provider = provider_for_model_filtered(&state.config, &model, exclude_stoke)
                .ok_or_else(|| (StatusCode::NOT_FOUND, format!("No provider for model: {}", model)))?;
            self_consistency(provider, &model, &req, n_samples, sc_temperature)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e))
        }
        _ => {
            // Node-aware placement: rank candidates (warm > cold > unknown;
            // ties by tier, in-flight count, latency EWMA), try best-first.
            let (ranked, explain) = state.nodes.rank(&model, &state.config.providers, exclude_stoke);
            if ranked.is_empty() {
                return Err((
                    StatusCode::NOT_FOUND,
                    format!("No provider for model: {} ({})", model, explain.join("; ")),
                ));
            }
            let mut last_err = (StatusCode::BAD_GATEWAY, "no candidate attempted".to_string());
            let mut outcome = None;
            for provider in ranked.iter().take(3) {
                let _inflight = state.nodes.begin(&provider.name);
                match call_provider_hop(provider, &req, hop).await {
                    Ok(r) => {
                        state.nodes.record_success(&provider.name, r.elapsed_ms);
                        route_note = Some(json!({
                            "node": provider.name,
                            "candidates": explain,
                        }));
                        outcome = Some(r);
                        break;
                    }
                    Err(e) => {
                        let client_error = e.contains(" returned 4");
                        // A pricing refusal says nothing about the node's health —
                        // it was never contacted. Counting it as an error would
                        // demote a perfectly good node for a config omission.
                        let policy_refusal = cost::is_unpriced_error(&e);
                        if !client_error && !policy_refusal {
                            state.nodes.record_error(&provider.name);
                        }
                        tracing::warn!("placement: {} failed: {}", provider.name, e);
                        last_err = (StatusCode::BAD_GATEWAY, e);
                        if client_error {
                            break; // deterministic client error — retrying elsewhere just replays it
                        }
                    }
                }
            }
            outcome.ok_or(last_err)
        }
    }
    }.await;

    let result = match result {
        Ok(r) => r,
        Err((code, msg)) => {
            // The pricing gate refuses inside the router, where every failure
            // reads as a provider failure. It is not one — nothing upstream was
            // contacted. Answer 403 so an operator sees a policy refusal.
            let code = if cost::is_unpriced_error(&msg) { StatusCode::FORBIDDEN } else { code };
            return (code, msg).into_response();
        }
    };

    tracing::info!(
        "Response: model={}, provider={} ({}ms) cost=${:.6}",
        model,
        result.provider_name,
        result.elapsed_ms,
        result.cost.cost_usd
    );

    // Record spend the moment the money is known to be gone — before the response
    // hooks, before any transform, before any of the returns below. A plugin that
    // blocks, panics, or rewrites the body does not un-bill the provider, and a
    // cap that only debits on the happy path is a cap a retry loop walks straight
    // through. `result.cost` is the whole request's bill, including every call a
    // fan-out pattern made.
    state.budget.record_spend(&api_key, result.cost.cost_usd);

    // Inject cost into the response (non-standard field, ignored by OpenAI clients)
    let mut response_json = serde_json::to_value(&result.response).unwrap();
    if let Some(obj) = response_json.as_object_mut() {
        obj.insert(
            "stoke_cost".into(),
            serde_json::to_value(&result.cost).unwrap(),
        );
        obj.insert(
            "stoke_elapsed_ms".into(),
            json!(result.elapsed_ms),
        );
        obj.insert("stoke_cache".into(), json!("miss"));
        if let Some(mut note) = route_note.take() {
            if let Some(auto) = auto_note.take() {
                note["auto"] = auto;
            }
            obj.insert("stoke_route".into(), note);
        } else if let Some(auto) = auto_note.take() {
            obj.insert("stoke_route".into(), json!({ "auto": auto }));
        }
    }

    // Plugin: post_response — audit or transform output
    if state.plugins.has_post_response() {
        response_json = state
            .plugins
            .post_response(
                &model,
                &response_json,
                result.cost.cost_usd,
                result.elapsed_ms,
                &api_key,
            )
            .await;
    }

    // Built-in: post_response (code formatter + audit log)
    if let Some(ref profile) = route_profile {
        if profile.builtins.contains(&"code_formatter".to_string())
            || profile.builtins.contains(&"audit_log".to_string())
        {
            if let Some(transformed) = state
                .builtins
                .post_response(&model, &response_json, result.cost.cost_usd, result.elapsed_ms, &api_key)
                .await
            {
                response_json = transformed;
            }
        }
    }

    // JS plugin: post_response — audit or transform output
    #[cfg(feature = "js-plugins")]
    if !state.js_plugins.is_empty() {
        let js_result = tokio::task::spawn_blocking({
            let js_plugins = state.js_plugins.clone();
            let model = model.clone();
            let response = response_json.clone();
            let cost_usd = result.cost.cost_usd;
            let elapsed_ms = result.elapsed_ms;
            let api_key = api_key.clone();
            move || js_plugins.post_response(&model, &response, cost_usd, elapsed_ms, &api_key)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("JS post_response join error: {}", e);
            Err("JS plugin interrupted".to_string())
        });

        if let Ok(Some(transformed)) = js_result {
            response_json = transformed;
        }
    }

    // (spend was recorded above, before the response hooks could return early)

    // Receipts: zero-marginal is determined by the serving provider's tier
    // (not by cost_usd — an unpriced free-tier model prices at $0 and would lie here).
    let zero_marginal = state
        .config
        .providers
        .iter()
        .find(|p| p.name == result.provider_name)
        .map(|p| p.tier != "cloud")
        .unwrap_or(false);
    state.budget.record_receipt(
        zero_marginal,
        if auto_routed && zero_marginal { auto_counterfactual_usd } else { 0.0 },
    );

    // Store in cache if we have a key (single routing, temp=0)
    // Uses put_with_embedding to generate embedding for semantic cache
    if let Some(ref key) = cache_key {
        state.cache.put_with_embedding(key, &cache_scope, response_json.clone(), &cache_prompt).await;
    }

    Json(response_json).into_response()
}