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

    let pricer = crate::cost::global();
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("");

    // The pricing gate. This path posts directly rather than going through
    // router::call_provider_hop, so a provider that may not serve this model is
    // dropped from the race here — before any connection is opened.
    let mut refusal: Option<String> = None;
    let providers: Vec<&ProviderConfig> = providers
        .into_iter()
        .filter(|p| match pricer.allows(&p.tier, model) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("stream_race: excluding {}: {}", p.name, e);
                refusal = Some(e);
                false
            }
        })
        .collect();
    if providers.is_empty() {
        return Err(refusal.unwrap_or_else(|| "No providers for stream_race".to_string()));
    }

    // Free tiers race each other; racing metered providers would pay for N
    // answers to use one. Which tier a provider is on is declared in config —
    // never inferred from its name, which was how a provider called "my-gpu-box"
    // pointing at a paid API used to be raced as if it were free.
    let is_free = |p: &ProviderConfig| crate::cost::is_free_tier(&p.tier);

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
        // Metered leg: ask it to report usage so the stream can be billed from a
        // measurement rather than an estimate.
        let body = crate::sse::request_stream_usage(body, &provider.tier, &provider.r#type);
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

    // The pricing gate, per racing model: this path posts directly, and each
    // model in the race is a separate billed request.
    let pricer = crate::cost::global();
    let models: Vec<String> = {
        let mut refusal: Option<String> = None;
        let kept: Vec<String> = models
            .iter()
            .filter(|m| match pricer.allows(&provider.tier, m) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("stream_race_models: excluding {}: {}", m, e);
                    refusal = Some(e);
                    false
                }
            })
            .cloned()
            .collect();
        if kept.is_empty() {
            return Err(refusal.unwrap_or_else(|| "No models for stream_race".to_string()));
        }
        kept
    };
    let models: &[String] = &models;

    // Every model here is served by the same provider, so one decision covers
    // them all: if it charges, ask it to report usage.
    let body = &crate::sse::request_stream_usage(body, &provider.tier, &provider.r#type);

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

    // Racing N models means the provider generates N answers and you use one. On
    // your own hardware that buys tail latency for the price of some electricity.
    // On a metered provider it buys the same latency for N times the money — and
    // only the winning stream gets a meter, so the losers' tokens would be billed
    // by the provider and invisible to the cap. `stream_race` already refuses to
    // race paid providers for exactly this reason; do the same here.
    if !crate::cost::is_free_tier(&provider.tier) {
        tracing::info!(
            "stream_race_models: {} is metered — trying {} models in order rather than racing",
            provider.name,
            models.len()
        );
        let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));
        let mut last_err = String::new();
        for m in models {
            let mut leg = body.clone();
            if let Some(obj) = leg.as_object_mut() {
                obj.insert("model".into(), Value::String(m.clone()));
            }
            match (&*SHARED_CLIENT)
                .post(&url)
                .header("Authorization", format!("Bearer {}", provider.resolve_api_key()))
                .header("Content-Type", "application/json")
                .json(&leg)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    return Ok((provider.name.clone(), m.clone(), r));
                }
                Ok(r) => {
                    let status = r.status();
                    last_err = format!("Provider {} model {}: {}", provider.name, m, status);
                    tracing::warn!("{}", last_err);
                }
                Err(e) => {
                    last_err = format!("Provider {} model {}: {}", provider.name, m, e);
                    tracing::warn!("{}", last_err);
                }
            }
        }
        return Err(if last_err.is_empty() {
            "All models failed in stream_race".to_string()
        } else {
            last_err
        });
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