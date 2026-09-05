use once_cell::sync::Lazy;

pub static OAUTH_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("OAuth HTTP client must build")
});

/// Refuse to put a subscription credential on anything except the exact,
/// operator-selected first-party HTTPS host. The test-only base override
/// (`STOKE_TEST_SUBSCRIPTION_BASES`) admits plain-HTTP loopback mocks so the
/// smoke harness can exercise OAuth-bearing paths deterministically; unset,
/// this check is absolute.
pub fn validate_oauth_destination(url: &str, allowed_host: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "invalid OAuth upstream URL".to_string())?;
    if test_allowed_subscription_bases().contains(&url.trim_end_matches('/').to_string()) {
        return Ok(());
    }
    if parsed.scheme() != "https"
        || parsed.host_str() != Some(allowed_host)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port_or_known_default() != Some(443)
    {
        return Err(format!(
            "OAuth passthrough requires the exact HTTPS host {allowed_host}"
        ));
    }
    Ok(())
}

/// The exact Responses endpoint Codex's subscription backend serves.
pub const CHATGPT_CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";

/// Test-only escape hatch for deterministic smoke harnesses: `STOKE_TEST_SUBSCRIPTION_BASES`
/// names the mock base URLs the gateway may accept IN ADDITION to the pinned
/// first-party hosts. Unset (production) the override does not exist and the
/// pins are absolute — fail-closed by default.
fn test_allowed_subscription_bases() -> Vec<String> {
    let raw = std::env::var("STOKE_TEST_SUBSCRIPTION_BASES").unwrap_or_default();
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let base = entry.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            continue;
        }
        // The pins are checked against full endpoint URLs too
        // ("<base>/responses", "<base>/v1/messages"), so the override admits
        // the same shapes for each allowed mock base.
        out.push(base.clone());
        out.push(format!("{base}/responses"));
        out.push(format!("{base}/v1/messages"));
    }
    out
}

/// The single endpoint a `codex_subscription` provider may dispatch to.
pub fn subscription_responses_endpoint(base_url: &str) -> Result<String, String> {
    let base = base_url.trim_end_matches('/');
    if base != CHATGPT_CODEX_BASE && !test_allowed_subscription_bases().contains(&base.to_string())
    {
        return Err(format!(
            "codex_subscription base_url must be exactly {CHATGPT_CODEX_BASE}"
        ));
    }
    Ok(format!("{base}/responses"))
}

/// The exact Anthropic API root Claude subscription OAuth credentials serve.
pub const CLAUDE_API_BASE: &str = "https://api.anthropic.com";

/// The host a `claude_subscription` credential may be sent to.
pub const CLAUDE_API_HOST: &str = "api.anthropic.com";

/// The single endpoint a `claude_subscription` provider may dispatch to.
pub fn claude_subscription_messages_endpoint(base_url: &str) -> Result<String, String> {
    let base = base_url.trim_end_matches('/');
    if base != CLAUDE_API_BASE && !test_allowed_subscription_bases().contains(&base.to_string()) {
        return Err(format!(
            "claude_subscription base_url must be exactly {CLAUDE_API_BASE}"
        ));
    }
    Ok(format!("{base}/v1/messages"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_base_override_does_not_exist_by_default() {
        // Fail-closed: production never sets STOKE_TEST_SUBSCRIPTION_BASES, so
        // the pins stay absolute. Assert the override truly changes nothing
        // when unset for a non-pinned URL.
        std::env::remove_var("STOKE_TEST_SUBSCRIPTION_BASES");
        assert!(subscription_responses_endpoint("http://127.0.0.1:1/v1").is_err());
        assert!(claude_subscription_messages_endpoint("http://127.0.0.1:1").is_err());
        assert!(validate_oauth_destination("http://127.0.0.1:1/x", "chatgpt.com").is_err());
    }

    #[tokio::test]
    async fn oauth_client_never_follows_redirects() {
        use axum::{http::StatusCode, response::Redirect, routing::get, Router};

        let app = Router::new()
            .route("/start", get(|| async { Redirect::temporary("/leak") }))
            .route("/leak", get(|| async { StatusCode::OK }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let response = OAUTH_CLIENT
            .get(format!("http://{address}/start"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    }

    #[test]
    fn oauth_destination_accepts_only_the_exact_https_host() {
        assert!(validate_oauth_destination(
            "https://api.anthropic.com/v1/messages",
            "api.anthropic.com"
        )
        .is_ok());
        assert!(validate_oauth_destination(
            "https://chatgpt.com/backend-api/codex/responses",
            "chatgpt.com"
        )
        .is_ok());

        for hostile in [
            "http://api.anthropic.com/v1/messages",
            "https://api.anthropic.com.evil.test/v1/messages",
            "https://***@evil.test/v1/messages",
            "https://api.anthropic.com:444/v1/messages",
        ] {
            assert!(
                validate_oauth_destination(hostile, "api.anthropic.com").is_err(),
                "must reject {hostile}"
            );
        }
    }

    #[test]
    fn subscription_endpoint_is_exact_or_refused() {
        assert_eq!(
            subscription_responses_endpoint("https://chatgpt.com/backend-api/codex").unwrap(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            subscription_responses_endpoint("https://chatgpt.com/backend-api/codex/").unwrap(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        for hostile in [
            "https://api.openai.com/v1",
            "https://chatgpt.com/backend-api/codex.evil.test",
            "https://chatgpt.com.evil.test/backend-api/codex",
            "http://chatgpt.com/backend-api/codex",
            "https://chatgpt.com/backend-api/codex/../v1",
        ] {
            assert!(
                subscription_responses_endpoint(hostile).is_err(),
                "must refuse {hostile}"
            );
        }
    }

    #[test]
    fn claude_subscription_endpoint_is_exact_or_refused() {
        assert_eq!(
            claude_subscription_messages_endpoint("https://api.anthropic.com").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            claude_subscription_messages_endpoint("https://api.anthropic.com/").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        for hostile in [
            "https://api.openai.com/v1",
            "https://api.anthropic.com.evil.test",
            "https://anthropic.evil.test/api.anthropic.com",
            "http://api.anthropic.com",
            "https://api.anthropic.com/v1/messages",
        ] {
            assert!(
                claude_subscription_messages_endpoint(hostile).is_err(),
                "must refuse {hostile}"
            );
        }
    }

    #[test]
    fn claude_subscription_base_survives_oauth_destination_validation() {
        let url = claude_subscription_messages_endpoint("https://api.anthropic.com").unwrap();
        assert!(validate_oauth_destination(&url, CLAUDE_API_HOST).is_ok());
    }
}
