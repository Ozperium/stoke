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
    pub prompt_hash: String,    // SHA256 of model+prompt+temp+max_tokens
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

    /// Compute cache key from request parameters.
    /// Only caches deterministic requests (temperature == 0).
    pub fn cache_key(
        model: &str,
        messages: &[Value],
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Option<String> {
        // Don't cache non-deterministic requests
        if temperature.unwrap_or(0.0) > 0.01 {
            return None;
        }

        // Extract prompt from messages
        let prompt: String = messages
            .iter()
            .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
            .collect::<Vec<_>>()
            .join("\n");

        if prompt.is_empty() {
            return None;
        }

        let mut hasher = Sha256::new();
        hasher.update(model.as_bytes());
        hasher.update(prompt.as_bytes());
        hasher.update(max_tokens.unwrap_or(8192).to_be_bytes());
        let hash = hasher.finalize();
        Some(hex::encode(hash))
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
        let exact = self.exact.read().unwrap();
        if let Some(entry) = exact.get(key) {
            // Check TTL
            if entry.created_at.elapsed() < self.ttl {
                tracing::debug!("cache hit (exact): key={}", &key[..8]);
                return Some(entry.response.clone());
            }
        }
        None
    }

    /// Look up semantic match by embedding similarity.
    /// Returns the best match above threshold, or None.
    pub fn get_semantic(&self, query_embedding: &[f32]) -> Option<(String, Value)> {
        if !self.semantic_enabled || query_embedding.is_empty() {
            return None;
        }

        let exact = self.exact.read().unwrap();
        let mut best: Option<(f32, &CacheEntry)> = None;

        for entry in exact.values() {
            // Check TTL
            if entry.created_at.elapsed() >= self.ttl {
                continue;
            }
            // Skip if embedding is empty (wasn't embedded)
            if entry.embedding.is_empty() {
                continue;
            }
            let sim = cosine_similarity(query_embedding, &entry.embedding);
            if sim > self.similarity_threshold {
                if best.is_none() || sim > best.unwrap().0 {
                    best = Some((sim, entry));
                }
            }
        }

        if let Some((sim, entry)) = best {
            tracing::debug!("cache hit (semantic): sim={:.3}", sim);
            Some((entry.prompt_hash.clone(), entry.response.clone()))
        } else {
            None
        }
    }

    /// Store a response in the cache.
    /// If semantic caching is enabled, generates an embedding for the prompt.
    pub async fn put_with_embedding(&self, key: &str, response: Value, prompt: &str) {
        let embedding = if self.semantic_enabled && !prompt.is_empty() {
            self.generate_embedding(prompt).await.unwrap_or_default()
        } else {
            Vec::new()
        };

        let entry = CacheEntry {
            response,
            embedding,
            prompt_hash: key.to_string(),
            created_at: Instant::now(),
            hit_count: 0,
        };
        self.exact.write().unwrap().insert(key.to_string(), entry);
    }

    /// Store a response in the cache (legacy, no embedding).
    pub fn put(&self, key: &str, response: Value, embedding: Vec<f32>) {
        let entry = CacheEntry {
            response,
            embedding,
            prompt_hash: key.to_string(),
            created_at: Instant::now(),
            hit_count: 0,
        };
        self.exact.write().unwrap().insert(key.to_string(), entry);
    }

    /// Try exact match first, then semantic match.
    /// If semantic is enabled and no exact match, generates an embedding for the query
    /// and searches for similar cached prompts.
    pub async fn get_smart(&self, key: &str, prompt: &str) -> Option<(String, Value)> {
        // Try exact match first
        if let Some(resp) = self.get_exact(key) {
            return Some((key.to_string(), resp));
        }

        // Try semantic match
        if self.semantic_enabled && !prompt.is_empty() {
            let query_embedding = self.generate_embedding(prompt).await?;
            if let Some((hash, resp)) = self.get_semantic(&query_embedding) {
                tracing::info!("semantic cache hit for key={}", &key[..8]);
                return Some((hash, resp));
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