//! Shared fail-closed admission and reservation settlement for HTTP handlers.
use serde_json::Value;

pub fn capped(config: &crate::Config, key: &str) -> bool {
    config
        .keys
        .iter()
        .any(|p| p.key == key && p.budget_usd > 0.0)
}

pub fn native_policy(config: &crate::Config) -> Result<(), String> {
    let p = &config.plugins;
    if !p.pre_request.is_empty()
        || !p.prompt_filter.is_empty()
        || !p.post_response.is_empty()
        || !p.scripts.is_empty()
    {
        return Err("Native protocol cannot enforce configured global policy hooks; use nonstream Chat Completions or remove those hooks explicitly".into());
    }
    Ok(())
}

/// Validate numeric limits even for unlimited callers. For capped requests,
/// reject provider features whose bill cannot be bounded by text token prices.
pub fn normalize(
    body: &mut Value,
    output_field: &str,
    default: u64,
    max_n: usize,
    finite: bool,
) -> Result<(u64, u64), String> {
    let obj = body.as_object_mut().ok_or("request must be an object")?;
    let mut output = None;
    for field in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
        if let Some(v) = obj.get(field) {
            let n = v
                .as_u64()
                .filter(|n| *n > 0 && *n <= u32::MAX as u64)
                .ok_or_else(|| format!("{field} must be a positive integer <= {}", u32::MAX))?;
            if (output_field == "max_output_tokens" && field != "max_output_tokens")
                || (output_field == "max_tokens" && field == "max_output_tokens")
            {
                return Err(format!("{field} is unsupported on this endpoint"));
            }
            if output.is_some() {
                return Err("specify only one output token limit".into());
            }
            output = Some(n);
        }
    }
    let output = output.unwrap_or(default);
    if !obj.contains_key("max_tokens")
        && !obj.contains_key("max_completion_tokens")
        && !obj.contains_key("max_output_tokens")
    {
        obj.insert(output_field.into(), output.into());
    }
    let mut copies = 1;
    for field in ["n", "n_samples", "best_of"] {
        if let Some(v) = obj.get(field) {
            let n = v
                .as_u64()
                .filter(|n| *n > 0 && *n <= max_n as u64)
                .ok_or_else(|| format!("{field} must be a positive integer <= {max_n}"))?;
            if field == "n" {
                copies = n;
            }
            if finite && field == "best_of" {
                return Err("best_of is unsupported with a finite budget".into());
            }
        }
    }
    if finite {
        let allowed = [
            "model",
            "messages",
            "input",
            "instructions",
            "system",
            "tools",
            "tool_choice",
            "temperature",
            "top_p",
            "top_k",
            "stop",
            "stop_sequences",
            "max_tokens",
            "max_completion_tokens",
            "max_output_tokens",
            "n",
            "n_samples",
            "stream",
            "stream_options",
            "routing",
            "vote_models",
            "metadata",
            "user",
            "seed",
            "frequency_penalty",
            "presence_penalty",
            "response_format",
            "logprobs",
            "top_logprobs",
            "parallel_tool_calls",
            "store",
            "reasoning",
            "reasoning_effort",
        ];
        if let Some(k) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!(
                "field '{k}' has no supported finite-budget billing bound"
            ));
        }
        fn text_only(v: &Value) -> bool {
            match v {
                Value::Array(a) => a.iter().all(text_only),
                Value::Object(o) => {
                    if o.contains_key("image_url")
                        || o.contains_key("file_id")
                        || o.contains_key("file_url")
                        || o.contains_key("source")
                    {
                        return false;
                    }
                    if let Some(t) = o.get("type").and_then(Value::as_str) {
                        if !matches!(
                            t,
                            "text"
                                | "input_text"
                                | "output_text"
                                | "message"
                                | "function"
                                | "function_call"
                                | "function_call_output"
                                | "tool_use"
                                | "tool_result"
                                | "json_object"
                                | "json_schema"
                                | "custom"
                        ) {
                            return false;
                        }
                    }
                    o.iter().all(|(k, v)| {
                        matches!(k.as_str(), "parameters" | "input_schema" | "schema")
                            || text_only(v)
                    })
                }
                _ => true,
            }
        }
        if !text_only(body) {
            return Err(
                "media, remote content and hosted tools are unsupported with a finite budget"
                    .into(),
            );
        }
    }
    Ok((output, copies))
}

/// UTF-8 bytes bound text tokens conservatively, including serialized tools,
/// schemas and metadata. Add per-message framing rather than dividing by four.
pub fn input_bound(body: &Value) -> u64 {
    let bytes = serde_json::to_vec(body)
        .map(|s| s.len() as u64)
        .unwrap_or(u64::MAX / 2);
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .map_or(1, |m| m.len() as u64);
    bytes
        .saturating_add(256)
        .saturating_add(messages.saturating_mul(64))
}

pub fn usage(body: Option<&Value>, input: &str, output: &str) -> Option<Value> {
    let u = body?;
    let mut p = u.get(input)?.as_u64()?;
    let c = u.get(output)?.as_u64()?;
    if input == "input_tokens" {
        for k in ["cache_read_input_tokens", "cache_creation_input_tokens"] {
            if let Some(v) = u.get(k) {
                p = p.checked_add(v.as_u64()?)?;
            }
        }
    }
    p.checked_add(c)?;
    Some(serde_json::json!({"prompt_tokens":p,"completion_tokens":c}))
}

pub fn dispatch(reservation: &mut Option<crate::budget::SpendReservation>) {
    if let Some(r) = reservation {
        r.mark_dispatched();
    }
}

pub fn settle(
    budget: &crate::budget::BudgetGuard,
    key: &str,
    reservation: Option<crate::budget::SpendReservation>,
    cost: f64,
    measured: bool,
) -> Result<f64, String> {
    let charge = if measured {
        cost
    } else {
        cost.max(reservation.as_ref().map_or(0.0, |r| r.amount()))
    };
    if let Some(r) = reservation {
        r.settle(charge, !measured)?;
    } else if measured {
        budget.record_spend(key, charge);
    } else {
        budget.record_spend_estimated(key, charge);
    }
    Ok(charge)
}

/// The legacy Plugins::post_response API returns the original body on errors.
/// Mandatory output policy needs a Result, so handlers use this strict adapter.
pub async fn post_response(
    urls: &[String],
    model: &str,
    mut response: Value,
    cost_usd: f64,
    elapsed_ms: u64,
    api_key: &str,
) -> Result<Value, String> {
    for url in urls {
        let parsed = reqwest::Url::parse(url).map_err(|_| "Invalid output policy URL")?;
        let host = parsed.host_str().ok_or("Output policy URL needs a host")?;
        let forbidden = match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) => {
                ip.is_unspecified()
                    || matches!(ip.octets(), [169, 254, 169, 254] | [169, 254, 170, 2])
            }
            Ok(std::net::IpAddr::V6(ip)) => {
                ip.is_unspecified() || ip.is_multicast() || ip.segments()[0] == 0xfd00
            }
            Err(_) => {
                host.eq_ignore_ascii_case("metadata")
                    || host.eq_ignore_ascii_case("metadata.google.internal")
            }
        };
        if !matches!(parsed.scheme(), "http" | "https") || forbidden {
            return Err("Forbidden output policy URL".into());
        }
        let ctx = crate::plugins::PostResponseContext {
            model,
            response: &response,
            cost_usd,
            elapsed_ms,
            api_key,
        };
        let reply = crate::router::SHARED_CLIENT
            .post(url)
            .timeout(std::time::Duration::from_secs(5))
            .json(&ctx)
            .send()
            .await
            .map_err(|_| "Required output policy unavailable")?;
        if !reply.status().is_success() {
            return Err("Required output policy returned an error".into());
        }
        let result = reply
            .json::<crate::plugins::PostResponseResult>()
            .await
            .map_err(|_| "Invalid output policy response")?;
        if let Some(transformed) = result.response {
            response = transformed;
        }
    }
    Ok(response)
}
