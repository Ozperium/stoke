use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Sha256, Digest};

/// A cached response entry.
pub struct CacheEntry {
    pub response: Value,        // Full OpenAI-compatible response JSON
    pub embedding: Vec<f32>,    // Embedding of the prompt for similarity search
    pub prompt_hash: String,    // Exact request identity hash
    /// Hash of the request with textual message content removed. Semantic
    /// matches are allowed only when this non-content identity also matches.
    pub semantic_identity: Option<String>,
    /// Who this entry belongs to: the API key and route path that produced it.
    /// Entries are only ever served back to the same scope. Without this, one
    /// key's answer is handed to another key that guessed the same prompt — and
    /// under semantic matching, to one that merely guessed a *similar* prompt.
    pub scope: String,
    pub created_at: Instant,
    pub hit_count: u32,
}

/// In-process response cache.
/// Two-layer: exact match (hash) + semantic match (embedding cosine similarity).
/// Semantic cache uses Ollama's embedding endpoint to generate embeddings.
pub struct ResponseCache {
    /// Exact match: hash → entry
    exact: RwLock<HashMap<String, CacheEntry>>,
    /// TTL for cache entries
    ttl: Duration,
    /// Similarity threshold for semantic cache (0.0-1.0)
    similarity_threshold: f32,
    /// Whether semantic caching is enabled (requires embeddings)
    semantic_enabled: bool,
    /// Ollama base URL for embedding generation
    ollama_url: String,
    /// Embedding model name
    embedding_model: String,
}

impl ResponseCache {
    pub fn new(ttl_secs: u64, similarity_threshold: f32, semantic_enabled: bool) -> Self {
        // Semantic caching needs an explicit embedding model — Stoke ships
        // no model names. Requested without one → exact-match only + warning.
        let embedding_model = std::env::var("STOKE_EMBED_MODEL").unwrap_or_default();
        if semantic_enabled && embedding_model.is_empty() {
            tracing::warn!(
                "semantic cache disabled: set STOKE_EMBED_MODEL to an embedding model \
                 available on your Ollama (exact-match caching stays on)."
            );
        }
        Self {
            exact: RwLock::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
            similarity_threshold,
            semantic_enabled: semantic_enabled && !embedding_model.is_empty(),
            ollama_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string()),
            embedding_model,
        }
    }

    /// The scope an entry belongs to. Two callers, or two route profiles with
    /// different policy, must never share a cache slot.
    pub fn scope_of(api_key: &str, path: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(api_key.as_bytes());
        hasher.update([0u8]); // domain separator: key "a"+path "b" != key "ab"
        hasher.update(path.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Route retention may shorten, but never extend, global retention.
    pub fn effective_ttl(&self, route_ttl_secs: u64) -> Duration {
        Duration::from_secs(route_ttl_secs.min(self.ttl.as_secs()))
    }

    /// Compute cache key from request parameters.
    /// Only caches deterministic requests (temperature == 0).
    pub fn cache_key(scope: &str, model: &str, request: &Value) -> Option<String> {
        // Don't cache non-deterministic requests
        if request
            .get("temperature")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            > 0.01
        {
            return None;
        }

        let Some(messages) = request.get("messages").and_then(Value::as_array) else {
            return None;
        };
        if messages.is_empty() || Self::extract_prompt(messages).is_empty() {
            return None;
        }

        // Hash the complete effective serialized request. This retains role and
        // message boundaries plus every forwarded output-affecting field.
        let identity = json!({
            "scope": scope,
            "model": model,
            "request": request,
        });
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(&identity).ok()?);
        let hash = hasher.finalize();
        Some(hex::encode(hash))
    }

    /// Identity used to constrain opt-in semantic matching. Unsupported
    /// message shapes deliberately return None rather than weakening exact
    /// request identity with a lossy embedding prompt.
    pub fn semantic_identity(scope: &str, model: &str, request: &Value) -> Option<String> {
        let messages = request.get("messages")?.as_array()?;
        let mut structure = Vec::with_capacity(messages.len());
        for message in messages {
            let mut object = message.as_object()?.clone();
            if !object.get("content")?.is_string() {
                return None;
            }
            object.remove("content");
            structure.push(Value::Object(object));
        }

        let mut request_without_content = request.as_object()?.clone();
        request_without_content.insert("messages".into(), Value::Array(structure));
        let identity = json!({
            "scope": scope,
            "model": model,
            "request": request_without_content,
        });
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(&identity).ok()?);
        Some(hex::encode(hasher.finalize()))
    }

    /// Extract prompt text from messages for embedding.
    pub fn extract_prompt(messages: &[Value]) -> String {
        messages
            .iter()
            .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Look up exact match by hash.
    pub fn get_exact(&self, key: &str) -> Option<Value> {
        self.get_exact_with_ttl(key, self.ttl)
    }

    pub fn get_exact_with_ttl(&self, key: &str, ttl: Duration) -> Option<Value> {
        self.get_exact_at(key, ttl, Instant::now())
    }

    fn get_exact_at(&self, key: &str, ttl: Duration, now: Instant) -> Option<Value> {
        let mut exact = self.exact.write().unwrap();
        if let Some(entry) = exact.get(key) {
            if now.duration_since(entry.created_at) < ttl {
                tracing::debug!("cache hit (exact): key={:.8}", key);
                return Some(entry.response.clone());
            }
            exact.remove(key);
        }
        None
    }

    /// Store a route-policy exact entry without deriving prompt text or an
    /// embedding. The request identity has already been computed by the caller.
    pub fn put_exact(&self, key: &str, scope: &str, response: Value) {
        self.put_exact_at(key, scope, response, Instant::now());
    }

    fn put_exact_at(&self, key: &str, scope: &str, response: Value, created_at: Instant) {
        self.evict();
        let entry = CacheEntry {
            response,
            embedding: Vec::new(),
            prompt_hash: key.to_string(),
            semantic_identity: None,
            scope: scope.to_string(),
            created_at,
            hit_count: 0,
        };
        self.exact.write().unwrap().insert(key.to_string(), entry);
    }

    /// Look up semantic match by embedding similarity.
    /// Returns the best match above threshold, or None.
    pub fn get_semantic(&self, scope: &str, query_embedding: &[f32]) -> Option<(String, Value)> {
        self.get_semantic_filtered(scope, None, query_embedding)
    }

    fn get_semantic_filtered(
        &self,
        scope: &str,
        semantic_identity: Option<&str>,
        query_embedding: &[f32],
    ) -> Option<(String, Value)> {
        if !self.semantic_enabled || query_embedding.is_empty() {
            return None;
        }

        let exact = self.exact.read().unwrap();
        let mut best: Option<(f32, &CacheEntry)> = None;

        for entry in exact.values() {
            if entry.scope != scope {
                continue;
            }
            if let Some(identity) = semantic_identity {
                if entry.semantic_identity.as_deref() != Some(identity) {
                    continue;
                }
            }
            if entry.created_at.elapsed() >= self.ttl || entry.embedding.is_empty() {
                continue;
            }
            let sim = cosine_similarity(query_embedding, &entry.embedding);
            if sim > self.similarity_threshold
                && (best.is_none() || sim > best.as_ref().unwrap().0)
            {
                best = Some((sim, entry));
            }
        }

        best.map(|(sim, entry)| {
            tracing::debug!("cache hit (semantic): sim={:.3}", sim);
            (entry.prompt_hash.clone(), entry.response.clone())
        })
    }

    /// Store a response in the cache.
    /// If semantic caching is enabled, generates an embedding for the prompt.
    pub async fn put_with_embedding(
        &self,
        key: &str,
        scope: &str,
        response: Value,
        prompt: &str,
        semantic_identity: Option<&str>,
    ) {
        let embedding = if self.semantic_enabled
            && semantic_identity.is_some()
            && !prompt.is_empty()
        {
            self.generate_embedding(prompt).await.unwrap_or_default()
        } else {
            Vec::new()
        };

        let entry = CacheEntry {
            response,
            embedding,
            prompt_hash: key.to_string(),
            semantic_identity: semantic_identity.map(str::to_string),
            scope: scope.to_string(),
            created_at: Instant::now(),
            hit_count: 0,
        };
        self.evict();
        self.exact.write().unwrap().insert(key.to_string(), entry);
    }

    /// Store a response in the cache (legacy, no embedding).
    pub fn put(&self, key: &str, scope: &str, response: Value, embedding: Vec<f32>) {
        self.evict();
        let entry = CacheEntry {
            response,
            embedding,
            prompt_hash: key.to_string(),
            semantic_identity: None,
            scope: scope.to_string(),
            created_at: Instant::now(),
            hit_count: 0,
        };
        self.exact.write().unwrap().insert(key.to_string(), entry);
    }

    /// Try exact match first, then semantic match.
    /// If semantic is enabled and no exact match, generates an embedding for the query
    /// and searches for similar cached prompts.
    pub async fn get_smart(
        &self,
        key: &str,
        scope: &str,
        prompt: &str,
        semantic_identity: Option<&str>,
    ) -> Option<(String, Value)> {
        // Try exact match first
        if let Some(resp) = self.get_exact(key) {
            return Some((key.to_string(), resp));
        }

        // Try semantic match only within the same non-content request identity.
        if self.semantic_enabled && semantic_identity.is_some() && !prompt.is_empty() {
            let query_embedding = self.generate_embedding(prompt).await?;
            if let Some(identity) = semantic_identity {
                if let Some((hash, resp)) =
                    self.get_semantic_filtered(scope, Some(identity), &query_embedding)
                {
                    tracing::info!("semantic cache hit for key={}", &key[..8]);
                    return Some((hash, resp));
                }
            }
        }

        None
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
            tracing::debug!("embedding request failed: {}", resp.status());
            return None;
        }

        let result: Value = resp.json().await.ok()?;
        result.get("embedding")
            .and_then(|e| e.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect())
    }

    /// Get cache stats.
    pub fn stats(&self) -> CacheStats {
        let exact = self.exact.read().unwrap();
        let total_entries = exact.len();
        let total_hits: u32 = exact.values().map(|e| e.hit_count).sum();
        CacheStats {
            entries: total_entries,
            hits: total_hits,
        }
    }

    /// Evict expired entries.
    pub fn evict(&self) {
        let mut exact = self.exact.write().unwrap();
        exact.retain(|_, entry| entry.created_at.elapsed() < self.ttl);
    }
}

#[derive(Serialize)]
pub struct CacheStats {
    pub entries: usize,
    pub hits: u32,
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
#[cfg(test)]
mod scope_tests {
    use super::*;

    fn msgs() -> Vec<Value> {
        vec![json!({"role": "user", "content": "the same question"})]
    }

    fn request(model: &str, messages: Vec<Value>, temperature: Option<f32>, max_tokens: Option<u32>) -> Value {
        json!({
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": false,
        })
    }

    fn with_field(mut request: Value, name: &str, value: Value) -> Value {
        request.as_object_mut().unwrap().insert(name.to_string(), value);
        request
    }

    #[test]
    fn different_api_keys_get_different_cache_keys() {
        // Regression: the key used to hash only (model, prompt, max_tokens), so a
        // second caller who guessed the prompt was served the first caller's
        // response — for free, and without the response hooks running.
        let a = ResponseCache::scope_of("key-a", "/v1/chat/completions");
        let b = ResponseCache::scope_of("key-b", "/v1/chat/completions");
        assert_ne!(a, b);
        let ka = ResponseCache::cache_key(&a, "m", &request("m", msgs(), Some(0.0), None));
        let kb = ResponseCache::cache_key(&b, "m", &request("m", msgs(), Some(0.0), None));
        assert!(ka.is_some() && kb.is_some());
        assert_ne!(ka, kb, "same prompt under two keys must not share a cache slot");
    }

    #[test]
    fn different_routes_get_different_cache_keys() {
        // Two route profiles can carry different policy for the same model.
        let a = ResponseCache::scope_of("key-a", "/v1/chat/completions");
        let b = ResponseCache::scope_of("key-a", "/v1/code/completions");
        assert_ne!(a, b);
    }

    #[test]
    fn the_same_caller_on_the_same_route_still_hits() {
        let s = ResponseCache::scope_of("key-a", "/v1/chat/completions");
        let k1 = ResponseCache::cache_key(&s, "m", &request("m", msgs(), Some(0.0), None));
        let k2 = ResponseCache::cache_key(&s, "m", &request("m", msgs(), Some(0.0), None));
        assert_eq!(k1, k2, "scoping must not disable caching for its own scope");
    }

    #[test]
    fn cache_key_preserves_role_and_message_boundaries() {
        let s = ResponseCache::scope_of("k", "/v1/chat/completions");
        let a = request("fixture-model", vec![
            json!({"role":"system","content":"alpha"}),
            json!({"role":"user","content":"beta"}),
        ], Some(0.0), Some(64));
        let b = request("fixture-model", vec![json!({"role":"user","content": "alpha".to_string() + "\n" + "beta"})], Some(0.0), Some(64));

        assert_ne!(
            ResponseCache::cache_key(&s, "fixture-model", &a),
            ResponseCache::cache_key(&s, "fixture-model", &b),
            "different roles and message boundaries must not share an exact cache slot"
        );
    }

    #[test]
    fn cache_key_preserves_output_affecting_message_fields_and_temperature() {
        let s = ResponseCache::scope_of("k", "/v1/chat/completions");
        let named = request("fixture-model", vec![json!({
            "role":"user",
            "content":"same",
            "name":"first"
        })], Some(0.0), Some(64));
        let renamed = request("fixture-model", vec![json!({
            "role":"user",
            "content":"same",
            "name":"second"
        })], Some(0.0), Some(64));

        assert_ne!(
            ResponseCache::cache_key(&s, "fixture-model", &named),
            ResponseCache::cache_key(&s, "fixture-model", &renamed),
            "message metadata forwarded to the provider must affect identity"
        );
        assert_ne!(
            ResponseCache::cache_key(&s, "fixture-model", &named),
            ResponseCache::cache_key(&s, "fixture-model", &request("fixture-model", vec![json!({"role":"user","content":"same","name":"first"})], Some(0.01), Some(64))),
            "distinct cached temperatures must not share an exact cache slot"
        );
    }

    #[test]
    fn cache_key_preserves_tool_response_reasoning_seed_and_max_tokens() {
        let s = ResponseCache::scope_of("k", "/v1/chat/completions");
        let base = request("fixture-model", msgs(), Some(0.0), Some(64));
        let cases = [
            (
                "tools",
                json!([{"type":"function","function":{"name":"lookup"}}]),
            ),
            ("tool_choice", json!("auto")),
            ("response_format", json!({"type":"json_object"})),
            ("reasoning", json!({"effort":"high"})),
            ("seed", json!(7)),
            ("max_tokens", json!(128)),
        ];

        for (field, value) in cases {
            assert_ne!(
                ResponseCache::cache_key(&s, "fixture-model", &base),
                ResponseCache::cache_key(
                    &s,
                    "fixture-model",
                    &with_field(base.clone(), field, value)
                ),
                "{field} must affect exact request identity"
            );
        }
    }

    #[tokio::test]
    async fn identical_request_from_same_caller_hits_exact_cache() {
        let scope = ResponseCache::scope_of("key-a", "/v1/chat/completions");
        let request = request("fixture-model", msgs(), Some(0.0), Some(64));
        let key = ResponseCache::cache_key(&scope, "fixture-model", &request).unwrap();
        let cache = ResponseCache::new(3600, 0.92, false);
        let response = json!({"answer":"cached"});
        cache
            .put_with_embedding(&key, &scope, response.clone(), "the same question", None)
            .await;

        assert_eq!(
            cache
                .get_smart(&key, &scope, "the same question", None)
                .await,
            Some((key, response))
        );
    }

    #[test]
    fn semantic_identity_is_deterministic_and_ignores_only_text() {
        let scope = ResponseCache::scope_of("k", "/v1/chat/completions");
        let first = request(
            "fixture-model",
            vec![json!({"role":"user","content":"alpha"})],
            Some(0.0),
            Some(64),
        );
        let second = request(
            "fixture-model",
            vec![json!({"role":"user","content":"beta"})],
            Some(0.0),
            Some(64),
        );
        assert_eq!(
            ResponseCache::semantic_identity(&scope, "fixture-model", &first),
            ResponseCache::semantic_identity(&scope, "fixture-model", &second),
            "same metadata with different text stays eligible for semantic matching"
        );
        assert!(ResponseCache::cache_key(&scope, "fixture-model", &first).is_some());
        assert!(ResponseCache::cache_key(&scope, "fixture-model", &second).is_some());
    }

    #[test]
    fn semantic_matching_rejects_same_vectors_for_role_model_or_setting_changes() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("STOKE_EMBED_MODEL", "fixture-embed");
        let cache = ResponseCache::new(3600, 0.5, true);
        std::env::remove_var("STOKE_EMBED_MODEL");
        let scope = ResponseCache::scope_of("k", "/v1/chat/completions");
        let base = request(
            "fixture-model",
            vec![json!({"role":"user","content":"alpha"})],
            Some(0.0),
            Some(64),
        );
        let base_id = ResponseCache::semantic_identity(&scope, "fixture-model", &base).unwrap();
        cache.exact.write().unwrap().insert(
            "base".to_string(),
            CacheEntry {
                response: json!({"answer":"base"}),
                embedding: vec![1.0, 0.0],
                prompt_hash: "base".to_string(),
                semantic_identity: Some(base_id.clone()),
                scope: scope.clone(),
                created_at: Instant::now(),
                hit_count: 0,
            },
        );

        assert!(cache
            .get_semantic_filtered(&scope, Some(&base_id), &[1.0, 0.0])
            .is_some());
        for (model, messages, temperature) in [
            (
                "fixture-model",
                vec![json!({"role":"system","content":"alpha"})],
                0.0,
            ),
            (
                "other-model",
                vec![json!({"role":"user","content":"alpha"})],
                0.0,
            ),
            (
                "fixture-model",
                vec![json!({"role":"user","content":"alpha"})],
                0.2,
            ),
        ] {
            let changed = request(model, messages, Some(temperature), Some(64));
            let changed_id = ResponseCache::semantic_identity(&scope, model, &changed).unwrap();
            assert!(cache
                .get_semantic_filtered(&scope, Some(&changed_id), &[1.0, 0.0])
                .is_none());
        }
    }

    #[test]
    fn unsupported_message_shapes_skip_both_cache_layers() {
        let scope = ResponseCache::scope_of("k", "/v1/chat/completions");
        let multipart = json!({
            "messages": [{"role":"user","content":[{"type":"text","text":"hi"}]}],
            "temperature": 0.0
        });
        let non_object = json!({"messages":["not a message"],"temperature":0.0});
        for request in [multipart, non_object] {
            assert!(ResponseCache::semantic_identity(&scope, "fixture-model", &request).is_none());
            assert!(ResponseCache::cache_key(&scope, "fixture-model", &request).is_none());
        }
    }

    #[test]
    fn cache_key_fields_are_domain_separated() {
        // model "ab" + prompt "c" must not hash the same as "a" + "bc".
        let s = ResponseCache::scope_of("k", "/p");
        let a = ResponseCache::cache_key(&s, "ab", &request("ab", vec![json!({"role":"user","content":"c"})], Some(0.0), None));
        let b = ResponseCache::cache_key(&s, "a", &request("a", vec![json!({"role":"user","content":"bc"})], Some(0.0), None));
        assert_ne!(a, b);
    }

    #[test]
    fn scope_is_domain_separated() {
        // Without a separator, key "ab" + path "c" and key "a" + path "bc" collide.
        assert_ne!(ResponseCache::scope_of("ab", "c"), ResponseCache::scope_of("a", "bc"));
    }

    /// `ResponseCache::new` silently disables the semantic layer unless an
    /// embedding model is named, so a test that skips this proves nothing: both
    /// lookups return None and the scope filter is never reached.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn semantic_lookup_will_not_cross_scopes() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("STOKE_EMBED_MODEL", "fixture-embed");
        let cache = ResponseCache::new(3600, 0.5, true);
        std::env::remove_var("STOKE_EMBED_MODEL");

        let mine = ResponseCache::scope_of("key-a", "/v1/chat/completions");
        let theirs = ResponseCache::scope_of("key-b", "/v1/chat/completions");
        cache.put("k", &theirs, json!({"answer": "secret"}), vec![1.0, 0.0]);

        // Same embedding, different scope: the semantic scan reaches entries by
        // similarity alone, so without the scope filter this would hand key-a
        // key-b's answer for a merely *similar* prompt.
        assert!(cache.get_semantic(&mine, &[1.0, 0.0]).is_none(), "leaked across scopes");
        assert!(
            cache.get_semantic(&theirs, &[1.0, 0.0]).is_some(),
            "own-scope semantic hit must still work — otherwise this test proves nothing"
        );
    }

    #[test]
    fn exact_cache_debug_logging_accepts_short_and_unicode_keys() {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(std::io::sink)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let cache = ResponseCache::new(3600, 0.92, false);
            for key in ["key", "€€€"] {
                let response = json!({"answer": "synthetic"});
                cache.put_exact(key, "scope", response.clone());
                assert_eq!(cache.get_exact(key), Some(response));
            }
        });
    }

    #[test]
    fn route_exact_ttl_expires_against_injected_instant_without_sleeping() {
        let cache = ResponseCache::new(3600, 0.92, false);
        let scope = ResponseCache::scope_of("key", "/v1/exact");
        let created_at = Instant::now() - Duration::from_secs(11);
        cache.exact.write().unwrap().insert(
            "key".to_string(),
            CacheEntry {
                response: json!({"answer": "old"}),
                embedding: Vec::new(),
                prompt_hash: "key".to_string(),
                semantic_identity: None,
                scope,
                created_at,
                hit_count: 0,
            },
        );
        assert!(cache.get_exact_at("key", Duration::from_secs(10), Instant::now()).is_none());
        assert!(
            !cache.exact.read().unwrap().contains_key("key"),
            "an expired exact entry must be removed on lookup"
        );
        cache.exact.write().unwrap().insert(
            "stale".to_string(),
            CacheEntry {
                response: json!({"answer": "stale"}),
                embedding: Vec::new(),
                prompt_hash: "stale".to_string(),
                semantic_identity: None,
                scope: "scope".to_string(),
                created_at: Instant::now() - Duration::from_secs(3601),
                hit_count: 0,
            },
        );
        cache.put("fresh", "scope", json!({"answer": "fresh"}), Vec::new());
        assert_eq!(cache.stats().entries, 1, "writes must prune global-TTL entries");
        cache.exact.write().unwrap().insert(
            "key".to_string(),
            CacheEntry {
                response: json!({"answer": "recent-enough"}),
                embedding: Vec::new(),
                prompt_hash: "key".to_string(),
                semantic_identity: None,
                scope: "scope".to_string(),
                created_at: Instant::now() - Duration::from_secs(11),
                hit_count: 0,
            },
        );
        assert!(cache.get_exact_at("key", Duration::from_secs(12), Instant::now()).is_some());
        assert_eq!(cache.effective_ttl(10), Duration::from_secs(10));
        assert_eq!(cache.effective_ttl(7200), Duration::from_secs(3600));
    }
}
