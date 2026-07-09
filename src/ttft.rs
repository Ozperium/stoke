use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

/// Tracks time-to-first-token (TTFT) and error rates per provider.
/// Used by TTFT-aware routing to pick the fastest available provider.
pub struct TtftTracker {
    /// Provider name → rolling average TTFT in milliseconds
    ttft: RwLock<HashMap<String, f64>>,
    /// Provider name → error count (consecutive failures)
    errors: RwLock<HashMap<String, u32>>,
    /// Window size for rolling average
    window: usize,
}

impl TtftTracker {
    pub fn new() -> Self {
        Self {
            ttft: RwLock::new(HashMap::new()),
            errors: RwLock::new(HashMap::new()),
            window: 10,
        }
    }

    /// Record a TTFT measurement for a provider.
    pub fn record_ttft(&self, provider: &str, ms: f64) {
        let mut ttft = self.ttft.write().unwrap();
        let entry = ttft.entry(provider.to_string()).or_insert(ms);
        // Rolling average: weight new value at 1/window
        *entry = *entry * (1.0 - 1.0 / self.window as f64) + ms / self.window as f64;

        // Reset error count on success
        self.errors.write().unwrap().insert(provider.to_string(), 0);
    }

    /// Record a provider failure.
    pub fn record_error(&self, provider: &str) {
        let mut errors = self.errors.write().unwrap();
        let count = errors.entry(provider.to_string()).or_insert(0);
        *count += 1;
    }

    /// Get the best (lowest TTFT) provider from a list, considering error rates.
    /// Providers with >3 consecutive errors are skipped.
    pub fn best_provider(&self, providers: &[String]) -> Option<String> {
        let ttft = self.ttft.read().unwrap();
        let errors = self.errors.read().unwrap();

        let mut best: Option<(String, f64)> = None;
        for p in providers {
            // Skip providers with too many consecutive errors
            let err_count = errors.get(p).copied().unwrap_or(0);
            if err_count > 3 {
                continue;
            }

            let score = ttft.get(p).copied().unwrap_or(f64::MAX);
            let best_score = best.as_ref().map(|(_, s)| *s).unwrap_or(f64::MAX);
            if best.is_none() || score < best_score {
                best = Some((p.clone(), score));
            }
        }

        best.map(|(p, _)| p)
    }

    /// Get stats for all tracked providers.
    pub fn stats(&self) -> HashMap<String, (f64, u32)> {
        let ttft = self.ttft.read().unwrap();
        let errors = self.errors.read().unwrap();
        let mut result = HashMap::new();
        for (name, &ms) in ttft.iter() {
            result.insert(name.clone(), (ms, errors.get(name).copied().unwrap_or(0)));
        }
        result
    }
}