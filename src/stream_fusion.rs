use serde_json::Value;
use tokio::task::JoinHandle;

use crate::config::ProviderConfig;
use crate::router::SHARED_CLIENT;

/// Race multiple providers in parallel for a streaming request.
///
/// FREE providers (local models, $0) race in parallel — first to respond wins.
/// PAID providers (cloud APIs) are sequential fallback — NOT raced, to avoid
/// paying for N requests when only 1 is used.
///
/// Use cases:
/// - Local + cloud: race local models, fall back to cloud only if all local fail
/// - Multi-local: race 2 Ollama instances, first responding wins ($0 cost)
/// - TTFT optimization: skip slow-loading local models without wasting cloud budget
///
/// Returns `(winner_name, response)` so the caller can inject which provider won.
pub async fn stream_race(
    providers: Vec<&ProviderConfig>,
    body: &Value,
) -> Result<(String, reqwest::Response), String> {
    if providers.is_empty() {
        return Err("No providers for stream_race".to_string());
    }

    // Split into free (race) and paid (sequential fallback)
    let pricer = crate::cost::Pricer::default();
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("");
    let is_free = |p: &ProviderConfig| {
        // Check if the model has a price on this provider
        // Local providers (Ollama) serve free models; cloud providers cost money
        let pricing = pricer.get_pricing(model);
        match pricing {
            Some(p) => p.input_per_1m == 0.0 && p.output_per_1m == 0.0,
            None => {
                // Unknown model: assume local provider (Ollama) is free, cloud is paid
                !p.name.contains("openrouter") && !p.name.contains("openai")
                    && !p.name.contains("anthropic") && !p.name.contains("google")
            }
        }
    };

    let free_providers: Vec<&ProviderConfig> = providers.iter().filter(|p| is_free(p)).copied().collect();
    let paid_providers: Vec<&ProviderConfig> = providers.iter().filter(|p| !is_free(p)).copied().collect();

    tracing::info!(
        "stream_race: {} free (racing), {} paid (sequential fallback)",
        free_providers.len(),
        paid_providers.len()
    );

    // Race free providers
    if !free_providers.is_empty() {
        match race_providers(free_providers, body).await {
            Ok(winner) => return Ok(winner),
            Err(e) => {
                tracing::warn!("stream_race: all free providers failed: {}", e);
            }
        }
    }

    // Sequential fallback to paid providers (no racing — avoid double billing)
    for provider in &paid_providers {
        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        );
        let api_key = provider.resolve_api_key();
        let body = body.clone();
        let provider_name = provider.name.clone();

        tracing::info!("stream_race: falling back to paid provider: {}", provider_name);

        let resp = (&*SHARED_CLIENT)
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                tracing::info!("stream_race: paid provider {} succeeded", provider_name);
                return Ok((provider_name, r));
            }
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                tracing::warn!("stream_race: paid provider {} returned {}: {}", provider_name, status, text);
            }
            Err(e) => {
                tracing::warn!("stream_race: paid provider {} failed: {}", provider_name, e);
            }
        }
    }

    Err("All providers (free + paid) failed in stream_race".to_string())
}

/// Race multiple free providers in parallel. First to return HTTP 200 wins.
async fn race_providers(
    providers: Vec<&ProviderConfig>,
    body: &Value,
) -> Result<(String, reqwest::Response), String> {
    if providers.is_empty() {
        return Err("No free providers to race".to_string());
    }

    if providers.len() == 1 {
        let provider = providers[0];
        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        );
        let resp = (&*SHARED_CLIENT)
            .post(&url)
            .header("Authorization", format!("Bearer {}", provider.resolve_api_key()))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| format!("Provider {}: {}", provider.name, e))?;

        if resp.status().is_success() {
            return Ok((provider.name.clone(), resp));
        }
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Provider {}: {} {}", provider.name, status, text));
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<(String, reqwest::Response), String>>(1);

    let mut handles: Vec<JoinHandle<()>> = Vec::with_capacity(providers.len());

    for provider in &providers {
        let tx = tx.clone();
        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        );
        let api_key = provider.resolve_api_key();
        let provider_name = provider.name.clone();
        let body = body.clone();

        let handle = tokio::spawn(async move {
            let resp = (&*SHARED_CLIENT)
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let _ = tx.send(Ok((provider_name, r))).await;
                }
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    let _ = tx
                        .send(Err(format!(
                            "Provider {}: {} {}",
                            provider_name, status, text
                        )))
                        .await;
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(format!("Provider {}: {}", provider_name, e)))
                        .await;
                }
            }
        });
        handles.push(handle);
    }

    drop(tx);

    let mut errors: Vec<String> = Vec::new();

    let winner = loop {
        match rx.recv().await {
            Some(Ok(result)) => break result,
            Some(Err(e)) => {
                tracing::warn!("race_providers: {}", e);
                errors.push(e);
            }
            None => {
                return Err(format!(
                    "All {} free providers failed: {}",
                    providers.len(),
                    errors.join("; ")
                ));
            }
        }
    };

    // Abort remaining (they're free, no cost)
    for handle in &handles {
        handle.abort();
    }

    tracing::info!("race_providers: winner = {}", winner.0);
    Ok(winner)
}

/// Race multiple models on the SAME provider for a streaming request.
///
/// Like `stream_race` but all models go through one provider (e.g., Ollama).
/// Used when `vote_models` is specified — each model gets its own request,
/// first to respond wins.
pub async fn stream_race_models(
    provider: &ProviderConfig,
    models: &[String],
    body: &Value,
) -> Result<(String, String, reqwest::Response), String> {
    if models.is_empty() {
        return Err("No models for stream_race".to_string());
    }

    if models.len() == 1 {
        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        );
        let mut body = body.clone();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("model".into(), Value::String(models[0].clone()));
        }
        let resp = (&*SHARED_CLIENT)
            .post(&url)
            .header("Authorization", format!("Bearer {}", provider.resolve_api_key()))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Provider {}: {}", provider.name, e))?;

        if resp.status().is_success() {
            return Ok((provider.name.clone(), models[0].clone(), resp));
        }
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Provider {}: {} {}",
            provider.name, status, text
        ));
    }

    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<Result<(String, String, reqwest::Response), String>>(1);

    let mut handles: Vec<JoinHandle<()>> = Vec::with_capacity(models.len());

    for model in models {
        let tx = tx.clone();
        let url = format!(
            "{}/chat/completions",
            provider.base_url.trim_end_matches('/')
        );
        let api_key = provider.resolve_api_key();
        let provider_name = provider.name.clone();
        let model = model.clone();
        let mut body = body.clone();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("model".into(), Value::String(model.clone()));
        }

        let handle = tokio::spawn(async move {
            let resp = (&*SHARED_CLIENT)
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    let _ = tx.send(Ok((provider_name, model, r))).await;
                }
                Ok(r) => {
                    let status = r.status();
                    let _ = tx
                        .send(Err(format!(
                            "Model {}: {} ",
                            model, status
                        )))
                        .await;
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("Model {}: {}", model, e))).await;
                }
            }
        });
        handles.push(handle);
    }

    drop(tx);

    let mut errors: Vec<String> = Vec::new();

    let winner = loop {
        match rx.recv().await {
            Some(Ok(result)) => break result,
            Some(Err(e)) => {
                tracing::warn!("stream_race_models: {}", e);
                errors.push(e);
            }
            None => {
                return Err(format!(
                    "All {} models failed in stream_race: {}",
                    models.len(),
                    errors.join("; ")
                ));
            }
        }
    };

    for handle in &handles {
        handle.abort();
    }

    tracing::info!("stream_race_models: winner = {}/{}", winner.0, winner.1);
    Ok(winner)
}