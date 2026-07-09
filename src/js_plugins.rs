// JS/TS plugin runtime via deno_core (V8 isolate).
// Feature-gated behind `js-plugins` — not compiled by default.
// Plugins are .ts/.js files that call registerPlugin({ pre_request, prompt_filter, post_response }).
//
// V8 isolates are !Send (Rc internally). We run them on a dedicated OS thread
// and communicate via std::sync::mpsc with oneshot reply channels.

#![cfg(feature = "js-plugins")]

use deno_core::serde_v8;
use deno_core::v8;
use deno_core::JsRuntime;
use deno_core::RuntimeOptions;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::sync::Barrier;
use std::thread;

/// Commands sent to the JS thread. Each carries a reply sender.
enum JsCommand {
    PreRequest {
        model: String,
        routing: String,
        messages: Vec<Value>,
        api_key: String,
        reply: std::sync::mpsc::Sender<String>, // serialized result or error
    },
    PromptFilter {
        messages: Vec<Value>,
        model: String,
        api_key: String,
        reply: std::sync::mpsc::Sender<String>,
    },
    PostResponse {
        model: String,
        response: Value,
        cost_usd: f64,
        elapsed_ms: u64,
        api_key: String,
        reply: std::sync::mpsc::Sender<String>,
    },
    IsEmpty {
        reply: std::sync::mpsc::Sender<bool>,
    },
}

struct JsPlugin {
    name: String,
    runtime: JsRuntime,
}

/// Public API. Send + Sync (the Sender is Send + Sync).
pub struct JsPlugins {
    tx: Sender<JsCommand>,
    handle: Option<thread::JoinHandle<()>>,
}

// Sender<T: Send> is Send + Sync. JsCommand contains only Send types.
unsafe impl Sync for JsPlugins {}

const RUNTIME_JS: &str = r#"
globalThis.__stoke_hooks = { pre_request: [], prompt_filter: [], post_response: [] };

function registerPlugin(hooks) {
  if (hooks.pre_request) globalThis.__stoke_hooks.pre_request.push(hooks.pre_request);
  if (hooks.prompt_filter) globalThis.__stoke_hooks.prompt_filter.push(hooks.prompt_filter);
  if (hooks.post_response) globalThis.__stoke_hooks.post_response.push(hooks.post_response);
}

globalThis.__stoke_run = function(hookType, dataJson) {
  const hooks = globalThis.__stoke_hooks[hookType];
  let data = JSON.parse(dataJson);
  for (const hook of hooks) {
    try {
      const result = hook(data);
      if (result && result.block) {
        return JSON.stringify({ __blocked: true, reason: result.block });
      }
      if (result) {
        data = result;
      }
    } catch (e) {
      console.error('Plugin ' + hookType + ' error: ' + (e.message || e));
    }
  }
  return JSON.stringify(data);
};
"#;

impl JsPlugins {
    pub fn load(paths: &[String]) -> Result<Self, String> {
        let (tx, rx) = channel::<JsCommand>();

        // Pre-read plugin files on the main thread.
        let mut plugin_codes: Vec<(String, String)> = Vec::new();
        for path in paths {
            if path.is_empty() {
                continue;
            }
            let code = std::fs::read_to_string(path)
                .map_err(|e| format!("Failed to read JS plugin {}: {}", path, e))?;
            let name = Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("plugin")
                .to_string();
            plugin_codes.push((name, code));
        }

        // Barrier ensures the thread has finished loading before is_empty() returns.
        let barrier = std::sync::Arc::new(Barrier::new(2));
        let barrier_clone = barrier.clone();

        let handle = thread::spawn(move || {
            let mut plugins: Vec<JsPlugin> = Vec::new();

            for (name, user_code) in &plugin_codes {
                match Self::create_plugin(name, user_code) {
                    Ok(p) => {
                        tracing::info!("Loaded JS plugin: {}", name);
                        plugins.push(p);
                    }
                    Err(e) => {
                        tracing::error!("Failed to load JS plugin {}: {}", name, e);
                    }
                }
            }

            // Signal that loading is complete.
            barrier_clone.wait();

            while let Ok(cmd) = rx.recv() {
                match cmd {
                    JsCommand::IsEmpty { reply } => {
                        let _ = reply.send(plugins.is_empty());
                    }
                    JsCommand::PreRequest { model, routing, messages, api_key, reply } => {
                        let result = Self::handle_pre_request(&mut plugins, &model, &routing, &messages, &api_key);
                        let _ = reply.send(serde_json::to_string(&Self::encode_pre_request(result)).unwrap_or_default());
                    }
                    JsCommand::PromptFilter { messages, model, api_key, reply } => {
                        let result = Self::handle_prompt_filter(&mut plugins, &messages, &model, &api_key);
                        let _ = reply.send(serde_json::to_string(&Self::encode_prompt_filter(result)).unwrap_or_default());
                    }
                    JsCommand::PostResponse { model, response, cost_usd, elapsed_ms, api_key, reply } => {
                        let result = Self::handle_post_response(&mut plugins, &model, &response, cost_usd, elapsed_ms, &api_key);
                        let _ = reply.send(serde_json::to_string(&Self::encode_post_response(result)).unwrap_or_default());
                    }
                }
            }
        });

        // Wait for the JS thread to finish loading plugins before returning.
        barrier.wait();

        Ok(Self { tx, handle: Some(handle) })
    }

    fn create_plugin(name: &str, user_code: &str) -> Result<JsPlugin, String> {
        let mut runtime = JsRuntime::new(RuntimeOptions { ..Default::default() });

        runtime
            .execute_script("runtime.js", RUNTIME_JS.to_string())
            .map_err(|e| format!("Failed to init JS runtime: {}", e))?;

        let wrapper = format!(
            r#"
(function() {{
  const module = {{ exports: {{}} }};
  {}
  if (module.exports && Object.keys(module.exports).length > 0) {{
    registerPlugin(module.exports);
  }}
}})();
"#,
            user_code
        );

        runtime
            .execute_script("plugin.js", wrapper)
            .map_err(|e| format!("Failed to load JS plugin {}: {}", name, e))?;

        Ok(JsPlugin { name: name.to_string(), runtime })
    }

    fn run_hook(plugin: &mut JsPlugin, hook_type: &str, input: &Value) -> Result<String, String> {
        let input_json = serde_json::to_string(input)
            .map_err(|e| format!("Failed to serialize plugin input: {}", e))?;

        let escaped_json = input_json.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', "\\n");
        let code = format!("globalThis.__stoke_run('{}', '{}');", hook_type, escaped_json);

        let global = plugin
            .runtime
            .execute_script("hook.js", code)
            .map_err(|e| format!("JS plugin {} execution failed: {}", plugin.name, e))?;

        let context = plugin.runtime.main_context();
        let mut scope = plugin.runtime.handle_scope();
        let context_local = v8::Local::new(&mut scope, context);
        let mut cs = v8::ContextScope::new(&mut scope, context_local);
        let local = v8::Local::new(&mut cs, global);
        let local_str = local
            .to_string(&mut cs)
            .ok_or_else(|| format!("JS plugin {} returned non-string", plugin.name))?;
        Ok(local_str.to_rust_string_lossy(&mut cs))
    }

    // --- Internal handlers (run on JS thread) ---

    fn handle_pre_request(plugins: &mut [JsPlugin], model: &str, routing: &str, messages: &[Value], api_key: &str) -> Result<(Option<String>, Option<String>, Option<Vec<String>>, Option<String>), String> {
        let input = json!({ "model": model, "routing": routing, "messages": messages, "api_key": api_key });
        for plugin in plugins {
            let result_str = Self::run_hook(plugin, "pre_request", &input)?;
            let result: Value = serde_json::from_str(&result_str)
                .map_err(|e| format!("JS plugin returned invalid JSON: {}", e))?;

            if let Some(block) = result.get("__blocked").and_then(|v| v.as_str()) {
                return Ok((None, None, None, Some(block.to_string())));
            }

            return Ok((
                result.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()),
                result.get("routing").and_then(|v| v.as_str()).map(|s| s.to_string()),
                result.get("vote_models").and_then(|v| v.as_array()).map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()),
                result.get("block").and_then(|v| v.as_str()).map(|s| s.to_string()),
            ));
        }
        Ok((None, None, None, None))
    }

    fn handle_prompt_filter(plugins: &mut [JsPlugin], messages: &[Value], model: &str, api_key: &str) -> Result<(Option<Vec<Value>>, Option<String>), String> {
        let input = json!({ "messages": messages, "model": model, "api_key": api_key });
        let mut current_messages = None;

        for plugin in plugins {
            let result_str = Self::run_hook(plugin, "prompt_filter", &input)?;
            let result: Value = serde_json::from_str(&result_str)
                .map_err(|e| format!("JS plugin returned invalid JSON: {}", e))?;

            if let Some(block) = result.get("__blocked").and_then(|v| v.as_str()) {
                return Ok((current_messages, Some(block.to_string())));
            }
            if let Some(msgs) = result.get("messages").and_then(|v| v.as_array()) {
                current_messages = Some(msgs.clone());
            }
            if let Some(block) = result.get("block").and_then(|v| v.as_str()) {
                return Ok((current_messages, Some(block.to_string())));
            }
        }
        Ok((current_messages, None))
    }

    fn handle_post_response(plugins: &mut [JsPlugin], model: &str, response: &Value, cost_usd: f64, elapsed_ms: u64, api_key: &str) -> Result<Option<Value>, String> {
        let input = json!({ "model": model, "response": response, "cost_usd": cost_usd, "elapsed_ms": elapsed_ms, "api_key": api_key });
        let mut current = None;

        for plugin in plugins {
            let result_str = Self::run_hook(plugin, "post_response", &input)?;
            let result: Value = serde_json::from_str(&result_str)
                .map_err(|e| format!("JS plugin returned invalid JSON: {}", e))?;

            if let Some(resp) = result.get("response") {
                current = Some(resp.clone());
            }
        }
        Ok(current)
    }

    // --- Encoders for channel serialization ---

    fn encode_pre_request(r: Result<(Option<String>, Option<String>, Option<Vec<String>>, Option<String>), String>) -> Value {
        match r {
            Ok((model, routing, vote_models, block)) => json!({
                "ok": true,
                "model": model,
                "routing": routing,
                "vote_models": vote_models,
                "block": block,
            }),
            Err(e) => json!({ "ok": false, "error": e }),
        }
    }

    fn encode_prompt_filter(r: Result<(Option<Vec<Value>>, Option<String>), String>) -> Value {
        match r {
            Ok((messages, block)) => json!({
                "ok": true,
                "messages": messages,
                "block": block,
            }),
            Err(e) => json!({ "ok": false, "error": e }),
        }
    }

    fn encode_post_response(r: Result<Option<Value>, String>) -> Value {
        match r {
            Ok(response) => json!({ "ok": true, "response": response }),
            Err(e) => json!({ "ok": false, "error": e }),
        }
    }

    // --- Public API (Send + Sync, communicates via channels) ---

    pub fn is_empty(&self) -> bool {
        let (tx, rx) = channel();
        if self.tx.send(JsCommand::IsEmpty { reply: tx }).is_err() {
            return true;
        }
        rx.recv().unwrap_or(true)
    }

    pub fn pre_request(&self, model: &str, routing: &str, messages: &[Value], api_key: &str) -> Result<(Option<String>, Option<String>, Option<Vec<String>>, Option<String>), String> {
        let (tx, rx) = channel();
        self.tx.send(JsCommand::PreRequest {
            model: model.to_string(),
            routing: routing.to_string(),
            messages: messages.to_vec(),
            api_key: api_key.to_string(),
            reply: tx,
        }).map_err(|e| format!("JS thread channel error: {}", e))?;

        let result_str = rx.recv().map_err(|e| format!("JS thread reply error: {}", e))?;
        let result: Value = serde_json::from_str(&result_str).map_err(|e| format!("Invalid reply: {}", e))?;

        if result.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            return Err(result.get("error").and_then(|v| v.as_str()).unwrap_or("unknown").to_string());
        }

        Ok((
            result.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()),
            result.get("routing").and_then(|v| v.as_str()).map(|s| s.to_string()),
            result.get("vote_models").and_then(|v| v.as_array()).map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()),
            result.get("block").and_then(|v| v.as_str()).map(|s| s.to_string()),
        ))
    }

    pub fn prompt_filter(&self, messages: &[Value], model: &str, api_key: &str) -> Result<(Option<Vec<Value>>, Option<String>), String> {
        let (tx, rx) = channel();
        self.tx.send(JsCommand::PromptFilter {
            messages: messages.to_vec(),
            model: model.to_string(),
            api_key: api_key.to_string(),
            reply: tx,
        }).map_err(|e| format!("JS thread channel error: {}", e))?;

        let result_str = rx.recv().map_err(|e| format!("JS thread reply error: {}", e))?;
        let result: Value = serde_json::from_str(&result_str).map_err(|e| format!("Invalid reply: {}", e))?;

        if result.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            return Err(result.get("error").and_then(|v| v.as_str()).unwrap_or("unknown").to_string());
        }

        Ok((
            result.get("messages").and_then(|v| v.as_array()).map(|a| a.clone()),
            result.get("block").and_then(|v| v.as_str()).map(|s| s.to_string()),
        ))
    }

    pub fn post_response(&self, model: &str, response: &Value, cost_usd: f64, elapsed_ms: u64, api_key: &str) -> Result<Option<Value>, String> {
        let (tx, rx) = channel();
        self.tx.send(JsCommand::PostResponse {
            model: model.to_string(),
            response: response.clone(),
            cost_usd,
            elapsed_ms,
            api_key: api_key.to_string(),
            reply: tx,
        }).map_err(|e| format!("JS thread channel error: {}", e))?;

        let result_str = rx.recv().map_err(|e| format!("JS thread reply error: {}", e))?;
        let result: Value = serde_json::from_str(&result_str).map_err(|e| format!("Invalid reply: {}", e))?;

        if result.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            return Err(result.get("error").and_then(|v| v.as_str()).unwrap_or("unknown").to_string());
        }

        Ok(result.get("response").cloned())
    }
}

impl Drop for JsPlugins {
    fn drop(&mut self) {
        drop(self.handle.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_empty() {
        let plugins = JsPlugins::load(&[]);
        assert!(plugins.is_ok());
        assert!(plugins.unwrap().is_empty());
    }
}