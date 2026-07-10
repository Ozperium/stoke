use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use serde_json::{json, Value};

/// Budget caps: track spend per API key, reject requests when over limit.
/// Also handles rate limiting (requests per minute per key).
/// Also detects agent loops (repeated identical OR semantically similar requests).

/// Loop detection: two layers.
/// 1. Exact: same prompt hash > threshold times in window → block.
/// 2. Semantic: prompts with embedding cosine similarity > threshold → count as "same".
/// This catches agent loops where an LLM retries with slightly different wording
/// or oscillates between similar reasoning states — the LoopGuard insight.

const DEFAULT_LOOP_THRESHOLD: usize = 5;
const DEFAULT_LOOP_WINDOW_SECS: u64 = 60;
const DEFAULT_LOOP_BLOCK_SECS: u64 = 120;
const DEFAULT_SEMANTIC_THRESHOLD: f32 = 0.85;

/// A tracked prompt entry for loop detection.
struct PromptEntry {
    hash: String,
    embedding: Vec<f32>,
    timestamp: Instant,
}

pub struct BudgetGuard {
    /// API key -> cumulative spend in USD
    spend: RwLock<HashMap<String, f64>>,
    /// The share of `spend` that Stoke estimated rather than read from a
    /// provider's usage report. Counted against the cap all the same — a
    /// provider that hides its usage must not thereby disable the cap — but
    /// surfaced separately so nobody mistakes an estimate for a measurement.
    estimated: RwLock<HashMap<String, f64>>,
    /// API key -> budget limit in USD (0 = unlimited)
    limits: RwLock<HashMap<String, f64>>,
    /// API key -> list of request timestamps (for rate limiting)
    request_times: RwLock<HashMap<String, Vec<Instant>>>,
    /// API key -> max requests per minute (0 = unlimited)
    rate_limits: RwLock<HashMap<String, u32>>,
    /// Honest savings receipts: requests served, zero-marginal share, and the
    /// list-price counterfactual of the configured quality model (estimates).
    receipts: RwLock<ReceiptTotals>,
    /// Loop detection: API key -> list of prompt entries (hash + embedding + timestamp)
    request_history: RwLock<HashMap<String, Vec<PromptEntry>>>,
    /// Loop detection: API key -> blocked until this Instant
    loop_blocked: RwLock<HashMap<String, Instant>>,
    /// Loop detection config
    loop_threshold: usize,
    loop_window: Duration,
    loop_block_duration: Duration,
    /// Semantic loop detection: similarity threshold for "same" prompt
    semantic_threshold: f32,
    /// Whether semantic loop detection is enabled (requires embedding model)
    semantic_enabled: bool,
    /// Ollama base URL for embedding generation
    ollama_url: String,
    /// Embedding model name
    embedding_model: String,
}

impl BudgetGuard {
    pub fn new() -> Self {
        Self {
            spend: RwLock::new(HashMap::new()),
            estimated: RwLock::new(HashMap::new()),
            limits: RwLock::new(HashMap::new()),
            request_times: RwLock::new(HashMap::new()),
            rate_limits: RwLock::new(HashMap::new()),
            receipts: RwLock::new(ReceiptTotals::default()),
            request_history: RwLock::new(HashMap::new()),
            loop_blocked: RwLock::new(HashMap::new()),
            loop_threshold: DEFAULT_LOOP_THRESHOLD,
            loop_window: Duration::from_secs(DEFAULT_LOOP_WINDOW_SECS),
            loop_block_duration: Duration::from_secs(DEFAULT_LOOP_BLOCK_SECS),
            semantic_threshold: std::env::var("STOKE_LOOP_SEMANTIC_THRESHOLD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_SEMANTIC_THRESHOLD),
            // Semantic detection needs an explicit embedding model — Stoke
            // ships no model names. Enabled without one → disabled + warning.
            semantic_enabled: {
                let wanted = std::env::var("STOKE_SEMANTIC_CACHE").is_ok();
                let has_model = std::env::var("STOKE_EMBED_MODEL").map(|m| !m.is_empty()).unwrap_or(false);
                if wanted && !has_model {
                    tracing::warn!(
                        "semantic loop detection disabled: STOKE_SEMANTIC_CACHE is set but \
                         STOKE_EMBED_MODEL is not. Set STOKE_EMBED_MODEL to an embedding \
                         model available on your Ollama."
                    );
                }
                wanted && has_model
            },
            ollama_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string()),
            embedding_model: std::env::var("STOKE_EMBED_MODEL").unwrap_or_default(),
        }
    }

    /// Record a receipt for a completed request. `avoided_usd_est` is the
    /// list-price counterfactual of the user's configured quality model —
    /// accumulated only for auto-routed requests served at zero marginal cost,
    /// and always an estimate, never a quality-equivalence claim.
    pub fn record_receipt(&self, zero_marginal: bool, avoided_usd_est: f64) {
        let mut r = self.receipts.write().unwrap();
        r.requests += 1;
        if zero_marginal {
            r.zero_marginal += 1;
            r.avoided_usd_est += avoided_usd_est.max(0.0);
        }
    }

    /// (requests, zero_marginal_requests, avoided_usd_est)
    pub fn receipts(&self) -> (u64, u64, f64) {
        let r = self.receipts.read().unwrap();
        (r.requests, r.zero_marginal, r.avoided_usd_est)
    }

    /// Set a budget limit for an API key. Applied at startup from [[keys]] config.
    pub fn set_budget(&self, key: &str, limit_usd: f64) {
        self.limits
            .write()
            .unwrap()
            .insert(key.to_string(), limit_usd);
    }

    /// Set a rate limit for an API key (requests per minute).
    pub fn set_rate_limit(&self, key: &str, rpm: u32) {
        self.rate_limits
            .write()
            .unwrap()
            .insert(key.to_string(), rpm);
    }

    /// Check if a request is allowed. Returns Ok(()) or Err(reason).
    /// Without loop detection (no prompt hash).
    pub async fn check(&self, key: &str) -> Result<(), String> {
        self.check_with_prompt(key, "", "").await
    }

    /// Check if a request is allowed, with loop detection.
    /// `prompt_hash` is a hash of the request prompt (model + messages + temp).
    /// `prompt_text` is the raw prompt text for embedding generation.
    /// If the same key sends the same prompt hash > threshold times in the window,
    /// the key is blocked for `loop_block_duration`.
    /// If semantic detection is enabled, also counts semantically similar prompts
    /// (cosine similarity > threshold) toward the loop counter.
    pub async fn check_with_prompt(
        &self,
        key: &str,
        prompt_hash: &str,
        prompt_text: &str,
    ) -> Result<(), String> {
        // Check if key is currently loop-blocked
        {
            let blocked = self.loop_blocked.read().unwrap();
            if let Some(until) = blocked.get(key) {
                if Instant::now() < *until {
                    let remaining = until.duration_since(Instant::now()).as_secs();
                    return Err(format!(
                        "Loop detected: key {} blocked for {}s (repeated similar requests). \
                         Review your agent's retry logic.",
                        &key[..8.min(key.len())],
                        remaining
                    ));
                }
            }
        }
        // Expired block — clean up
        {
            let mut blocked = self.loop_blocked.write().unwrap();
            blocked.retain(|_, until| Instant::now() < *until);
        }

        // Check budget + rate limit in a separate scope so guards are dropped
        // before any .await below (RwLock guards are !Send).
        {
            let limits = self.limits.read().unwrap();
            let limit = limits.get(key).copied().unwrap_or(0.0);
            if limit > 0.0 {
                let spend = self.spend.read().unwrap();
                let current = spend.get(key).copied().unwrap_or(0.0);
                if current >= limit {
                    return Err(format!(
                        "Budget exceeded: ${:.4}/${:.4} for key {}",
                        current,
                        limit,
                        &key[..8.min(key.len())]
                    ));
                }
            }
        }

        {
            let rate_limits = self.rate_limits.read().unwrap();
            let rpm = rate_limits.get(key).copied().unwrap_or(0);
            if rpm > 0 {
                let mut times = self.request_times.write().unwrap();
                let entries = times.entry(key.to_string()).or_default();
                let now = Instant::now();
                entries.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
                if entries.len() >= rpm as usize {
                    return Err(format!(
                        "Rate limit exceeded: {}/{} rpm for key {}",
                        entries.len(),
                        rpm,
                        &key[..8.min(key.len())]
                    ));
                }
                entries.push(now);
            }
        }

        // Loop detection: track prompt frequency (exact + semantic)
        if !prompt_hash.is_empty() {
            // Generate embedding FIRST (before acquiring any locks) — the await
            // would make !Send RwLock guards cross an await point otherwise.
            let embedding = if self.semantic_enabled && !prompt_text.is_empty() {
                self.generate_embedding(prompt_text).await.unwrap_or_default()
            } else {
                Vec::new()
            };

            // Now acquire the lock — no .await while holding it
            let mut history = self.request_history.write().unwrap();
            let entries = history.entry(key.to_string()).or_default();
            let now = Instant::now();

            // Prune entries outside the window
            entries.retain(|e| now.duration_since(e.timestamp) < self.loop_window);

            // Count occurrences: exact hash match OR semantic similarity match
            let mut similar_count = 0usize;
            for e in entries.iter() {
                if e.hash == prompt_hash {
                    similar_count += 1;
                } else if self.semantic_enabled && !embedding.is_empty() && !e.embedding.is_empty() {
                    let sim = cosine_similarity(&embedding, &e.embedding);
                    if sim > self.semantic_threshold {
                        similar_count += 1;
                    }
                }
            }

            // Record this request
            entries.push(PromptEntry {
                hash: prompt_hash.to_string(),
                embedding,
                timestamp: now,
            });

            if similar_count + 1 >= self.loop_threshold {
                // Trip the circuit breaker
                let mut blocked = self.loop_blocked.write().unwrap();
                blocked.insert(key.to_string(), now + self.loop_block_duration);

                // Clear history for this key so it starts fresh after the block
                entries.clear();

                tracing::warn!(
                    "Loop detected: key {} sent {} similar requests in {}s — blocking for {}s",
                    &key[..8.min(key.len())],
                    similar_count + 1,
                    self.loop_window.as_secs(),
                    self.loop_block_duration.as_secs()
                );

                return Err(format!(
                    "Loop detected: key {} sent {} similar requests within {}s. \
                     Blocked for {}s. Check your agent's retry logic — it may be stuck.",
                    &key[..8.min(key.len())],
                    similar_count + 1,
                    self.loop_window.as_secs(),
                    self.loop_block_duration.as_secs()
                ));
            }
        }

        Ok(())
    }

    /// Generate an embedding for a prompt using Ollama's embedding endpoint.
    /// Uses the shared HTTP client for connection pooling.
    async fn generate_embedding(&self, prompt: &str) -> Option<Vec<f32>> {
        let url = format!("{}/api/embeddings", self.ollama_url.trim_end_matches('/'));
        let body = json!({
            "model": self.embedding_model,
            "prompt": prompt,
        });

        let resp = (&*crate::router::SHARED_CLIENT)
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            tracing::debug!("loop detection embedding failed: {}", resp.status());
            return None;
        }

        let result: Value = resp.json().await.ok()?;
        result
            .get("embedding")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect()
            })
    }

    /// Record a cost for an API key (call after the request completes).
    pub fn record_spend(&self, key: &str, cost_usd: f64) {
        let mut spend = self.spend.write().unwrap();
        let entry = spend.entry(key.to_string()).or_insert(0.0);
        *entry += cost_usd;
    }

    /// Record spend Stoke had to estimate: a metered provider streamed a
    /// response and never reported its token usage. It counts against the cap,
    /// and it is also tallied separately so `/v1/budget` can admit it is a guess.
    pub fn record_spend_estimated(&self, key: &str, cost_usd: f64) {
        self.record_spend(key, cost_usd);
        let mut est = self.estimated.write().unwrap();
        *est.entry(key.to_string()).or_insert(0.0) += cost_usd;
    }

    /// Of a key's cumulative spend, how much was estimated rather than reported.
    pub fn estimated_spend(&self, key: &str) -> f64 {
        self.estimated.read().unwrap().get(key).copied().unwrap_or(0.0)
    }

    /// Get current spend for a key.
    pub fn get_spend(&self, key: &str) -> f64 {
        self.spend.read().unwrap().get(key).copied().unwrap_or(0.0)
    }

    /// Get stats for all keys.
    pub fn stats(&self) -> Vec<(String, f64, f64, u32, f64)> {
        let spend = self.spend.read().unwrap();
        let estimated = self.estimated.read().unwrap();
        let limits = self.limits.read().unwrap();
        let rate_limits = self.rate_limits.read().unwrap();
        let times = self.request_times.read().unwrap();

        let keys: std::collections::HashSet<String> = spend
            .keys()
            .chain(limits.keys())
            .chain(rate_limits.keys())
            .cloned()
            .collect();

        keys.iter()
            .map(|k| {
                let s = spend.get(k).copied().unwrap_or(0.0);
                let l = limits.get(k).copied().unwrap_or(0.0);
                let r = rate_limits.get(k).copied().unwrap_or(0);
                let recent = times.get(k).map(|v| v.len() as u32).unwrap_or(0);
                let e = estimated.get(k).copied().unwrap_or(0.0);
                (k.clone(), s, l, recent, e)
            })
            .collect()
    }

    /// Check if a key is currently loop-blocked.
    pub fn is_loop_blocked(&self, key: &str) -> bool {
        let blocked = self.loop_blocked.read().unwrap();
        blocked.get(key).map(|until| Instant::now() < *until).unwrap_or(false)
    }

    /// Get loop detection config for introspection.
    pub fn loop_config(&self) -> (usize, u64, u64) {
        (self.loop_threshold, self.loop_window.as_secs(), self.loop_block_duration.as_secs())
    }
}

/// Cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Aggregated request receipts (see `record_receipt`).
#[derive(Debug, Default)]
struct ReceiptTotals {
    requests: u64,
    zero_marginal: u64,
    avoided_usd_est: f64,
}

/// API key validation. If no keys are configured, all requests are allowed (dev mode).
/// If keys are configured, requests must send an Authorization Bearer header.
pub struct Auth {
    /// Valid API keys. Empty = no auth required.
    keys: RwLock<Vec<String>>,
}

impl Auth {
    pub fn new() -> Self {
        let keys = std::env::var("STOKE_API_KEYS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();
        Self {
            keys: RwLock::new(keys),
        }
    }

    /// Validate an API key from the Authorization header.
    /// Returns the key if valid, None if invalid/missing.
    /// Auth is required by default. Set STOKE_DEV=1 to allow anonymous access.
    pub fn validate(&self, auth_header: Option<&str>) -> Option<String> {
        let keys = self.keys.read().unwrap();
        let dev_mode = std::env::var("STOKE_DEV").unwrap_or_default() == "1";

        if keys.is_empty() {
            if dev_mode {
                return Some("anonymous".to_string());
            }
            tracing::warn!(
                "No STOKE_API_KEYS set and STOKE_DEV!=1. \
                 Set STOKE_API_KEYS=key1,key2 or STOKE_DEV=1 for local dev."
            );
            return None;
        }

        let token = auth_header
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|s| s.trim().to_string());

        match token {
            Some(t) if keys.contains(&t) => Some(t),
            _ => None,
        }
    }

    pub fn is_auth_enabled(&self) -> bool {
        !self.keys.read().unwrap().is_empty()
            || std::env::var("STOKE_DEV").unwrap_or_default() != "1"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `STOKE_DEV` / `STOKE_API_KEYS` are process-global, so the tests that
    /// mutate them must not run concurrently with each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[tokio::test]
    async fn test_budget_tracking() {
        let guard = BudgetGuard::new();
        guard.set_budget("test-key", 1.0);

        assert!(guard.check("test-key").await.is_ok());
        guard.record_spend("test-key", 0.5);
        assert!(guard.check("test-key").await.is_ok());
        guard.record_spend("test-key", 0.6);
        assert!(guard.check("test-key").await.is_err()); // 1.1 > 1.0
    }

    #[tokio::test]
    async fn test_no_limit_allows_all() {
        let guard = BudgetGuard::new();
        // No limit set -> always allowed
        assert!(guard.check("any-key").await.is_ok());
        guard.record_spend("any-key", 100.0);
        assert!(guard.check("any-key").await.is_ok());
    }

    #[tokio::test]
    async fn test_rate_limit() {
        let guard = BudgetGuard::new();
        guard.set_rate_limit("test-key", 2);

        assert!(guard.check("test-key").await.is_ok());
        assert!(guard.check("test-key").await.is_ok());
        assert!(guard.check("test-key").await.is_err()); // 3rd request blocked
    }

    #[test]
    fn test_auth_dev_mode() {
        // STOKE_DEV=1 + no keys → anonymous access allowed
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("STOKE_API_KEYS");
        std::env::set_var("STOKE_DEV", "1");
        let auth = Auth::new();
        assert_eq!(auth.validate(None), Some("anonymous".to_string()));
        assert_eq!(
            auth.validate(Some("Bearer anything")),
            Some("anonymous".to_string())
        );
        std::env::remove_var("STOKE_DEV");
    }

    #[test]
    fn test_key_policy_caps_enforce() {
        let g = BudgetGuard::new();
        g.set_budget("k", 0.01);
        g.record_spend("k", 0.02); // over the cap
        // check() must now refuse this key
        let refused = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(g.check("k"))
            .is_err();
        assert!(refused, "spend over cap must be refused");
    }

    #[test]
    fn test_auth_rejects_by_default() {
        // No STOKE_DEV, no STOKE_API_KEYS → reject all
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("STOKE_DEV");
        std::env::remove_var("STOKE_API_KEYS");
        let auth = Auth::new();
        assert_eq!(auth.validate(None), None);
        assert_eq!(auth.validate(Some("Bearer anything")), None);
    }

    #[tokio::test]
    async fn test_loop_detection() {
        let guard = BudgetGuard::new();
        let key = "loop-test-key";
        let prompt = "hash123";

        // First 4 requests: OK (threshold is 5)
        for i in 0..4 {
            assert!(
                guard.check_with_prompt(key, prompt, "").await.is_ok(),
                "request {} should pass",
                i
            );
        }

        // 5th request: trips the circuit breaker
        let result = guard.check_with_prompt(key, prompt, "").await;
        assert!(result.is_err(), "5th request should be blocked");
        let err = result.unwrap_err();
        assert!(
            err.contains("Loop detected"),
            "error should mention loop: {}",
            err
        );

        // Subsequent requests: blocked
        assert!(guard.check_with_prompt(key, prompt, "").await.is_err());
        assert!(guard.check_with_prompt(key, "different-hash", "").await.is_err());
        assert!(guard.is_loop_blocked(key));
    }

    #[tokio::test]
    async fn test_loop_detection_different_prompts() {
        let guard = BudgetGuard::new();
        let key = "loop-diff-key";

        // Same key, different prompt hashes: should NOT trigger loop detection
        for i in 0..10 {
            let prompt = format!("hash-{}", i);
            assert!(
                guard.check_with_prompt(key, &prompt, "").await.is_ok(),
                "different prompts should not loop"
            );
        }
        assert!(!guard.is_loop_blocked(key));
    }

    #[tokio::test]
    async fn test_loop_detection_no_hash() {
        let guard = BudgetGuard::new();
        let key = "loop-nohash-key";

        // Empty prompt hash: loop detection not active, should never block
        for _ in 0..100 {
            assert!(guard.check_with_prompt(key, "", "").await.is_ok());
        }
        assert!(!guard.is_loop_blocked(key));
    }

    #[test]
    fn test_cosine_similarity() {
        // Identical vectors -> 1.0
        let a = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 0.001);

        // Orthogonal vectors -> 0.0
        let b = vec![1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&b, &c).abs() < 0.001);

        // Empty -> 0.0
        assert_eq!(cosine_similarity(&[], &[]), 0.0);

        // Different lengths -> 0.0
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }
}