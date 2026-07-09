//! Node registry: live view of every provider node for placement decisions.
//!
//! Polls Ollama-backed nodes (tier "local"/"remote") for their model inventory
//! (`/api/tags`) and warm state (`/api/ps`), tracks in-flight requests and a
//! latency EWMA per node, and ranks candidate providers for a given model:
//!
//!   warm on node > cold but present > capability unknown (fallback tail)
//!   ties: tier (local < remote < cloud), fewer in-flight, lower latency EWMA
//!
//! Cloud-tier providers are never polled (quota is scarce; health is learned
//! from real traffic). Every ranking produces human-readable reasons — routing
//! decisions must be inspectable (`/v1/nodes`, `stoke_route` response field).

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::config::{Config, ProviderConfig};

const EWMA_ALPHA: f64 = 0.3;
const POLL_TIMEOUT_SECS: u64 = 2;
/// Ceiling on a federated peer's self-reported in-flight count — a corrupted
/// or malicious peer must not be able to poison ranking arithmetic.
const REPORTED_INFLIGHT_CAP: usize = 10_000;

/// Ollama treats a bare model name as its ":latest" tag — "name" and
/// "name:latest" are the same model. Match accordingly.
fn matches_model(candidate: &str, requested: &str) -> bool {
    candidate == requested
        || candidate.strip_suffix(":latest").is_some_and(|c| c == requested)
        || requested.strip_suffix(":latest").is_some_and(|r| r == candidate)
}

#[derive(Debug, Clone, Default)]
pub struct NodeStatus {
    /// Node answered its last poll (or has never been polled: assumed healthy).
    pub healthy: bool,
    /// Models pulled on this node (from /api/tags, or a federated Stoke's /v1/nodes).
    pub models: HashSet<String>,
    /// Models currently loaded in memory (from /api/ps, or federated warm sets).
    pub warm: HashSet<String>,
    /// In-flight requests the node itself reports (federated Stoke nodes only).
    pub reported_inflight: usize,
    /// EWMA of request latency in ms (0.0 = no data yet).
    pub ewma_latency_ms: f64,
    pub errors: u64,
    pub polled: bool,
    /// Per-model static metadata (from /api/show; fetched lazily post-discovery).
    pub meta: HashMap<String, ModelMeta>,
    /// Per-model measured performance (EWMA from live streams).
    pub perf: HashMap<String, PerfStats>,
}

/// Static metadata for a model on a node, from Ollama /api/show.
#[derive(Debug, Clone, Default)]
pub struct ModelMeta {
    pub context_length: Option<u64>,
    pub tools: Option<bool>,
}

/// Measured performance for a model on a node, from live streams.
#[derive(Debug, Clone, Default)]
pub struct PerfStats {
    pub tps: f64,
    pub ttft_ms: f64,
}

/// How a provider's live state is discovered.
enum PollKind {
    /// Not pollable (cloud, or non-Ollama OpenAI-compatible server).
    None,
    /// Ollama node: poll {base}/api/tags + {base}/api/ps.
    Ollama(String),
    /// Federated Stoke gateway: poll {base}/nodes (base already ends in /v1).
    /// Carries the provider's resolved API key — the peer's status endpoints
    /// are fail-closed like everything else.
    Stoke { base: String, api_key: String },
}

struct NodeEntry {
    tier: String,
    is_stoke: bool,
    poll_kind: PollKind,
    status: RwLock<NodeStatus>,
    inflight: AtomicUsize,
    errors: AtomicU64,
}

pub struct NodeRegistry {
    nodes: HashMap<String, Arc<NodeEntry>>,
}

/// A model found on a healthy node — the raw material for auto-routing.
#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub name: String,
    pub node: String,
    /// 0 = local, 1 = remote, 2 = cloud/unknown (mirrors placement tiers).
    pub tier_rank: u8,
    pub warm: bool,
    /// From /api/show (None until fetched).
    pub context_length: Option<u64>,
    /// Structured tool_calls capability, from /api/show (None = unknown).
    pub tools: Option<bool>,
    /// Measured tokens/sec on this node (EWMA from live streams).
    pub tps: Option<f64>,
    /// Measured time-to-first-token on this node in ms (EWMA; includes prefill).
    pub ttft_ms: Option<f64>,
}

impl DiscoveredModel {
    /// Tag-normalized name match ("name" == "name:latest").
    pub fn matches(&self, requested: &str) -> bool {
        matches_model(&self.name, requested)
    }
}

/// RAII guard counting an in-flight request against a node.
pub struct InflightGuard {
    entry: Arc<NodeEntry>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.entry.inflight.fetch_sub(1, Ordering::Relaxed);
    }
}

fn tier_rank(tier: &str) -> u8 {
    match tier {
        "local" | "" => 0,
        "remote" => 1,
        _ => 2, // cloud and anything unknown routes last on ties
    }
}

/// Decide how to poll a provider for live state.
fn poll_kind(provider: &ProviderConfig) -> PollKind {
    if provider.r#type == "stoke" {
        // Federated Stoke gateway: its /v1/nodes gives richer state than raw
        // Ollama ever can (warmth AND in-flight load). Poll regardless of tier.
        return PollKind::Stoke {
            base: provider.base_url.trim_end_matches('/').to_string(),
            api_key: provider.resolve_api_key(),
        };
    }
    if provider.tier == "cloud" || provider.r#type == "anthropic" {
        return PollKind::None; // never poll cloud/anthropic: no Ollama-style status API
    }
    let trimmed = provider.base_url.trim_end_matches('/');
    match trimmed.strip_suffix("/v1") {
        Some(base) => PollKind::Ollama(base.to_string()),
        None => PollKind::None,
    }
}

impl NodeRegistry {
    pub fn from_config(config: &Config) -> Self {
        let nodes = config
            .providers
            .iter()
            .map(|p| {
                let entry = NodeEntry {
                    tier: p.tier.clone(),
                    is_stoke: p.r#type == "stoke",
                    poll_kind: poll_kind(p),
                    status: RwLock::new(NodeStatus {
                        healthy: true, // optimistic until a poll says otherwise
                        ..Default::default()
                    }),
                    inflight: AtomicUsize::new(0),
                    errors: AtomicU64::new(0),
                };
                (p.name.clone(), Arc::new(entry))
            })
            .collect();
        Self { nodes }
    }

    /// Poll all pollable nodes once. Called from the background poller.
    pub async fn poll_once(&self) {
        for (name, entry) in &self.nodes {
            let polled = match &entry.poll_kind {
                PollKind::None => continue,
                PollKind::Ollama(base) => {
                    let (models, warm, ok) = poll_ollama_node(base).await;
                    (models, warm, 0, ok, HashMap::new(), HashMap::new())
                }
                PollKind::Stoke { base, api_key } => poll_stoke_node(base, api_key).await,
            };
            let (models, warm, reported_inflight, ok, fed_meta, fed_perf) = polled;
            // Lazy /api/show: fetch metadata for a couple of not-yet-known
            // models per cycle (avoids a burst on big libraries).
            let mut fetched_meta: Vec<(String, ModelMeta)> = Vec::new();
            if ok {
                if let PollKind::Ollama(base) = &entry.poll_kind {
                    let unknown: Vec<String> = {
                        let status = entry.status.read().unwrap();
                        models
                            .iter()
                            .filter(|m| !status.meta.contains_key(*m))
                            .take(2)
                            .cloned()
                            .collect()
                    };
                    for m in unknown {
                        if let Some(meta) = fetch_model_meta(base, &m).await {
                            fetched_meta.push((m, meta));
                        }
                    }
                }
            }
            let mut status = entry.status.write().unwrap();
            status.polled = true;
            status.healthy = ok;
            if ok {
                status.models = models;
                status.warm = warm;
                status.reported_inflight = reported_inflight;
                for (m, meta) in fetched_meta {
                    status.meta.insert(m, meta);
                }
                // Federated peers report their own meta/perf — adopt it.
                for (m, meta) in fed_meta {
                    status.meta.insert(m, meta);
                }
                for (m, perf) in fed_perf {
                    status.perf.insert(m, perf);
                }
            } else {
                status.warm.clear();
                status.reported_inflight = 0;
                tracing::debug!("node poll failed: {}", name);
            }
        }
    }

    /// Rank providers able to serve `model`, best first, with reasons.
    ///
    /// Returns (ranked providers, per-provider reason strings). Providers that
    /// are polled-unhealthy or verifiably don't have the model are excluded
    /// (their reason still appears in the explain list).
    ///
    /// `exclude_stoke`: set when the incoming request already crossed a Stoke
    /// (x-stoke-hop >= 1) — federated providers are excluded so a request can
    /// never loop between gateways. Federation depth is exactly one hop.
    pub fn rank<'a>(
        &self,
        model: &str,
        providers: &'a [ProviderConfig],
        exclude_stoke: bool,
    ) -> (Vec<&'a ProviderConfig>, Vec<String>) {
        let mut explain = Vec::new();
        // (provider, score, tier_rank, inflight, ewma)
        let mut candidates: Vec<(&ProviderConfig, u8, u8, usize, f64)> = Vec::new();

        for p in providers {
            if exclude_stoke && p.r#type == "stoke" {
                explain.push(format!("{}: excluded (hop guard)", p.name));
                continue;
            }
            let entry = self.nodes.get(&p.name);
            let listed = p.models.iter().any(|m| matches_model(m, model));
            let (status, local_inflight) = match entry {
                Some(e) => (e.status.read().unwrap().clone(), e.inflight.load(Ordering::Relaxed)),
                None => (NodeStatus { healthy: true, ..Default::default() }, 0),
            };
            // Federated nodes report their own in-flight, which INCLUDES the
            // requests we forwarded (our local guards) once their state is
            // polled — summing would double-count. max() keeps both signals:
            // local covers poll lag, reported covers the peer's other clients.
            let inflight = if p.r#type == "stoke" {
                local_inflight.max(status.reported_inflight.min(REPORTED_INFLIGHT_CAP))
            } else {
                local_inflight
            };

            if status.polled && !status.healthy {
                explain.push(format!("{}: excluded (unreachable)", p.name));
                continue;
            }

            let discovered = status.models.iter().any(|m| matches_model(m, model));
            let warm = status.warm.iter().any(|m| matches_model(m, model));

            // Score: warm > cold-but-present > unknown capability.
            // Only a POLLED node that lacks the model is verifiably unable to
            // serve it — exclude. An unpolled provider (cloud, non-Ollama) is
            // kept as an unknown-capability fallback tail even when its
            // configured list doesn't mention the model: partial lists are
            // common and the old first-provider fallback relied on this.
            let score = if warm {
                3
            } else if discovered || listed {
                2
            } else if status.polled {
                explain.push(format!("{}: excluded (model not present)", p.name));
                continue;
            } else {
                1
            };

            let state_word = match score {
                3 => "warm",
                2 => "cold",
                _ => "unknown",
            };
            explain.push(format!(
                "{}: {} (tier={}, inflight={}, ttft~{:.0}ms)",
                p.name,
                state_word,
                if p.tier.is_empty() { "local" } else { &p.tier },
                inflight,
                status.ewma_latency_ms
            ));
            candidates.push((p, score, tier_rank(&p.tier), inflight, status.ewma_latency_ms));
        }

        candidates.sort_by(|a, b| {
            b.1.cmp(&a.1) // score desc
                .then(a.2.cmp(&b.2)) // tier asc (local first)
                .then(a.3.cmp(&b.3)) // inflight asc
                .then(a.4.partial_cmp(&b.4).unwrap_or(std::cmp::Ordering::Equal)) // ewma asc
        });

        (candidates.into_iter().map(|c| c.0).collect(), explain)
    }

    /// Count a request in-flight against a node for the guard's lifetime.
    pub fn begin(&self, name: &str) -> Option<InflightGuard> {
        self.nodes.get(name).map(|e| {
            e.inflight.fetch_add(1, Ordering::Relaxed);
            InflightGuard { entry: e.clone() }
        })
    }

    pub fn record_success(&self, name: &str, elapsed_ms: u64) {
        if let Some(e) = self.nodes.get(name) {
            let mut status = e.status.write().unwrap();
            status.ewma_latency_ms = if status.ewma_latency_ms == 0.0 {
                elapsed_ms as f64
            } else {
                EWMA_ALPHA * elapsed_ms as f64 + (1.0 - EWMA_ALPHA) * status.ewma_latency_ms
            };
        }
    }

    pub fn record_error(&self, name: &str) {
        if let Some(e) = self.nodes.get(name) {
            e.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Every model discovered on a healthy node, in stable order
    /// (tier, node name, model name). Feeds auto-route resolution —
    /// Stoke ships no model names of its own.
    pub fn discovered(&self) -> Vec<DiscoveredModel> {
        let mut out = Vec::new();
        for (name, e) in &self.nodes {
            let status = e.status.read().unwrap();
            if status.polled && !status.healthy {
                continue;
            }
            for m in &status.models {
                let meta = status.meta.get(m);
                let perf = status.perf.get(m);
                out.push(DiscoveredModel {
                    name: m.clone(),
                    node: name.clone(),
                    tier_rank: tier_rank(&e.tier),
                    warm: status.warm.contains(m),
                    context_length: meta.and_then(|x| x.context_length),
                    tools: meta.and_then(|x| x.tools),
                    tps: perf.map(|p| p.tps).filter(|v| *v > 0.0),
                    ttft_ms: perf.map(|p| p.ttft_ms).filter(|v| *v > 0.0),
                });
            }
        }
        out.sort_by(|a, b| {
            (a.tier_rank, &a.node, &a.name).cmp(&(b.tier_rank, &b.node, &b.name))
        });
        out
    }

    /// Record measured stream performance for (node, model): time-to-first-
    /// token and tokens/sec, EWMA-smoothed. Called by the stream meter.
    ///
    /// The perf map is keyed by the node's own inventory name (e.g.
    /// `qwen3:latest`), which is how `discovered()`/`snapshot()` read it back.
    /// The requested model may be the bare tag (`qwen3`), so we resolve it to
    /// the actual inventory name first — otherwise the EWMA is written to a key
    /// that is never read and the measurement silently does nothing.
    pub fn record_stream_stats(&self, node: &str, model: &str, ttft_ms: f64, tokens: u64, gen_ms: f64) {
        let Some(e) = self.nodes.get(node) else { return };
        let mut status = e.status.write().unwrap();
        let key = status
            .models
            .iter()
            .find(|m| matches_model(m, model))
            .cloned()
            .unwrap_or_else(|| model.to_string());
        let entry = status.perf.entry(key).or_default();
        let mix = |old: f64, new: f64| if old <= 0.0 { new } else { EWMA_ALPHA * new + (1.0 - EWMA_ALPHA) * old };
        if ttft_ms > 0.0 {
            entry.ttft_ms = mix(entry.ttft_ms, ttft_ms);
        }
        if tokens > 1 && gen_ms > 0.0 {
            let tps = tokens as f64 / (gen_ms / 1000.0);
            entry.tps = mix(entry.tps, tps);
        }
    }

    /// JSON snapshot for the /v1/nodes endpoint.
    pub fn snapshot(&self) -> Value {
        let nodes: Vec<Value> = self
            .nodes
            .iter()
            .map(|(name, e)| {
                let status = e.status.read().unwrap();
                let mut models: Vec<&String> = status.models.iter().collect();
                models.sort();
                let mut warm: Vec<&String> = status.warm.iter().collect();
                warm.sort();
                let local_inflight = e.inflight.load(Ordering::Relaxed);
                // federated peers already count our forwarded requests: max, not sum
                let effective_inflight = if e.is_stoke {
                    local_inflight.max(status.reported_inflight.min(REPORTED_INFLIGHT_CAP))
                } else {
                    local_inflight
                };
                // Per-model meta + measured perf: lets federated peers make
                // predictions about our models without probing them.
                let models_meta: serde_json::Map<String, Value> = status
                    .models
                    .iter()
                    .filter_map(|m| {
                        let meta = status.meta.get(m);
                        let perf = status.perf.get(m);
                        if meta.is_none() && perf.is_none() {
                            return None;
                        }
                        Some((m.clone(), json!({
                            "context_length": meta.and_then(|x| x.context_length),
                            "tools": meta.and_then(|x| x.tools),
                            "tps": perf.map(|p| p.tps).filter(|v| *v > 0.0),
                            "ttft_ms": perf.map(|p| p.ttft_ms).filter(|v| *v > 0.0),
                        })))
                    })
                    .collect();
                json!({
                    "name": name,
                    "type": if e.is_stoke { "stoke" } else { "direct" },
                    "tier": if e.tier.is_empty() { "local" } else { &e.tier },
                    "pollable": !matches!(e.poll_kind, PollKind::None),
                    "polled": status.polled,
                    "healthy": status.healthy,
                    "models": models,
                    "warm": warm,
                    "models_meta": models_meta,
                    "inflight": effective_inflight,
                    "ewma_latency_ms": status.ewma_latency_ms,
                    "errors": e.errors.load(Ordering::Relaxed),
                })
            })
            .collect();
        json!({ "nodes": nodes })
    }
}

/// Poll a federated Stoke gateway: {base}/nodes (base ends in /v1).
/// Aggregates the remote's DIRECT nodes only — its own federated entries are
/// skipped so status can't echo back through a cycle (depth-1 federation).
async fn poll_stoke_node(
    base: &str,
    api_key: &str,
) -> (HashSet<String>, HashSet<String>, usize, bool, HashMap<String, ModelMeta>, HashMap<String, PerfStats>) {
    let client = &*crate::router::SHARED_CLIENT;
    let mut req = client
        .get(format!("{}/nodes", base))
        .timeout(Duration::from_secs(POLL_TIMEOUT_SECS));
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().await;
    let body: Option<Value> = match resp {
        Ok(r) if r.status().is_success() => r.json().await.ok(),
        _ => None,
    };
    let Some(body) = body else {
        return (HashSet::new(), HashSet::new(), 0, false, HashMap::new(), HashMap::new());
    };
    let (models, warm, inflight, meta, perf) = aggregate_stoke_snapshot(&body);
    (models, warm, inflight, true, meta, perf)
}

/// Aggregate a remote Stoke /v1/nodes snapshot into (models, warm, inflight,
/// meta, perf). Only healthy, non-federated ("direct") remote nodes count.
fn aggregate_stoke_snapshot(
    v: &Value,
) -> (HashSet<String>, HashSet<String>, usize, HashMap<String, ModelMeta>, HashMap<String, PerfStats>) {
    let mut models = HashSet::new();
    let mut warm = HashSet::new();
    let mut inflight = 0usize;
    let mut meta: HashMap<String, ModelMeta> = HashMap::new();
    let mut perf: HashMap<String, PerfStats> = HashMap::new();
    if let Some(nodes) = v.get("nodes").and_then(|n| n.as_array()) {
        for n in nodes {
            let is_stoke = n.get("type").and_then(|t| t.as_str()) == Some("stoke");
            let healthy = n.get("healthy").and_then(|h| h.as_bool()).unwrap_or(false);
            if is_stoke || !healthy {
                continue;
            }
            for key in ["models", "warm"] {
                if let Some(arr) = n.get(key).and_then(|m| m.as_array()) {
                    for m in arr.iter().filter_map(|m| m.as_str()) {
                        if key == "warm" {
                            warm.insert(m.to_string());
                        } else {
                            models.insert(m.to_string());
                        }
                    }
                }
            }
            // Adopt the peer's per-model meta + measured perf when reported.
            if let Some(mm) = n.get("models_meta").and_then(|m| m.as_object()) {
                for (model, info) in mm {
                    let ctx = info.get("context_length").and_then(|c| c.as_u64());
                    let tools = info.get("tools").and_then(|t| t.as_bool());
                    if ctx.is_some() || tools.is_some() {
                        let e = meta.entry(model.clone()).or_default();
                        if ctx.is_some() {
                            e.context_length = ctx;
                        }
                        if tools.is_some() {
                            e.tools = tools;
                        }
                    }
                    let tps = info.get("tps").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    let ttft = info.get("ttft_ms").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    if tps > 0.0 || ttft > 0.0 {
                        let p = perf.entry(model.clone()).or_default();
                        if tps > 0.0 {
                            p.tps = tps;
                        }
                        if ttft > 0.0 {
                            p.ttft_ms = ttft;
                        }
                    }
                }
            }
            let reported = n.get("inflight").and_then(|i| i.as_u64()).unwrap_or(0);
            inflight = inflight
                .saturating_add(reported.min(REPORTED_INFLIGHT_CAP as u64) as usize)
                .min(REPORTED_INFLIGHT_CAP);
        }
    }
    // warm models are implicitly present
    models.extend(warm.iter().cloned());
    (models, warm, inflight, meta, perf)
}

/// Fetch static model metadata from Ollama /api/show: context window and
/// declared capabilities (notably structured tool_calls support).
async fn fetch_model_meta(base: &str, model: &str) -> Option<ModelMeta> {
    let client = &*crate::router::SHARED_CLIENT;
    let resp = client
        .post(format!("{}/api/show", base))
        .timeout(Duration::from_secs(POLL_TIMEOUT_SECS))
        .json(&json!({ "model": model }))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    // context_length lives in model_info under an architecture-prefixed key,
    // e.g. "llama.context_length" — match by suffix.
    let context_length = v.get("model_info").and_then(|mi| mi.as_object()).and_then(|mi| {
        mi.iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, val)| val.as_u64())
    });
    let tools = v
        .get("capabilities")
        .and_then(|c| c.as_array())
        .map(|caps| caps.iter().any(|c| c.as_str() == Some("tools")));
    Some(ModelMeta { context_length, tools })
}

/// Poll one Ollama node: /api/tags (inventory) + /api/ps (warm). 2s timeout each.
async fn poll_ollama_node(base: &str) -> (HashSet<String>, HashSet<String>, bool) {
    let client = &*crate::router::SHARED_CLIENT;
    let timeout = Duration::from_secs(POLL_TIMEOUT_SECS);

    let tags: Option<Value> = match client
        .get(format!("{}/api/tags", base))
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.json().await.ok(),
        _ => None,
    };
    let Some(tags) = tags else {
        return (HashSet::new(), HashSet::new(), false);
    };

    let models = extract_model_names(&tags);

    let warm = match client
        .get(format!("{}/api/ps", base))
        .timeout(timeout)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r
            .json()
            .await
            .ok()
            .map(|v: Value| extract_model_names(&v))
            .unwrap_or_default(),
        _ => HashSet::new(),
    };

    (models, warm, true)
}

/// Both /api/tags and /api/ps return { "models": [ { "name": "..." }, ... ] }.
fn extract_model_names(v: &Value) -> HashSet<String> {
    v.get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Spawn the background poll loop. Interval via STOKE_NODE_POLL_SECS (default 10).
pub fn spawn_poller(registry: Arc<NodeRegistry>) {
    let interval_secs: u64 = std::env::var("STOKE_NODE_POLL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    tokio::spawn(async move {
        loop {
            registry.poll_once().await;
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(name: &str, tier: &str, models: &[&str]) -> ProviderConfig {
        ProviderConfig {
            name: name.into(),
            r#type: "openai_compatible".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key: String::new(),
            api_key_env: String::new(),
            models: models.iter().map(|s| s.to_string()).collect(),
            tier: tier.into(),
        }
    }

    fn registry_with(providers: &[ProviderConfig]) -> NodeRegistry {
        let config = Config {
            server: crate::config::ServerConfig { host: "h".into(), port: 1 },
            routing: None,
            default_model: None,
            providers: providers.to_vec(),
            plugins: Default::default(),
            builtins: Default::default(),
            routes: vec![],
            keys: vec![],
            auto_route: Default::default(),
        };
        NodeRegistry::from_config(&config)
    }

    fn set_status(reg: &NodeRegistry, name: &str, f: impl FnOnce(&mut NodeStatus)) {
        let entry = reg.nodes.get(name).unwrap();
        f(&mut entry.status.write().unwrap());
    }

    #[test]
    fn warm_beats_cold() {
        let providers = vec![
            provider("laptop", "local", &[]),
            provider("studio", "remote", &[]),
        ];
        let reg = registry_with(&providers);
        set_status(&reg, "laptop", |s| {
            s.polled = true;
            s.models = ["m1".to_string()].into();
        });
        set_status(&reg, "studio", |s| {
            s.polled = true;
            s.models = ["m1".to_string()].into();
            s.warm = ["m1".to_string()].into();
        });
        let (ranked, _) = reg.rank("m1", &providers, false);
        assert_eq!(ranked[0].name, "studio"); // warm remote beats cold local
        assert_eq!(ranked[1].name, "laptop");
    }

    #[test]
    fn inflight_breaks_warm_ties() {
        let providers = vec![
            provider("laptop", "local", &[]),
            provider("studio", "local", &[]),
        ];
        let reg = registry_with(&providers);
        for name in ["laptop", "studio"] {
            set_status(&reg, name, |s| {
                s.polled = true;
                s.models = ["m1".to_string()].into();
                s.warm = ["m1".to_string()].into();
            });
        }
        let _g1 = reg.begin("laptop");
        let _g2 = reg.begin("laptop");
        let _g3 = reg.begin("studio");
        let (ranked, _) = reg.rank("m1", &providers, false);
        assert_eq!(ranked[0].name, "studio"); // fewer in-flight wins
    }

    #[test]
    fn polled_node_without_model_is_excluded_but_unknown_cloud_is_fallback() {
        let mut cloud = provider("cloud", "cloud", &[]);
        cloud.base_url = "https://api.example.com/v1".into();
        let providers = vec![provider("laptop", "local", &[]), cloud];
        let reg = registry_with(&providers);
        set_status(&reg, "laptop", |s| {
            s.polled = true;
            s.models = ["other".to_string()].into();
        });
        let (ranked, explain) = reg.rank("m1", &providers, false);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].name, "cloud");
        assert!(explain.iter().any(|e| e.contains("laptop: excluded")));
    }

    #[test]
    fn unhealthy_nodes_are_excluded() {
        let providers = vec![provider("laptop", "local", &["m1"])];
        let reg = registry_with(&providers);
        set_status(&reg, "laptop", |s| {
            s.polled = true;
            s.healthy = false;
        });
        let (ranked, _) = reg.rank("m1", &providers, false);
        assert!(ranked.is_empty());
    }

    #[test]
    fn explicit_model_list_counts_as_present() {
        let providers = vec![provider("laptop", "local", &["m1"])];
        let reg = registry_with(&providers);
        // never polled — listed model should still rank as cold-present
        let (ranked, explain) = reg.rank("m1", &providers, false);
        assert_eq!(ranked.len(), 1);
        assert!(explain[0].contains("cold"));
    }

    #[test]
    fn inflight_guard_decrements_on_drop() {
        let providers = vec![provider("laptop", "local", &["m1"])];
        let reg = registry_with(&providers);
        {
            let _g = reg.begin("laptop");
            let entry = reg.nodes.get("laptop").unwrap();
            assert_eq!(entry.inflight.load(Ordering::Relaxed), 1);
        }
        let entry = reg.nodes.get("laptop").unwrap();
        assert_eq!(entry.inflight.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn poll_kind_derivation() {
        let p = provider("laptop", "local", &[]);
        assert!(matches!(poll_kind(&p), PollKind::Ollama(b) if b == "http://127.0.0.1:11434"));
        let mut cloud = provider("c", "cloud", &[]);
        cloud.base_url = "https://x.com/v1".into();
        assert!(matches!(poll_kind(&cloud), PollKind::None));
        let mut weird = provider("w", "local", &[]);
        weird.base_url = "http://host:9000".into(); // no /v1 suffix — not pollable
        assert!(matches!(poll_kind(&weird), PollKind::None));
        let mut fed = provider("fed", "remote", &[]);
        fed.r#type = "stoke".into();
        fed.base_url = "http://192.168.1.33:8787/v1".into();
        fed.api_key = "stk-test".into();
        assert!(matches!(poll_kind(&fed),
            PollKind::Stoke { base, api_key } if base == "http://192.168.1.33:8787/v1" && api_key == "stk-test"));
    }

    #[test]
    fn hop_guard_excludes_stoke_providers() {
        let mut fed = provider("fed-b", "remote", &[]);
        fed.r#type = "stoke".into();
        let providers = vec![provider("laptop", "local", &["m1"]), fed];
        let reg = registry_with(&providers);
        // fed-b would normally be an unknown-capability fallback candidate
        let (ranked, _) = reg.rank("m1", &providers, false);
        assert_eq!(ranked.len(), 2);
        // with hop guard on, it must disappear with an explain entry
        let (ranked, explain) = reg.rank("m1", &providers, true);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].name, "laptop");
        assert!(explain.iter().any(|e| e.contains("fed-b: excluded (hop guard)")));
    }

    #[test]
    fn reported_inflight_breaks_ties() {
        let mut fed1 = provider("fed-1", "remote", &[]);
        fed1.r#type = "stoke".into();
        let mut fed2 = provider("fed-2", "remote", &[]);
        fed2.r#type = "stoke".into();
        let providers = vec![fed1, fed2];
        let reg = registry_with(&providers);
        for (name, load) in [("fed-1", 4usize), ("fed-2", 1usize)] {
            set_status(&reg, name, |s| {
                s.polled = true;
                s.models = ["m1".to_string()].into();
                s.warm = ["m1".to_string()].into();
                s.reported_inflight = load;
            });
        }
        let (ranked, _) = reg.rank("m1", &providers, false);
        assert_eq!(ranked[0].name, "fed-2"); // lower remote-reported load wins
    }

    #[test]
    fn bare_model_name_matches_latest_tag() {
        // Ollama accepts a bare name for its :latest tag — placement must too
        let providers = vec![provider("laptop", "local", &[])];
        let reg = registry_with(&providers);
        set_status(&reg, "laptop", |s| {
            s.polled = true;
            s.models = ["somemodel:latest".to_string()].into();
            s.warm = ["somemodel:latest".to_string()].into();
        });
        let (ranked, explain) = reg.rank("somemodel", &providers, false);
        assert_eq!(ranked.len(), 1, "bare name must match :latest tag: {:?}", explain);
        assert!(explain[0].contains("warm"), "warm detection must normalize tags too");
        // and the reverse: tagged request, bare configured list
        let providers2 = vec![provider("laptop2", "local", &["somemodel"])];
        let reg2 = registry_with(&providers2);
        let (ranked2, _) = reg2.rank("somemodel:latest", &providers2, false);
        assert_eq!(ranked2.len(), 1);
    }

    #[test]
    fn unpolled_partial_list_is_fallback_not_excluded() {
        // Old provider_for_model fell back to the first provider even when its
        // explicit list didn't mention the model; unpolled providers must keep
        // that unknown-capability fallback role.
        let mut cloud = provider("cloud", "cloud", &["gpt-4o"]);
        cloud.base_url = "https://api.example.com/v1".into();
        let providers = vec![cloud];
        let reg = registry_with(&providers);
        let (ranked, explain) = reg.rank("gpt-4o-mini", &providers, false);
        assert_eq!(ranked.len(), 1, "unpolled partial list must not exclude: {:?}", explain);
        assert!(explain[0].contains("unknown"));
    }

    #[test]
    fn federated_inflight_uses_max_not_sum() {
        // Our forwarded requests are counted locally AND show up in the peer's
        // report once polled — summing would double-count.
        let mut fed = provider("fed", "remote", &[]);
        fed.r#type = "stoke".into();
        let providers = vec![fed];
        let reg = registry_with(&providers);
        set_status(&reg, "fed", |s| {
            s.polled = true;
            s.models = ["m1".to_string()].into();
            s.warm = ["m1".to_string()].into();
            s.reported_inflight = 5;
        });
        let _g1 = reg.begin("fed");
        let _g2 = reg.begin("fed"); // 2 local, 5 reported -> effective 5, not 7
        let snap = reg.snapshot();
        let inflight = snap["nodes"][0]["inflight"].as_u64().unwrap();
        assert_eq!(inflight, 5, "expected max(2,5)=5, got {}", inflight);
    }

    #[test]
    fn aggregate_clamps_hostile_inflight() {
        let snapshot = serde_json::json!({
            "nodes": [
                { "name": "n1", "type": "direct", "healthy": true,
                  "models": [], "warm": [], "inflight": u64::MAX },
                { "name": "n2", "type": "direct", "healthy": true,
                  "models": [], "warm": [], "inflight": u64::MAX }
            ]
        });
        let (_, _, inflight, _, _) = aggregate_stoke_snapshot(&snapshot); // must not panic
        assert!(inflight <= REPORTED_INFLIGHT_CAP);
    }

    #[test]
    fn aggregate_snapshot_skips_federated_and_unhealthy() {
        let snapshot = serde_json::json!({
            "nodes": [
                { "name": "n1", "type": "direct", "healthy": true,
                  "models": ["a"], "warm": ["b"], "inflight": 2 },
                { "name": "dead", "type": "direct", "healthy": false,
                  "models": ["x"], "warm": [], "inflight": 9 },
                { "name": "fed", "type": "stoke", "healthy": true,
                  "models": ["echo"], "warm": ["echo"], "inflight": 7 }
            ]
        });
        let (models, warm, inflight, _meta, _perf) = aggregate_stoke_snapshot(&snapshot);
        assert!(models.contains("a") && models.contains("b")); // warm implies present
        assert!(!models.contains("x"), "unhealthy node's models must not count");
        assert!(!models.contains("echo"), "federated entries must not echo through");
        assert_eq!(warm, ["b".to_string()].into());
        assert_eq!(inflight, 2);
    }
}
