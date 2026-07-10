use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Command;
use std::time::Instant;
use once_cell::sync::Lazy;

use crate::config::ProviderConfig;
use crate::cost::CostBreakdown;

/// Run a Python test file in a restricted sandbox.
/// Blocks dangerous modules and enforces CPU/memory/time limits.
fn run_python_sandboxed(script_path: &std::path::Path) -> std::io::Result<std::process::Output> {
    let sandbox_code = format!(
        r#"
import importlib, sys, resource, signal

# Block dangerous modules
_BLOCKED = ["os", "subprocess", "ctypes", "socket", "http", "ftplib", "smtplib",
           "telnetlib", "socketserver", "multiprocessing", "threading", "signal"]
_orig_import = __builtins__["__import__"] if isinstance(__builtins__, dict) else __builtins__.__import__
def _safe_import(name, *args, **kwargs):
    top = name.split(".")[0]
    if top in _BLOCKED:
        raise ImportError(f"Module '{{top}}' is blocked by Stoke sandbox")
    return _orig_import(name, *args, **kwargs)
if isinstance(__builtins__, dict):
    __builtins__["__import__"] = _safe_import
else:
    __builtins__.__import__ = _safe_import

# Resource limits: 10s CPU, 512MB memory, 60s wall clock
resource.setrlimit(resource.RLIMIT_CPU, (10, 10))
resource.setrlimit(resource.RLIMIT_AS, (512 * 1024 * 1024, 512 * 1024 * 1024))
signal.alarm(60)

# Run the test file
exec(open("{}").read())
"#,
        script_path.display()
    );

    Command::new("python3")
        .arg("-c")
        .arg(&sandbox_code)
        .output()
}

/// Shared HTTP client with connection pooling — avoids creating a new
/// client (and TLS handshake) on every request.
pub static SHARED_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .pool_max_idle_per_host(16)
        .tcp_keepalive(Some(std::time::Duration::from_secs(60)))
        .gzip(true)
        .build()
        .expect("Failed to build shared HTTP client")
});

/// A chat completion request in OpenAI format.
/// We pass through everything we don't explicitly handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    /// Pass through any other fields
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Response from a provider in OpenAI format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Value>,
    #[serde(default)]
    pub usage: Option<Value>,
}

/// Result of a single provider call — includes timing and cost.
#[derive(Clone)]
pub struct ProviderResult {
    pub response: ChatCompletionResponse,
    pub elapsed_ms: u64,
    pub provider_name: String,
    pub cost: CostBreakdown,
}

/// Call a single provider with a chat completion request.
pub async fn call_provider(
    provider: &ProviderConfig,
    request: &ChatCompletionRequest,
) -> Result<ProviderResult, String> {
    call_provider_hop(provider, request, 0).await
}

/// Like `call_provider`, but propagates the federation hop count.
/// A downstream Stoke gateway reads `x-stoke-hop` and refuses to forward
/// again (depth-1 federation); other providers ignore the header.
pub async fn call_provider_hop(
    provider: &ProviderConfig,
    request: &ChatCompletionRequest,
    hop: u32,
) -> Result<ProviderResult, String> {
    let pricer = crate::cost::global();

    // The dispatch gate. Every routing pattern — single, failover, and each
    // fusion fan-out — funnels through here, so this is the one place that can
    // refuse unmeterable spend while refusing is still free.
    pricer.allows(&provider.tier, &request.model)?;

    let client = &*SHARED_CLIENT;
    let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));

    let body = serde_json::to_value(request)
        .map_err(|e| format!("Failed to serialize request: {}", e))?;

    let start = Instant::now();

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", provider.resolve_api_key()))
        .header("Content-Type", "application/json")
        .header("x-stoke-hop", hop.saturating_add(1).to_string())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Provider {} request failed: {}", provider.name, e))?;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Provider {} returned {}: {}", provider.name, status, text));
    }

    let response: ChatCompletionResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse provider {} response: {}", provider.name, e))?;

    let cost = pricer.calculate(&request.model, response.usage.as_ref());

    Ok(ProviderResult {
        response,
        elapsed_ms,
        provider_name: provider.name.clone(),
        cost,
    })
}

/// A fusion pattern issues many provider calls and returns exactly one of them.
/// The operator is billed for *all* of them, so the returned result must carry
/// the whole bill — otherwise `record_spend` books one call's cost for N calls'
/// worth of money and the budget cap under-counts by a factor of N.
///
/// `attempted` should include every call whose tokens were actually generated,
/// including the losers.
fn bill_all(winner: &mut ProviderResult, attempted: &[ProviderResult]) {
    let mut total = CostBreakdown::zero(&winner.cost.model);
    for r in attempted {
        total.merge(&r.cost);
    }
    winner.cost = total;
}

/// Fusion: Parallel+Vote — send the same request to N providers,
/// pick the most common response (majority vote on the content).
pub async fn parallel_vote(
    providers: Vec<&ProviderConfig>,
    request: &ChatCompletionRequest,
) -> Result<ProviderResult, String> {
    if providers.is_empty() {
        return Err("No providers for parallel_vote".to_string());
    }

    let mut set = tokio::task::JoinSet::new();
    for p in providers {
        let req = request.clone();
        let provider = p.clone();
        set.spawn(async move {
            call_provider(&provider, &req).await
        });
    }

    let mut results = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(r) = res {
            results.push(r);
        }
    }

    let successes: Vec<ProviderResult> = results.into_iter().filter_map(|r| r.ok()).collect();

    if successes.is_empty() {
        return Err("All providers failed in parallel_vote".to_string());
    }

    // Majority vote: extract the message content from each response, pick the most common.
    let mut content_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in &successes {
        if let Some(choice) = r.response.choices.first() {
            if let Some(content) = choice.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                *content_counts.entry(content.to_string()).or_insert(0) += 1;
            }
        }
    }

    // Find the content with the most votes
    let winner_content = content_counts
        .iter()
        .max_by_key(|(_, &count)| count)
        .map(|(content, _)| content.clone());

    // Return the first result that matches the winning content
    if let Some(winner) = winner_content {
        for r in &successes {
            if let Some(choice) = r.response.choices.first() {
                if let Some(content) = choice.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                    if content == winner {
                        let mut out = r.clone();
                        bill_all(&mut out, &successes);
                        return Ok(out);
                    }
                }
            }
        }
    }

    // Fallback: return the first success
    let mut out = successes[0].clone();
    bill_all(&mut out, &successes);
    Ok(out)
}

/// Fusion: Self-Consistency — same model, N samples at temp>0, majority vote.
/// Different from parallel_vote (which uses different models). Self-consistency
/// exploits the fact that correct reasoning paths are more likely to converge.
/// Good for: math, reasoning, multi-step problems where chain-of-thought varies.
pub async fn self_consistency(
    provider: &ProviderConfig,
    model: &str,
    request: &ChatCompletionRequest,
    n_samples: usize,
    temperature: f32,
) -> Result<ProviderResult, String> {
    if n_samples == 0 {
        return Err("n_samples must be > 0 for self_consistency".to_string());
    }

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..n_samples {
        let mut req = request.clone();
        req.model = model.to_string();
        req.temperature = Some(temperature);
        let provider = provider.clone();
        set.spawn(async move { call_provider(&provider, &req).await });
    }

    let mut results = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(Ok(r)) = res {
            results.push(r);
        }
    }

    if results.is_empty() {
        return Err("All samples failed in self_consistency".to_string());
    }

    // Majority vote on content
    let mut content_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in &results {
        if let Some(choice) = r.response.choices.first() {
            if let Some(content) = choice.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                *content_counts.entry(content.to_string()).or_insert(0) += 1;
            }
        }
    }

    let winner_content = content_counts
        .iter()
        .max_by_key(|(_, &count)| count)
        .map(|(content, _)| content.clone());

    if let Some(winner) = winner_content {
        for r in &results {
            if let Some(choice) = r.response.choices.first() {
                if let Some(content) = choice.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                    if content == winner {
                        tracing::info!("self_consistency: model {} majority vote ({} samples)", model, n_samples);
                        let mut out = r.clone();
                        bill_all(&mut out, &results);
                        return Ok(out);
                    }
                }
            }
        }
    }

    let mut out = results[0].clone();
    bill_all(&mut out, &results);
    Ok(out)
}



/// Fusion: Deliberation — panel generates, judge analyzes, synthesizer writes final.
/// 1. Panel: N models answer in parallel
/// 2. Judge: one model receives all responses, outputs structured analysis
///    (consensus, contradictions, gaps, unique insights)
/// 3. Synthesizer: primary model receives original prompt + judge's analysis,
///    writes the final answer
///
/// Unlike parallel_merge (judge merges into one answer), the judge here outputs
/// METADATA about agreement/disagreement, and the synthesizer uses that to decide.
pub async fn deliberation(
    provider: &ProviderConfig,
    panel_models: &[String],
    judge_model: &str,
    synth_model: &str,
    request: &ChatCompletionRequest,
) -> Result<ProviderResult, String> {
    if panel_models.is_empty() {
        return Err("No panel models for deliberation".to_string());
    }

    let original_prompt = request
        .messages
        .first()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    // Phase 1: Panel — fan out to all models in parallel
    let mut set = tokio::task::JoinSet::new();
    for model in panel_models {
        let mut req = request.clone();
        req.model = model.clone();
        let provider = provider.clone();
        set.spawn(async move { call_provider(&provider, &req).await });
    }

    // Panel and judge tokens are billed even though only the synthesizer's
    // response is returned. Carry their cost forward.
    let mut side_cost = CostBreakdown::zero(synth_model);

    let mut responses: Vec<(String, String)> = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(Ok(r)) = res {
            side_cost.merge(&r.cost);
            let content = r
                .response
                .choices
                .first()
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if !content.is_empty() {
                responses.push((r.response.model.clone(), content));
            }
        }
    }

    if responses.is_empty() {
        return Err("All panel models failed in deliberation".to_string());
    }

    // Phase 2: Judge — analyze panel responses, output structured analysis
    let mut judge_prompt = format!(
        "You are a judge analyzing responses from multiple AI models to the same question.\n\
Your job is to ANALYZE the responses — do NOT write a merged answer. Output a structured analysis with:\n\n\
1. CONSENSUS: What the models agreed on (high confidence areas)\n\
2. CONTRADICTIONS: Where models disagree (areas needing judgment)\n\
3. COVERAGE_GAPS: Important aspects only some models addressed\n\
4. UNIQUE_INSIGHTS: Valuable points from individual models\n\
5. BLIND_SPOTS: Important aspects none of the models addressed\n\n\
Original question:\n{}\n\n",
        original_prompt
    );

    for (i, (model, content)) in responses.iter().enumerate() {
        judge_prompt.push_str(&format!("Model {} ({}):\n{}\n\n", i + 1, model, content));
    }

    judge_prompt.push_str("Output your structured analysis now.");

    let mut judge_req = request.clone();
    judge_req.model = judge_model.to_string();
    if let Some(msg) = judge_req.messages.get_mut(0) {
        msg["content"] = serde_json::Value::String(judge_prompt);
    }

    let judge_result = call_provider(provider, &judge_req).await?;
    side_cost.merge(&judge_result.cost);

    let judge_analysis = judge_result
        .response
        .choices
        .first()
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    // Phase 3: Synthesizer — receives original prompt + judge's analysis, writes final answer
    let synth_prompt = format!(
        "You are answering a question. A panel of AI models provided initial responses, \
and a judge analyzed them. Use the judge's analysis to write the best possible answer.\n\n\
Original question:\n{}\n\n\
Judge's analysis of panel responses:\n{}\n\n\
Write the final answer. Incorporate consensus areas, resolve contradictions using your judgment, \
address coverage gaps and blind spots, and include unique insights where relevant.",
        original_prompt, judge_analysis
    );

    let mut synth_req = request.clone();
    synth_req.model = synth_model.to_string();
    if let Some(msg) = synth_req.messages.get_mut(0) {
        msg["content"] = serde_json::Value::String(synth_prompt);
    }

    tracing::info!(
        "deliberation: {} panel models, judge={}, synth={}",
        responses.len(),
        judge_model,
        synth_model
    );

    let mut out = call_provider(provider, &synth_req).await?;
    out.cost.merge(&side_cost);
    Ok(out)
}


/// Fusion: Test+Vote — fan out to N models in parallel, test each
/// candidate against the unit tests, return the first that passes.
/// Requires `test_code` and `entry_point` fields in the request.
pub async fn test_vote_models(
    provider: &ProviderConfig,
    models: &[String],
    request: &ChatCompletionRequest,
    test_code: &str,
    entry_point: &str,
) -> Result<ProviderResult, String> {
    use std::sync::atomic::{AtomicU64, Ordering};

    if models.is_empty() {
        return Err("No models for test_vote".to_string());
    }

    // Extract the prompt text (first user message content)
    let prompt_text = request
        .messages
        .first()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    // Fan out to all models in parallel
    let mut set = tokio::task::JoinSet::new();
    for model in models {
        let mut req = request.clone();
        req.model = model.clone();
        let provider = provider.clone();
        set.spawn(async move { call_provider(&provider, &req).await });
    }

    // Unique temp file counter — avoids collision between concurrent requests
    static FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
    let file_id = FILE_COUNTER.fetch_add(1, Ordering::SeqCst);

    // Collect results and test each as it arrives. Every model was dispatched, so
    // every model's tokens are billed — aborting the client future does not un-bill
    // them. Drain the set to observe each cost rather than reporting only the
    // winner's, then hand the winner the whole bill.
    let mut fallback: Option<ProviderResult> = None;
    let mut winner: Option<ProviderResult> = None;
    let mut billed = CostBreakdown::zero(&request.model);

    while let Some(res) = set.join_next().await {
        let r = match res {
            Ok(Ok(r)) => r,
            _ => continue,
        };
        billed.merge(&r.cost);

        // Keep first successful result as fallback
        if fallback.is_none() {
            fallback = Some(r.clone());
        }

        if winner.is_some() {
            continue; // already have a passing candidate; just drain for accounting
        }

        if let Some(content) = r
            .response
            .choices
            .first()
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
        {
            let completion_code = extract_completion_code(content);
            // Detect if model returned the full function (including signature).
            // If so, don't prepend the prompt (would duplicate the function def).
            // But do prepend any imports from the prompt (cloud models often drop them).
            let test_file = if has_matching_func_def(prompt_text, &completion_code) {
                let imports = extract_imports(prompt_text);
                format!(
                    "{}{}\n{}\n\ncheck({})\n",
                    imports, completion_code, test_code, entry_point
                )
            } else {
                format!(
                    "{}\n{}\n\n{}\ncheck({})\n",
                    prompt_text, completion_code, test_code, entry_point
                )
            };

            let tmp = std::env::temp_dir().join(format!(
                "stoke_test_{}_{}.py",
                std::process::id(),
                file_id
            ));
            if std::fs::write(&tmp, &test_file).is_err() {
                continue;
            }

            let test_result = run_python_sandboxed(&tmp);
            let _ = std::fs::remove_file(&tmp);

            if let Ok(output) = test_result {
                if output.status.success() {
                    tracing::info!("test_vote: model {} passed tests", r.response.model);
                    winner = Some(r);
                }
            }
        }
    }

    if let Some(mut w) = winner {
        w.cost = billed;
        return Ok(w);
    }

    // No candidate passed — return first result as fallback
    tracing::warn!("test_vote: no candidate passed tests, returning first result");
    let mut out = fallback.ok_or_else(|| "All models failed in test_vote".to_string())?;
    out.cost = billed;
    Ok(out)
}

/// Extract code from a model response — handles markdown code blocks.
pub fn extract_completion_code(content: &str) -> String {
    if let Some(start) = content.find("```python") {
        if let Some(end) = content[start + 9..].find("```") {
            return content[start + 9..start + 9 + end].to_string();
        }
    }
    if let Some(start) = content.find("```") {
        if let Some(end) = content[start + 3..].find("```") {
            return content[start + 3..start + 3 + end]
                .trim_start_matches('\n')
                .to_string();
        }
    }
    content.to_string()
}

/// Check if a completion contains a function definition matching one in the prompt.
/// Used to detect when a model returns the full function (including signature) instead
/// of just the body — prevents duplicating the function def by prepending the prompt.
/// Also extracts imports from the prompt that cloud models often drop.
fn has_matching_func_def(prompt: &str, completion: &str) -> bool {
    // Extract first function name from prompt
    if let Some(start) = prompt.find("def ") {
        let after_def = &prompt[start + 4..];
        if let Some(paren) = after_def.find('(') {
            let func_name = after_def[..paren].trim();
            if !func_name.is_empty() {
                return completion.contains(&format!("def {}", func_name));
            }
        }
    }
    false
}

/// Extract import lines from a prompt (e.g. "from typing import List")
fn extract_imports(prompt: &str) -> String {
    let mut imports: Vec<&str> = Vec::new();
    for line in prompt.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("from ") || trimmed.starts_with("import ") {
            imports.push(line);
        }
    }
    if imports.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", imports.join("\n"))
    }
}

/// Fusion: Cascade+Test — try each model sequentially, test each candidate,
/// return the first that passes. Cheaper than test_vote (no parallel fan-out).
/// Enables local→cloud fallback: try local models first, cloud only if all fail.
pub async fn cascade_test_models(
    provider: &ProviderConfig,
    models: &[String],
    request: &ChatCompletionRequest,
    test_code: &str,
    entry_point: &str,
) -> Result<ProviderResult, String> {
    use std::sync::atomic::{AtomicU64, Ordering};

    if models.is_empty() {
        return Err("No models for cascade_test".to_string());
    }

    let prompt_text = request
        .messages
        .first()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    static FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut last_result: Option<ProviderResult> = None;
    // A candidate that generated code and then failed the tests still burned
    // tokens. Carry every attempt's cost forward onto whichever result we return.
    let mut billed = CostBreakdown::zero(&request.model);

    for model in models {
        let mut req = request.clone();
        req.model = model.clone();

        tracing::info!("cascade_test: trying model {}", model);

        let result = match call_provider(provider, &req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("cascade_test: model {} failed: {}", model, e);
                continue;
            }
        };
        billed.merge(&result.cost);

        if last_result.is_none() {
            last_result = Some(result.clone());
        }

        if let Some(content) = result
            .response
            .choices
            .first()
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
        {
            let completion_code = extract_completion_code(content);
            // Detect if model returned the full function (including signature).
            let test_file = if has_matching_func_def(prompt_text, &completion_code) {
                let imports = extract_imports(prompt_text);
                format!(
                    "{}{}\n{}\n\ncheck({})\n",
                    imports, completion_code, test_code, entry_point
                )
            } else {
                format!(
                    "{}\n{}\n\n{}\ncheck({})\n",
                    prompt_text, completion_code, test_code, entry_point
                )
            };

            let file_id = FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
            let tmp = std::env::temp_dir().join(format!(
                "stoke_ctest_{}_{}.py",
                std::process::id(),
                file_id
            ));

            if std::fs::write(&tmp, &test_file).is_err() {
                continue;
            }

            let test_result = run_python_sandboxed(&tmp);
            let _ = std::fs::remove_file(&tmp);

            if let Ok(output) = test_result {
                if output.status.success() {
                    tracing::info!("cascade_test: model {} passed tests", model);
                    let mut out = result;
                    out.cost = billed;
                    return Ok(out);
                }
            }
        }
    }

    tracing::warn!("cascade_test: no model passed tests, returning last result");
    let mut out = last_result.ok_or_else(|| "All models failed in cascade_test".to_string())?;
    out.cost = billed;
    Ok(out)
}


pub async fn cascade(
    providers: Vec<&ProviderConfig>,
    request: &ChatCompletionRequest,
) -> Result<ProviderResult, String> {
    let mut last_err = String::new();
    for p in providers {
        match call_provider(p, request).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_err = e;
                tracing::warn!("Cascade: provider {} failed, trying next", p.name);
            }
        }
    }
    Err(format!("All providers failed in cascade. Last error: {}", last_err))
}

