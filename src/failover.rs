use serde_json::Value;
use std::time::Instant;

use crate::config::ProviderConfig;
use crate::nodes::{InflightGuard, NodeRegistry};

/// A successful streaming connection.
pub struct StreamWin {
    pub provider_name: String,
    pub connect_ms: u64,
    pub response: reqwest::Response,
    /// Held for the stream's lifetime so the node's in-flight count stays
    /// accurate through prefill and generation, not just handler scope.
    /// Drop it when the client stream ends.
    pub guard: Option<InflightGuard>,
}

/// Stream a response from a provider, with failover to backup providers
/// if the connection fails before any data is sent.
///
/// In-flight is counted from the connect attempt: for LLM streams the connect
/// phase IS the prefill, which is exactly the load placement must see.
/// Failed attempts are recorded against the node; the winner's connect time
/// feeds its latency EWMA.
pub async fn stream_with_failover(
    providers: Vec<&ProviderConfig>,
    body: &Value,
    registry: &NodeRegistry,
    hop: u32,
) -> Result<StreamWin, String> {
    stream_with_failover_detailed(providers, body, registry, hop)
        .await
        .map_err(|error| error.message)
}

pub async fn stream_with_failover_detailed(
    providers: Vec<&ProviderConfig>,
    body: &Value,
    registry: &NodeRegistry,
    hop: u32,
) -> Result<StreamWin, crate::router::ProviderError> {
    if providers.is_empty() {
        return Err(crate::router::ProviderError::local("No providers for streaming"));
    }

    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or_default();
    let mut last_error = crate::router::ProviderError::local("no provider attempted");

    for (i, provider) in providers.iter().enumerate() {
        // The pricing gate. This path builds its own request rather than going
        // through router::call_provider_hop, so it needs the check explicitly —
        // otherwise `stream: true` is a way to reach a metered provider with a
        // model Stoke cannot price, and streams already accrue no spend.
        if let Err(reason) = crate::cost::global().allows(&provider.tier, model) {
            tracing::warn!("stream_with_failover: skipping {}: {}", provider.name, reason);
            last_error = crate::router::ProviderError::local(reason);
            continue;
        }

        let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));
        let start = Instant::now();

        tracing::info!("stream_with_failover: trying provider {} ({})", provider.name, i);

        // A streamed response is forwarded byte-for-byte, so the only way to know
        // what it cost is to have the provider say so. Ask — but only where the
        // answer changes something, so a local Ollama stream stays exactly as it
        // was for the client reading it.
        let outgoing = crate::sse::request_stream_usage(body, &provider.tier, &provider.r#type);

        let guard = registry.begin(&provider.name);

        match (&*crate::router::SHARED_CLIENT)
            .post(&url)
            .header("Authorization", format!("Bearer {}", provider.resolve_api_key()))
            .header("Content-Type", "application/json")
            .header("x-stoke-hop", hop.saturating_add(1).to_string())
            .json(&outgoing)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    let connect_ms = start.elapsed().as_millis() as u64;
                    tracing::info!(
                        "stream_with_failover: provider {} connected in {}ms",
                        provider.name,
                        connect_ms
                    );
                    registry.record_success(&provider.name, connect_ms);
                    return Ok(StreamWin {
                        provider_name: provider.name.clone(),
                        connect_ms,
                        response: resp,
                        guard,
                    });
                } else {
                    let status = resp.status();
                    let (retry_after, should_retry) =
                        crate::router::capture_upstream_hints(&resp);
                    let text = resp.text().await.unwrap_or_default();
                    last_error = crate::router::ProviderError {
                        message: format!("Provider {}: {} {}", provider.name, status, text),
                        upstream: Some(crate::router::UpstreamErrorMetadata { status, retry_after, should_retry }),
                    };
                    registry.record_error(&provider.name);
                    tracing::warn!("stream_with_failover: provider {} returned {}", provider.name, status);
                }
            }
            Err(e) => {
                last_error = crate::router::ProviderError::local(format!("Provider {}: {}", provider.name, e));
                registry.record_error(&provider.name);
                tracing::warn!("stream_with_failover: provider {} failed: {}", provider.name, e);
            }
        }
        // guard drops here on failure — the attempt is no longer in flight
    }

    Err(crate::router::ProviderError {
        message: format!("All providers failed for streaming: {}", last_error.message),
        upstream: if providers.len() == 1 { last_error.upstream } else { None },
    })
}

/// Hedged dispatch: fire the same streaming request at two nodes, first to
/// connect (for LLM streams: first past prefill) wins; the loser's connection
/// is dropped, which stops its generation. Both nodes must be zero-marginal —
/// the caller enforces that; duplicate compute is the price of the latency.
/// Hop headers are sent on both attempts, so hedging across a federated
/// gateway is loop-safe.
pub async fn stream_hedged(
    a: &ProviderConfig,
    b: &ProviderConfig,
    body: &Value,
    registry: &NodeRegistry,
    hop: u32,
) -> Result<StreamWin, String> {
    let fa = stream_with_failover(vec![a], body, registry, hop);
    let fb = stream_with_failover(vec![b], body, registry, hop);
    tokio::pin!(fa);
    tokio::pin!(fb);
    tokio::select! {
        ra = &mut fa => match ra {
            Ok(win) => {
                tracing::info!("hedged dispatch: {} won", win.provider_name);
                Ok(win)
            }
            Err(_) => fb.await,
        },
        rb = &mut fb => match rb {
            Ok(win) => {
                tracing::info!("hedged dispatch: {} won", win.provider_name);
                Ok(win)
            }
            Err(_) => fa.await,
        },
    }
}
