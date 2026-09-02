#[derive(Clone, Copy)]
pub enum Client {
    Claude,
    Codex,
}

pub struct LaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub remove_env: Vec<String>,
}

impl LaunchSpec {
    #[cfg(test)]
    fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn launch_spec(client: Client, args: &[String], stoke_url: &str, api_key: &str) -> LaunchSpec {
    let base_url = stoke_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_string();
    match client {
        Client::Claude => LaunchSpec {
            program: "claude".to_string(),
            args: args.to_vec(),
            env: vec![
                ("ANTHROPIC_BASE_URL".to_string(), base_url),
                ("ANTHROPIC_AUTH_TOKEN".to_string(), api_key.to_string()),
            ],
            remove_env: vec!["ANTHROPIC_API_KEY".to_string()],
        },
        Client::Codex => {
            let mut codex_args = vec![
                "-c".to_string(),
                "model_provider=\"stoke\"".to_string(),
                "-c".to_string(),
                "model_providers.stoke.name=\"Stoke\"".to_string(),
                "-c".to_string(),
                format!("model_providers.stoke.base_url=\"{base_url}/v1\""),
                "-c".to_string(),
                "model_providers.stoke.env_key=\"STOKE_API_KEY\"".to_string(),
                "-c".to_string(),
                "model_providers.stoke.wire_api=\"responses\"".to_string(),
                "-c".to_string(),
                "model_providers.stoke.supports_websockets=false".to_string(),
            ];
            codex_args.extend_from_slice(args);
            LaunchSpec {
                program: "codex".to_string(),
                args: codex_args,
                env: vec![("STOKE_API_KEY".to_string(), api_key.to_string())],
                remove_env: Vec::new(),
            }
        }
    }
}

fn select_key(primary: Option<&str>, server_keys: Option<&str>, dev_mode: bool) -> Option<String> {
    primary
        .filter(|key| !key.trim().is_empty())
        .map(|key| key.trim().to_string())
        .or_else(|| {
            server_keys.and_then(|keys| {
                keys.split(',')
                    .map(str::trim)
                    .find(|key| !key.is_empty())
                    .map(str::to_string)
            })
        })
        .or_else(|| dev_mode.then(|| "anonymous".to_string()))
}

pub fn run(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        eprintln!(
            "stoke run — launch an agent through Stoke\n\n\
             Usage:\n  \
               stoke run claude [-- <claude args...>]\n  \
               stoke run codex  [-- <codex args...>]\n\n\
             Environment:\n  \
               STOKE_URL       gateway URL (default: http://127.0.0.1:8787)\n  \
               STOKE_API_KEY   client key; otherwise first value from STOKE_API_KEYS"
        );
        return if args.is_empty() { 2 } else { 0 };
    }

    let client = match args[0].as_str() {
        "claude" => Client::Claude,
        "codex" => Client::Codex,
        other => {
            eprintln!("stoke run: unsupported client '{other}'; use claude or codex");
            return 2;
        }
    };
    let client_args = if args.get(1).map(String::as_str) == Some("--") {
        &args[2..]
    } else {
        &args[1..]
    };
    let primary = std::env::var("STOKE_API_KEY").ok();
    let server_keys = std::env::var("STOKE_API_KEYS").ok();
    let Some(api_key) = select_key(
        primary.as_deref(),
        server_keys.as_deref(),
        std::env::var("STOKE_DEV").ok().as_deref() == Some("1"),
    ) else {
        eprintln!("stoke run: set STOKE_API_KEY (or STOKE_API_KEYS) for gateway authentication");
        return 2;
    };
    let stoke_url =
        std::env::var("STOKE_URL").unwrap_or_else(|_| "http://127.0.0.1:8787".to_string());
    let spec = launch_spec(client, client_args, &stoke_url, &api_key);
    let mut command = std::process::Command::new(&spec.program);
    command.args(&spec.args);
    for (name, value) in &spec.env {
        command.env(name, value);
    }
    for name in &spec.remove_env {
        command.env_remove(name);
    }
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("stoke run: could not launch {}: {error}", spec.program);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_launch_uses_stoke_as_its_anthropic_gateway() {
        let spec = launch_spec(
            Client::Claude,
            &["--print".into(), "hello".into()],
            "http://127.0.0.1:8787/v1/",
            "client-key",
        );

        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, vec!["--print", "hello"]);
        assert_eq!(
            spec.env_value("ANTHROPIC_BASE_URL"),
            Some("http://127.0.0.1:8787")
        );
        assert_eq!(spec.env_value("ANTHROPIC_AUTH_TOKEN"), Some("client-key"));
        assert!(spec.remove_env.contains(&"ANTHROPIC_API_KEY".to_string()));
    }

    #[test]
    fn codex_launch_injects_a_responses_provider_without_putting_the_key_in_argv() {
        let spec = launch_spec(
            Client::Codex,
            &["exec".into(), "hello".into()],
            "http://127.0.0.1:8787/",
            "client-key",
        );

        assert_eq!(spec.program, "codex");
        assert_eq!(spec.args[0..2], ["-c", "model_provider=\"stoke\""]);
        assert!(spec
            .args
            .contains(&"model_providers.stoke.base_url=\"http://127.0.0.1:8787/v1\"".to_string()));
        assert!(spec
            .args
            .contains(&"model_providers.stoke.env_key=\"STOKE_API_KEY\"".to_string()));
        assert!(spec
            .args
            .contains(&"model_providers.stoke.wire_api=\"responses\"".to_string()));
        assert!(spec
            .args
            .contains(&"model_providers.stoke.supports_websockets=false".to_string()));
        assert_eq!(&spec.args[spec.args.len() - 2..], ["exec", "hello"]);
        assert_eq!(spec.env_value("STOKE_API_KEY"), Some("client-key"));
        assert!(!spec.args.iter().any(|arg| arg.contains("client-key")));
    }

    #[test]
    fn client_key_prefers_the_dedicated_value_then_the_first_server_key() {
        assert_eq!(
            select_key(Some("client"), Some("server-a,server-b"), false),
            Some("client".into())
        );
        assert_eq!(
            select_key(None, Some(" server-a, server-b "), false),
            Some("server-a".into())
        );
        assert_eq!(select_key(None, None, true), Some("anonymous".into()));
        assert_eq!(select_key(None, None, false), None);
    }
}
