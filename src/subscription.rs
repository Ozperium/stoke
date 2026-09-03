use once_cell::sync::Lazy;

pub static OAUTH_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("OAuth HTTP client must build")
});

/// Refuse to put a subscription credential on anything except the exact,
/// operator-selected first-party HTTPS host.
pub fn validate_oauth_destination(url: &str, allowed_host: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "invalid OAuth upstream URL".to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
            "https://api.anthropic.com@evil.test/v1/messages",
            "https://api.anthropic.com:444/v1/messages",
        ] {
            assert!(
                validate_oauth_destination(hostile, "api.anthropic.com").is_err(),
                "must reject {hostile}"
            );
        }
    }
}
