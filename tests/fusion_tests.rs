use stoke::config::ProviderConfig;
use stoke::router::*;

fn mock_provider(name: &str, base_url: &str) -> ProviderConfig {
    ProviderConfig {
        name: name.to_string(),
        r#type: "openai_compatible".to_string(),
        base_url: base_url.to_string(),
        api_key: "test-key".to_string(),
        api_key_env: String::new(),
        models: vec![],
        tier: String::new(),
    }
}

fn mock_request() -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "test-model".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
        temperature: Some(0.0),
        max_tokens: Some(32),
        stream: Some(false),
        extra: serde_json::Map::new(),
    }
}

#[test]
fn test_extract_completion_code_plain() {
    let code = extract_completion_code("return a + b");
    assert_eq!(code, "return a + b");
}

#[test]
fn test_extract_completion_code_python_block() {
    let input = "Here is the code:\n```python\nreturn a + b\n```\nDone.";
    let code = extract_completion_code(input);
    assert_eq!(code, "\nreturn a + b\n");
}

#[test]
fn test_extract_completion_code_generic_block() {
    let input = "```\nreturn a + b\n```";
    let code = extract_completion_code(input);
    assert_eq!(code, "return a + b\n");
}

#[test]
fn test_extract_completion_code_nested_backticks() {
    // Should find the first ```python block
    let input = "```python\nx = 1\n```\n```python\ny = 2\n```";
    let code = extract_completion_code(input);
    assert_eq!(code, "\nx = 1\n");
}

#[test]
fn test_provider_config_resolve_api_key_direct() {
    let p = mock_provider("test", "http://localhost:11434/v1");
    assert_eq!(p.resolve_api_key(), "test-key");
}

#[test]
fn test_provider_config_resolve_api_key_env() {
    let p = ProviderConfig {
        name: "test".to_string(),
        r#type: "openai_compatible".to_string(),
        base_url: "http://localhost:11434/v1".to_string(),
        api_key: String::new(),
        api_key_env: "TEST_API_KEY_VAR".to_string(),
        models: vec![],
        tier: String::new(),
    };
    std::env::set_var("TEST_API_KEY_VAR", "env-key-value");
    assert_eq!(p.resolve_api_key(), "env-key-value");
    std::env::remove_var("TEST_API_KEY_VAR");
}

#[test]
fn test_provider_config_resolve_api_key_empty() {
    let p = ProviderConfig {
        name: "test".to_string(),
        r#type: "openai_compatible".to_string(),
        base_url: "http://localhost:11434/v1".to_string(),
        api_key: String::new(),
        api_key_env: String::new(),
        models: vec![],
        tier: String::new(),
    };
    assert_eq!(p.resolve_api_key(), "");
}

#[test]
fn test_mock_request_construction() {
    let req = mock_request();
    assert_eq!(req.model, "test-model");
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.temperature, Some(0.0));
    assert_eq!(req.max_tokens, Some(32));
    assert_eq!(req.stream, Some(false));
}

#[test]
fn test_chat_completion_request_serde_roundtrip() {
    let req = mock_request();
    let json = serde_json::to_string(&req).unwrap();
    let parsed: ChatCompletionRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.model, req.model);
    assert_eq!(parsed.max_tokens, req.max_tokens);
}

#[test]
fn test_chat_completion_request_extra_fields() {
    let json_str = r#"{"model":"test","messages":[],"custom_field":42}"#;
    let req: ChatCompletionRequest = serde_json::from_str(json_str).unwrap();
    assert_eq!(req.model, "test");
    assert!(req.extra.contains_key("custom_field"));
    assert_eq!(req.extra.get("custom_field").unwrap().as_u64(), Some(42));
}