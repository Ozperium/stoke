use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use stoke::config::ProviderConfig;
use stoke::router::{call_provider_hop_detailed, ChatCompletionRequest};

const HTTP_DATE: &str = "Tue, 01 Jan 2030 00:00:00 GMT";

fn provider(base_url: String) -> ProviderConfig {
    ProviderConfig {
        name: "retry-hints-test".to_string(),
        r#type: "openai_compatible".to_string(),
        base_url,
        api_key: "test-key".to_string(),
        api_key_env: String::new(),
        models: vec!["test-model".to_string()],
        tier: "local".to_string(),
    }
}

fn request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "test-model".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
        temperature: Some(0.0),
        max_tokens: Some(16),
        stream: Some(false),
        extra: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn captures_literal_lowercase_retry_hint_and_integer_retry_after() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request_bytes = [0_u8; 4096];
        let _ = stream.read(&mut request_bytes).unwrap();
        let body = b"{\"error\":{\"message\":\"cooldown\"}}";
        write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nRetry-After: 1\r\nx-should-retry: true\r\nX-Leak: do-not-forward\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    });

    let error = match call_provider_hop_detailed(
        &provider(format!("http://{}/v1", address)),
        &request(),
        0,
    )
    .await
    {
        Ok(_) => panic!("the mock upstream must fail"),
        Err(error) => error,
    };

    let upstream = error.upstream.expect("HTTP failure metadata");
    assert_eq!(upstream.status.as_u16(), 503);
    assert_eq!(upstream.retry_after.as_deref(), Some("1"));
    assert_eq!(upstream.should_retry.as_deref(), Some("true"));
    server.join().unwrap();
}

#[tokio::test]
async fn rejects_uppercase_retry_values_without_losing_status() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request_bytes = [0_u8; 4096];
        let _ = stream.read(&mut request_bytes).unwrap();
        let body = b"failure";
        write!(
            stream,
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nRetry-After: secret-header-data\r\nx-should-retry: TRUE\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    });

    let error = match call_provider_hop_detailed(
        &provider(format!("http://{}/v1", address)),
        &request(),
        0,
    )
    .await
    {
        Ok(_) => panic!("the mock upstream must fail"),
        Err(error) => error,
    };

    let upstream = error.upstream.expect("HTTP failure metadata");
    assert_eq!(upstream.status.as_u16(), 429);
    assert_eq!(upstream.retry_after, None);
    assert_eq!(upstream.should_retry, None);
    server.join().unwrap();
}

#[tokio::test]
async fn preserves_valid_http_date_retry_after_unchanged() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request_bytes = [0_u8; 4096];
        let _ = stream.read(&mut request_bytes).unwrap();
        let body = b"failure";
        write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nRetry-After: {}\r\nx-should-retry: false\r\nConnection: close\r\n\r\n",
            body.len(),
            HTTP_DATE
        )
        .unwrap();
        stream.write_all(body).unwrap();
    });

    let error = match call_provider_hop_detailed(
        &provider(format!("http://{}/v1", address)),
        &request(),
        0,
    )
    .await
    {
        Ok(_) => panic!("the mock upstream must fail"),
        Err(error) => error,
    };

    let upstream = error.upstream.expect("HTTP failure metadata");
    assert_eq!(upstream.retry_after.as_deref(), Some(HTTP_DATE));
    assert_eq!(upstream.should_retry.as_deref(), Some("false"));
    server.join().unwrap();
}

#[tokio::test]
async fn rejects_ambiguous_duplicate_retry_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request_bytes = [0_u8; 4096];
        let _ = stream.read(&mut request_bytes).unwrap();
        let body = b"failure";
        write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nRetry-After: 1\r\nRetry-After: 2\r\nx-should-retry: true\r\nx-should-retry: false\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    });

    let error = match call_provider_hop_detailed(
        &provider(format!("http://{}/v1", address)),
        &request(),
        0,
    )
    .await
    {
        Ok(_) => panic!("the mock upstream must fail"),
        Err(error) => error,
    };

    let upstream = error.upstream.expect("HTTP failure metadata");
    assert_eq!(upstream.retry_after, None);
    assert_eq!(upstream.should_retry, None);
    server.join().unwrap();
}
