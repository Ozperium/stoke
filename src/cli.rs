use std::os::unix::process::CommandExt;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        return ExitCode::FAILURE;
    }

    match args[1].as_str() {
        "init" => cli_init(&args[2..]),
        "serve" => cli_serve(&args[2..]),
        "route" => cli_route(&args[2..]),
        "bench" => cli_bench(&args[2..]),
        "models" => cli_models(&args[2..]),
        "pricing" => cli_pricing(&args[2..]),
        "routes" => cli_routes(&args[2..]),
        "version" | "--version" | "-V" => {
            println!("stoke {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "--help" | "-h" | "help" => {
            print_usage();
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "Stoke — the firewall for AI agent spend.\n\
         Runaway-loop detection, rate limits, and local-first routing.\n\n\
         Usage: stoke <command> [options]\n\n\
         Commands:\n  \
           init     Generate stoke.toml config\n  \
           serve    Start the Stoke proxy server\n  \
           route    Send a chat request through Stoke\n  \
           routes   List configured route profiles\n  \
           models   List available models\n  \
           pricing  Show model pricing\n  \
           version  Show version\n\n\
         Quick start:\n  \
           stoke init          # generate config\n  \
           stoke serve          # start proxy on :8787\n  \
           stoke route -m <model> -p 'def add(a, b):'\n\n\
         Environment:\n  \
           STOKE_URL     Proxy URL (default: http://127.0.0.1:8787)\n  \
           STOKE_HOME    Path to Stoke project root\n  \
           STOKE_API_KEYS  Comma-separated API keys for auth\n  \
           STOKE_SEMANTIC_CACHE  Set to enable semantic cache\n\n\
         Full docs: https://github.com/Ozperium/stoke"
    );
}

fn proxy_url() -> String {
    std::env::var("STOKE_URL").unwrap_or_else(|_| "http://127.0.0.1:8787".to_string())
}

fn cli_route(args: &[String]) -> ExitCode {
    let mut model = String::new();
    let mut prompt = String::new();
    let mut routing = String::from("single");
    let mut vote_models = String::new();
    let mut test_code = String::new();
    let mut entry_point = String::new();
    let mut temperature = 0.0f64;
    let mut max_tokens = 8192u32;
    let mut stream = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--model" | "-m" => { i += 1; if i < args.len() { model = args[i].clone(); } }
            "--prompt" | "-p" => { i += 1; if i < args.len() { prompt = args[i].clone(); } }
            "--routing" | "-r" => { i += 1; if i < args.len() { routing = args[i].clone(); } }
            "--vote-models" => { i += 1; if i < args.len() { vote_models = args[i].clone(); } }
            "--test-code" => { i += 1; if i < args.len() { test_code = args[i].clone(); } }
            "--entry-point" => { i += 1; if i < args.len() { entry_point = args[i].clone(); } }
            "--temperature" | "-t" => { i += 1; if i < args.len() { temperature = args[i].parse().unwrap_or(0.0); } }
            "--max-tokens" => { i += 1; if i < args.len() { max_tokens = args[i].parse().unwrap_or(8192); } }
            "--stream" | "-s" => { stream = true; }
            "--help" | "-h" => {
                eprintln!(
                    "stoke route — send a chat request through Stoke\n\n\
                     Options:\n  \
                       --model, -m       Model name (required)\n  \
                       --prompt, -p      Prompt text (required)\n  \
                       --routing, -r     Routing pattern: single, test_vote, cascade_test, self_consistency\n  \
                       --vote-models     Comma-separated models for multi-model patterns\n  \
                       --test-code       Test code for test_vote/cascade_test\n  \
                       --entry-point     Entry point function name for test_vote\n  \
                       --temperature, -t Sampling temperature (default: 0.0)\n  \
                       --max-tokens      Max tokens to generate (default: 8192)\n  \
                       --stream, -s      Stream response (SSE)"
                );
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    if model.is_empty() || prompt.is_empty() {
        eprintln!("Error: --model and --prompt are required");
        return ExitCode::FAILURE;
    }

    let url = format!("{}/v1/chat/completions", proxy_url());

    let mut payload = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": temperature,
        "max_tokens": max_tokens,
        "stream": stream,
    });

    if routing != "single" {
        payload["routing"] = serde_json::Value::String(routing.clone());
    }
    if !vote_models.is_empty() {
        payload["vote_models"] = serde_json::Value::Array(
            vote_models.split(',').map(|s| serde_json::Value::String(s.trim().to_string())).collect()
        );
    }
    if !test_code.is_empty() {
        payload["test_code"] = serde_json::Value::String(test_code);
    }
    if !entry_point.is_empty() {
        payload["entry_point"] = serde_json::Value::String(entry_point);
    }

    if stream {
        // For streaming, pipe SSE via curl with explicit args (no shell)
        let payload_str = payload.to_string();
        let _ = std::process::Command::new("curl")
            .arg("-s")
            .arg("-N")
            .arg(&url)
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(&payload_str)
            .status();
    } else {
        match ureq::post(&url).send_json(payload) {
            Ok(resp) => {
                let body: serde_json::Value = resp.into_json().unwrap_or_default();
                if let Some(content) = body["choices"][0]["message"]["content"].as_str() {
                    println!("{}", content);
                } else {
                    println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
                }
                if let Some(cost) = body.get("stoke_cost") {
                    eprintln!("\n[Stoke] cost: ${:.6}", cost["cost_usd"].as_f64().unwrap_or(0.0));
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}

fn cli_bench(args: &[String]) -> ExitCode {
    // Delegate to the Python benchmark script
    let script_path = std::env::var("STOKE_HOME")
        .map(|h| format!("{}/benchmarks/run_benchmark.py", h))
        .unwrap_or_else(|_| {
            // Try relative to the binary location
            let exe = std::env::current_exe().unwrap_or_default();
            let dir = exe.parent().unwrap_or(std::path::Path::new("."));
            format!("{}/../../benchmarks/run_benchmark.py", dir.display())
        });

    let py_args: Vec<String> = args.iter().filter(|a| a.as_str() != "--help" && a.as_str() != "-h").cloned().collect();

    if args.iter().any(|a| a.as_str() == "--help" || a.as_str() == "-h") {
        eprintln!(
            "stoke bench — run HumanEval benchmark\n\n\
             Delegates to benchmarks/run_benchmark.py. All arguments are forwarded.\n\n\
             Common options:\n  \
               --model         Model to test (required)\n  \
               --routing       Routing pattern (single, test_vote, self_consistency, etc.)\n  \
               --limit         Number of problems (default 20, max 164)\n  \
               --dataset       humaneval or humanevalplus\n  \
               --vote-models   Comma-separated models for multi-model patterns\n  \
               --temperatures  Comma-separated temps for self_consistency\n  \
               --refine-rounds Refine rounds for best_of_n\n  \
               --output        Save results JSON to this path\n\n\
             Environment:\n  \
               STOKE_HOME   Path to Stoke project root (for finding benchmarks/)"
        );
        return ExitCode::SUCCESS;
    }

    let status = std::process::Command::new("python3")
        .arg(&script_path)
        .args(&py_args)
        .status();

    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn cli_models(_args: &[String]) -> ExitCode {
    let url = format!("{}/v1/models", proxy_url());
    match ureq::get(&url).call() {
        Ok(resp) => {
            let body: serde_json::Value = resp.into_json().unwrap_or_default();
            if let Some(data) = body["data"].as_array() {
                for m in data {
                    println!("{}  ({})", m["id"].as_str().unwrap_or("?"), m["provider"].as_str().unwrap_or("?"));
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn cli_pricing(_args: &[String]) -> ExitCode {
    let url = format!("{}/v1/pricing", proxy_url());
    match ureq::get(&url).call() {
        Ok(resp) => {
            let body: serde_json::Value = resp.into_json().unwrap_or_default();
            if let Some(prices) = body["pricing"].as_array() {
                println!("{:<30} {:>12} {:>12} {}", "MODEL", "IN/1M", "OUT/1M", "LOCAL");
                println!("{}", "-".repeat(65));
                for p in prices {
                    let local = p["local"].as_bool().unwrap_or(false);
                    println!(
                        "{:<30} {:>12.4} {:>12.4} {}",
                        p["model"].as_str().unwrap_or("?"),
                        p["input_per_1m"].as_f64().unwrap_or(0.0),
                        p["output_per_1m"].as_f64().unwrap_or(0.0),
                        if local { "yes" } else { "no" }
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn cli_routes(_args: &[String]) -> ExitCode {
    let url = format!("{}/v1/routes", proxy_url());
    match ureq::get(&url).call() {
        Ok(resp) => {
            let body: serde_json::Value = resp.into_json().unwrap_or_default();
            if let Some(routes) = body["routes"].as_array() {
                if routes.is_empty() {
                    println!("No route profiles configured. Use [[routes]] in stoke.toml.");
                    println!("Default endpoint: /v1/chat/completions (auto-route)");
                    return ExitCode::SUCCESS;
                }
                println!("{:<15} {:<30} {:<12} {:<20} {}", "NAME", "PATH", "ROUTING", "MODEL", "BUILTINS");
                println!("{}", "-".repeat(90));
                for r in routes {
                    let builtins = r["builtins"].as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(","))
                        .unwrap_or_default();
                    println!(
                        "{:<15} {:<30} {:<12} {:<20} {}",
                        r["name"].as_str().unwrap_or("?"),
                        r["path"].as_str().unwrap_or("?"),
                        r["routing"].as_str().unwrap_or("?"),
                        r["model"].as_str().unwrap_or("auto"),
                        builtins,
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {} (is the proxy running? try 'stoke serve')", e);
            ExitCode::FAILURE
        }
    }
}

fn cli_init(args: &[String]) -> ExitCode {
    let mut host = "127.0.0.1".to_string();
    let mut port = "8787".to_string();
    let mut ollama_url = "http://127.0.0.1:11434/v1".to_string();
    // No built-in model name: --model wins, else discovered from the user's
    // own Ollama at init time, else left for the user to fill in.
    let mut default_model = String::new();
    let mut output = "stoke.toml".to_string();
    let mut bind_all = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => { i += 1; if i < args.len() { host = args[i].clone(); } }
            "--port" => { i += 1; if i < args.len() { port = args[i].clone(); } }
            "--ollama-url" => { i += 1; if i < args.len() { ollama_url = args[i].clone(); } }
            "--model" | "-m" => { i += 1; if i < args.len() { default_model = args[i].clone(); } }
            "--output" | "-o" => { i += 1; if i < args.len() { output = args[i].clone(); } }
            "--bind-all" | "-b" => { bind_all = true; }
            "--help" | "-h" => {
                eprintln!(
                    "stoke init — generate stoke.toml config\n\n\
                     Options:\n  \
                       --host         Listen address (default: 127.0.0.1)\n  \
                       --port         Listen port (default: 8787)\n  \
                       --bind-all     Bind to 0.0.0.0 (for serving over LAN)\n  \
                       --ollama-url   Ollama API URL (default: http://127.0.0.1:11434/v1)\n  \
                       --model, -m    Default model (default: first model discovered on your Ollama)\n  \
                       --output, -o   Output file (default: stoke.toml)\n\n\
                     Example:\n  \
                       stoke init --bind-all --model <your-model>"
                );
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    if bind_all {
        host = "0.0.0.0".to_string();
    }

    // Discover a default model from the user's own Ollama when not given.
    if default_model.is_empty() {
        let native = ollama_url.trim_end_matches('/').trim_end_matches("/v1").to_string();
        default_model = ureq::get(&format!("{}/api/tags", native))
            .timeout(std::time::Duration::from_secs(2))
            .call()
            .ok()
            .and_then(|r| r.into_json::<serde_json::Value>().ok())
            .and_then(|v| {
                v.get("models")?
                    .as_array()?
                    .first()?
                    .get("name")?
                    .as_str()
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        if default_model.is_empty() {
            eprintln!(
                "⚠ Could not discover a model from {} — set default_model in the \
                 generated config (or rerun with --model).",
                ollama_url
            );
            default_model = "<set-your-model>".to_string();
        } else {
            eprintln!("✓ Discovered default model from your Ollama: {}", default_model);
        }
    }

    let config = format!(
        r#"# Stoke — generated by `stoke init`
# Docs: https://github.com/Ozperium/stoke
# Stoke rejects unauthenticated requests: set STOKE_API_KEYS (or STOKE_DEV=1 for local dev).

# Top-level defaults must sit above the first [section] header.
routing = "single"
default_model = "{default_model}"

[server]
host = "{host}"
port = {port}

# Local Ollama provider
[[providers]]
name = "ollama"
type = "openai_compatible"
base_url = "{ollama_url}"
api_key = "ollama-local"
tier = "local"

# Cloud fallback (uncomment and set API key env var).
# A cloud provider serves nothing until you price its models below: Stoke
# refuses spend it cannot meter, so `budget_usd` is never decorative.
# [[providers]]
# name = "openrouter"
# type = "openai_compatible"
# base_url = "https://openrouter.ai/api/v1"
# api_key_env = "OPENROUTER_API_KEY"
# tier = "cloud"
# models = ["<remote-model>"]

# Prices per 1M tokens, copied from your provider's pricing page. Required for
# every model a non-local provider serves. Local and remote tiers need none —
# your own hardware is not billed per token.
# [pricing.models."<remote-model>"]
# input_per_1m = 3.00
# output_per_1m = 15.00

# Ceilings on how far one request may fan out into billed provider calls, and
# how much output to assume when holding money for a request that names no
# max_tokens (a streamed request is charged only once it ends).
# [limits]
# max_n_samples = 5
# max_vote_models = 5
# allow_caller_routing = false
# assumed_max_output_tokens = 2048

# Built-in plugins (uncomment to enable)
# [builtins.prompt_harness]
# mode = "prepend"
# [builtins.prompt_harness.prompts]
# code = "You are an expert code generator. Write clean, idiomatic code."
# reasoning = "Think step by step. Show your reasoning before the final answer."

# [builtins.pii_redact]
# replacement = "[REDACTED]"

# [builtins.audit_log]
# path = "/tmp/stoke-audit.jsonl"
# log_body = false

# Route profiles (multi-endpoint routing)
# [[routes]]
# name = "code"
# path = "/v1/code/completions"
# routing = "single"
# model = "{default_model}"
# builtins = ["prompt_harness", "pii_redact", "audit_log"]
#
# [[routes]]
# name = "fast"
# path = "/v1/fast/completions"
# routing = "single"
# model = "{default_model}"
#
# [[routes]]
# name = "reasoning"
# path = "/v1/reasoning/completions"
# routing = "single"
# model = "{default_model}"
# builtins = ["prompt_harness"]
"#,
        host = host,
        port = port,
        default_model = default_model,
        ollama_url = ollama_url,
    );

    match std::fs::write(&output, &config) {
        Ok(_) => {
            // Restrict file permissions to owner-only (0600) — config may contain API keys
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o600));
            }
            println!("Generated {} — edit it to add cloud providers, plugins, or routes.", output);
            println!("\nNext steps:");
            println!("  stoke serve     # start the proxy");
            println!("  stoke models     # list available models");
            println!("  stoke route -m {} -p 'Hello!'", default_model);
            println!("\nSecurity: set STOKE_API_KEYS env var for auth, or STOKE_DEV=1 for local dev.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error writing {}: {}", output, e);
            ExitCode::FAILURE
        }
    }
}

fn cli_serve(args: &[String]) -> ExitCode {
    // Check for --help
    if args.iter().any(|a| a.as_str() == "--help" || a.as_str() == "-h") {
        eprintln!(
            "stoke serve — start the Stoke proxy server\n\n\
             Options:\n  \
               --config, -c   Path to config file (default: ./stoke.toml)\n\n\
             The server reads stoke.toml from the current directory,\n\
             or ~/.config/stoke/stoke.toml as fallback.\n\n\
             Environment variables:\n  \
               STOKE_API_KEYS       Comma-separated API keys for auth\n  \
               STOKE_SEMANTIC_CACHE  Set to enable semantic cache\n  \
               OLLAMA_BASE_URL         Ollama URL for embeddings\n  \
               STOKE_EMBED_MODEL    Embedding model (default: nomic-embed-text)"
        );
        return ExitCode::SUCCESS;
    }

    // The server binary is `stoke` (not `stoke-cli`).
    // We exec it with the same args.
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    let server_path = dir.join("stoke");

    if !server_path.exists() {
        eprintln!("Error: server binary 'stoke' not found next to CLI at {}", dir.display());
        eprintln!("Make sure both binaries are installed (cargo install --path . --bins)");
        return ExitCode::FAILURE;
    }

    // Pass --config if specified
    let mut server_args: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--config" || args[i] == "-c" {
            i += 1;
            if i < args.len() {
                // Set env var that Config::load() can read (TODO: add --config to server)
                // For now, we cd to the config's directory
                let config_path = std::path::PathBuf::from(&args[i]);
                if let Some(parent) = config_path.parent() {
                    if parent != std::path::Path::new("") {
                        eprintln!("[stoke] using config: {}", config_path.display());
                        let _ = std::env::set_current_dir(parent);
                    }
                }
            }
        }
        i += 1;
    }

    let err = std::process::Command::new(&server_path).exec();

    eprintln!("Error: {}", err);
    ExitCode::FAILURE
}