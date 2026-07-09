use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-model pricing (per 1M tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_1m: f64,
    pub output_per_1m: f64,
}

/// Cost calculation from token usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
}

impl CostBreakdown {
    pub fn zero(model: &str) -> Self {
        Self {
            model: model.to_string(),
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cost_usd: 0.0,
        }
    }

    pub fn merge(&mut self, other: &CostBreakdown) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        self.cost_usd += other.cost_usd;
    }
}

/// Registry of model prices. Local models are free ($0).
pub struct Pricer {
    prices: HashMap<String, ModelPricing>,
}

impl Default for Pricer {
    fn default() -> Self {
        let mut prices = HashMap::new();

        // Local Ollama models — always free
        let local_models = [
            "qwen2.5-coder:3b", "qwen2.5-coder:7b", "qwen3:8b", "qwen3:8b-128k",
            "phi4-mini", "llama3.2:3b", "gpt-oss:20b", "qwen3.6:35b",
            "gemma4:12b", "gemma4:12b-mlx", "gemma4:e4b",
        ];
        for m in &local_models {
            prices.insert(
                m.to_string(),
                ModelPricing { input_per_1m: 0.0, output_per_1m: 0.0 },
            );
        }

        // Cloud models (published API pricing per 1M tokens, Jun 2026)
        prices.insert("deepseek-v4-pro:cloud".into(), ModelPricing { input_per_1m: 1.10, output_per_1m: 4.40 });
        prices.insert("deepseek-v4-flash:cloud".into(), ModelPricing { input_per_1m: 0.30, output_per_1m: 1.20 });
        prices.insert("kimi-k2.6:cloud".into(), ModelPricing { input_per_1m: 0.60, output_per_1m: 2.50 });
        prices.insert("gpt-5.2".into(), ModelPricing { input_per_1m: 2.50, output_per_1m: 10.00 });
        prices.insert("gpt-5.2-mini".into(), ModelPricing { input_per_1m: 0.30, output_per_1m: 1.20 });
        prices.insert("claude-sonnet-4".into(), ModelPricing { input_per_1m: 3.00, output_per_1m: 15.00 });
        prices.insert("claude-haiku-4".into(), ModelPricing { input_per_1m: 0.80, output_per_1m: 4.00 });
        prices.insert("gemini-3-pro".into(), ModelPricing { input_per_1m: 1.25, output_per_1m: 5.00 });
        prices.insert("gemini-3-flash".into(), ModelPricing { input_per_1m: 0.15, output_per_1m: 0.60 });
        prices.insert("glm-5.2:cloud".into(), ModelPricing { input_per_1m: 0.50, output_per_1m: 2.00 });
        prices.insert("minimax-m3:cloud".into(), ModelPricing { input_per_1m: 0.30, output_per_1m: 1.20 });
        prices.insert("qwen3.6-plus".into(), ModelPricing { input_per_1m: 0.40, output_per_1m: 1.60 });

        Self { prices }
    }
}

impl Pricer {
    /// Calculate cost from a usage object (OpenAI format: {prompt_tokens, completion_tokens}).
    pub fn calculate(&self, model: &str, usage: Option<&serde_json::Value>) -> CostBreakdown {
        let pricing = self.prices.get(model).or_else(|| {
            // Fallback: if model name contains "cloud", assume it has a cost
            if model.contains("cloud") {
                Some(&ModelPricing { input_per_1m: 0.50, output_per_1m: 2.00 })
            } else {
                // Default: free (local)
                Some(&ModelPricing { input_per_1m: 0.0, output_per_1m: 0.0 })
            }
        });

        let (prompt_tokens, completion_tokens) = if let Some(u) = usage {
            let pt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let ct = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            (pt, ct)
        } else {
            (0, 0)
        };

        let cost_usd = if let Some(p) = pricing {
            (prompt_tokens as f64 * p.input_per_1m / 1_000_000.0)
                + (completion_tokens as f64 * p.output_per_1m / 1_000_000.0)
        } else {
            0.0
        };

        CostBreakdown {
            model: model.to_string(),
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cost_usd,
        }
    }

    /// Get the pricing for a model (for display/comparison).
    pub fn get_pricing(&self, model: &str) -> Option<&ModelPricing> {
        self.prices.get(model)
    }

    /// List all known model prices.
    pub fn all_prices(&self) -> &HashMap<String, ModelPricing> {
        &self.prices
    }
}