//! Anthropic Messages -> OpenAI chat/completions translation.
//!
//! Lets regular OpenAI-compatible local providers (tier local/remote, e.g.
//! Ollama) serve `/v1/messages` clients such as Claude Code. The request is
//! translated Anthropic -> OpenAI, dispatched to `{base_url}/chat/completions`,
//! and the response (non-streaming or SSE) is translated back so the client
//! sees a normal Anthropic Messages payload.
//!
//! Fail-closed by design: anything this slice cannot represent faithfully —
//! images, thinking, server tools — is rejected with a clear 400 naming
//! the unsupported feature, never silently mangled. Tool use IS supported:
//! Anthropic tool definitions, tool_use and tool_result blocks translate to
//! OpenAI function calling in both directions.

use serde_json::{json, Value};

/// Translate an Anthropic Messages request body into an OpenAI
/// chat/completions request body.
///
/// Supported: `model` passthrough, `system` (string or text-block array),
/// `messages` with string or block content (text, tool_use, tool_result),
/// `tools` and `tool_choice` (function-calling translation), `max_tokens`,
/// `temperature`, `top_p`, `stop_sequences` -> `stop`, `stream`. Anything else
/// structural (thinking, images, server tools) is a hard error.
pub fn translate_request(req: &Value) -> Result<Value, String> {
    for key in ["thinking", "server_tool_use", "web_search"] {
        if req.get(key).map(|v| !v.is_null()).unwrap_or(false) {
            return Err(format!(
                "unsupported request feature '{key}': the /v1/messages OpenAI translation \
                 for local providers does not support it yet; use an Anthropic-type provider"
            ));
        }
    }

    let model = req.get("model").and_then(Value::as_str).unwrap_or("");
    let mut messages: Vec<Value> = Vec::new();

    if let Some(system) = req.get("system").filter(|v| !v.is_null()) {
        let text = content_text(system)?;
        if !text.is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }

    if let Some(msgs) = req.get("messages").and_then(Value::as_array) {
        for m in msgs {
            translate_message(m, &mut messages)?;
        }
    }

    let mut out = json!({ "model": model, "messages": messages });
    if let Some(tools) = req.get("tools").filter(|v| !v.is_null()) {
        out["tools"] = translate_tools(tools)?;
    }
    if let Some(tc) = req.get("tool_choice").filter(|v| !v.is_null()) {
        out["tool_choice"] = translate_tool_choice(tc)?;
    }
    if let Some(mt) = req.get("max_tokens").and_then(Value::as_u64) {
        out["max_tokens"] = json!(mt);
    }
    if let Some(t) = req.get("temperature").and_then(Value::as_f64) {
        out["temperature"] = json!(t);
    }
    if let Some(t) = req.get("top_p").and_then(Value::as_f64) {
        out["top_p"] = json!(t);
    }
    if let Some(stops) = req.get("stop_sequences").and_then(Value::as_array) {
        out["stop"] = json!(stops);
    }
    if req.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        out["stream"] = json!(true);
    }
    Ok(out)
}

/// Flatten Anthropic content (a string or an array of typed blocks) into plain
/// text. Only text is representable in this slice; any other block type is a
/// fail-closed error that names the type.
fn content_text(content: &Value) -> Result<String, String> {
    match content {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        Value::Array(blocks) => {
            let mut parts: Vec<String> = Vec::new();
            for b in blocks {
                let ty = b.get("type").and_then(Value::as_str).unwrap_or("missing");
                if ty != "text" {
                    return Err(format!(
                        "unsupported content block type '{ty}': the /v1/messages OpenAI \
                         translation for local providers only supports text blocks"
                    ));
                }
                parts.push(
                    b.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                );
            }
            Ok(parts.join("\n"))
        }
        _ => Err(
            "unsupported content value: /v1/messages OpenAI translation expects a string \
             or an array of content blocks"
                .to_string(),
        ),
    }
}

/// Translate one Anthropic message into zero or more OpenAI messages.
///
/// - text content -> a single message with flattened text
/// - tool_use blocks (assistant) -> `tool_calls` alongside any text content
/// - tool_result blocks (user) -> one `role:"tool"` message per result,
///   interleaved in order with any text content
fn translate_message(m: &Value, out: &mut Vec<Value>) -> Result<(), String> {
    let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
    let content = m.get("content").unwrap_or(&Value::Null);

    match content {
        Value::String(_) | Value::Null => {
            let text = content_text(content)?;
            out.push(json!({ "role": role, "content": text }));
        }
        Value::Array(blocks) => {
            // Fast path: all-text blocks keep the previous single-message shape.
            if blocks
                .iter()
                .all(|b| b.get("type").and_then(Value::as_str).unwrap_or("missing") == "text")
            {
                let text = content_text(content)?;
                out.push(json!({ "role": role, "content": text }));
                return Ok(());
            }
            let mut pending_text: Vec<String> = Vec::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str).unwrap_or("missing") {
                    "text" => pending_text.push(
                        b.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ),
                    "tool_use" => {
                        flush_text(role, &mut pending_text, out);
                        let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                        let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
                        out.push(json!({
                            "role": role,
                            "content": Value::Null,
                            "tool_calls": [{
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(&input)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                },
                            }],
                        }));
                    }
                    "tool_result" => {
                        flush_text(role, &mut pending_text, out);
                        let tool_use_id =
                            b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                        let result_text = content_text(b.get("content").unwrap_or(&Value::Null))?;
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": result_text,
                        }));
                    }
                    ty => {
                        return Err(format!(
                            "unsupported content block type '{ty}': the /v1/messages OpenAI \
                             translation for local providers supports text, tool_use and \
                             tool_result blocks"
                        ));
                    }
                }
            }
            flush_text(role, &mut pending_text, out);
        }
        _ => {
            let text = content_text(content)?;
            out.push(json!({ "role": role, "content": text }));
        }
    }
    Ok(())
}

/// Emit accumulated text as one message, keeping text and tool blocks in
/// their original order relative to each other.
fn flush_text(role: &str, pending: &mut Vec<String>, out: &mut Vec<Value>) {
    if pending.is_empty() {
        return;
    }
    let text = std::mem::take(pending).join("\n");
    out.push(json!({ "role": role, "content": text }));
}

/// Translate Anthropic `tools` into OpenAI function definitions.
fn translate_tools(tools: &Value) -> Result<Value, String> {
    let list = tools.as_array().ok_or_else(|| {
        "unsupported 'tools' value: expected an array of tool definitions".to_string()
    })?;
    let mut out = Vec::with_capacity(list.len());
    for t in list {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("");
        let ty = t.get("type").and_then(Value::as_str).unwrap_or("custom");
        // Fail closed on server-side tool types (web_search, bash, etc.):
        // they have no local function-call equivalent.
        if ty != "custom" {
            return Err(format!(
                "unsupported tool type '{ty}': the /v1/messages OpenAI translation for \
                 local providers only supports custom tools"
            ));
        }
        out.push(json!({
            "type": "function",
            "function": {
                "name": name,
                "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({ "type": "object" })),
            },
        }));
    }
    Ok(Value::Array(out))
}

/// Translate Anthropic `tool_choice` into OpenAI `tool_choice`.
fn translate_tool_choice(tc: &Value) -> Result<Value, String> {
    match tc.get("type").and_then(Value::as_str) {
        Some("auto") => Ok(json!("auto")),
        Some("any") => Ok(json!("required")),
        Some("tool") => {
            let name = tc.get("name").and_then(Value::as_str).unwrap_or("");
            Ok(json!({ "type": "function", "function": { "name": name } }))
        }
        Some(other) => Err(format!(
            "unsupported tool_choice type '{other}': the /v1/messages OpenAI translation \
             for local providers supports auto, any and named tool choices"
        )),
        None => Err("unsupported tool_choice: expected an object with a 'type' field".to_string()),
    }
}

/// Map an OpenAI `finish_reason` to an Anthropic `stop_reason`.
pub fn map_stop_reason(finish_reason: &str) -> &'static str {
    match finish_reason {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        _ => "end_turn",
    }
}

/// Build Anthropic content blocks from an OpenAI choice: one text block when
/// the message carries content, plus one `tool_use` block per tool call with
/// the JSON-stringified `arguments` parsed back into an input object.
fn response_tool_blocks(choice: &Value, text: &str) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    if let Some(calls) = choice
        .pointer("/message/tool_calls")
        .and_then(Value::as_array)
    {
        for call in calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let input = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
            blocks.push(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }));
        }
    }
    blocks
}

/// Translate a non-streaming OpenAI chat/completions response into an
/// Anthropic Messages response body.
pub fn translate_response(openai: &Value, model: &str) -> Value {
    let id = openai
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg_stoke");
    let choice = openai
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first());
    let (content, finish) = choice
        .map(|c| {
            let text = c
                .pointer("/message/content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let blocks = response_tool_blocks(c, &text);
            (
                blocks,
                c["finish_reason"].as_str().unwrap_or("stop").to_string(),
            )
        })
        .unwrap_or_else(|| (Vec::new(), "stop".to_string()));
    let (input, output) = openai
        .get("usage")
        .map(|u| {
            (
                u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                u.get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": map_stop_reason(&finish),
        "stop_sequence": Value::Null,
        "usage": { "input_tokens": input, "output_tokens": output },
    })
}

/// Incrementally translate an OpenAI SSE byte stream into Anthropic SSE events.
///
/// `feed_bytes` accepts raw upstream chunks (SSE lines may be split across
/// chunk boundaries) and returns any complete Anthropic SSE events the fed
/// bytes produced, framed exactly as Anthropic documents them:
/// `event: <name>\ndata: <json>\n\n`. The `[DONE]` sentinel flushes the
/// closing `content_block_stop`, `message_delta`, and `message_stop` events.
///
/// Usage: OpenAI reports tokens on the final frame, after `message_start` has
/// already gone out, so `message_start` carries input 0 and the final
/// `message_delta` reports both observed token counts.
pub struct StreamTranslator {
    model: String,
    id: String,
    started: bool,
    /// Anthropic block index currently open (`content_block_start` sent,
    /// `content_block_stop` not yet sent). OpenAI tool calls may interleave
    /// with text deltas, so multiple blocks can open and close per message.
    block_open: Option<usize>,
    /// Next Anthropic block index to hand out. Anthropic indexes every
    /// content block 0..n across the whole message; OpenAI indexes tool
    /// calls separately from text, so every block (text or tool) is
    /// allocated from this one monotonic counter.
    next_index: usize,
    /// Anthropic block index per OpenAI tool-call index, so continuation
    /// fragments append to the right block instead of opening a duplicate.
    tool_blocks: Vec<Option<usize>>,
    finished: bool,
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: Option<String>,
    partial: Vec<u8>,
}

impl StreamTranslator {
    pub fn new(model: impl Into<String>) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self {
            model: model.into(),
            id: format!("msg_stoke_{nanos}"),
            started: false,
            block_open: None,
            next_index: 0,
            tool_blocks: Vec::new(),
            finished: false,
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            partial: Vec::new(),
        }
    }

    /// Feed raw upstream bytes; returns complete Anthropic SSE event frames.
    ///
    /// Buffers raw bytes and only converts complete lines to UTF-8, so a
    /// multi-byte character split across chunk boundaries is not corrupted
    /// into U+FFFD replacement garbage mid-stream.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Vec<String> {
        self.partial.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(pos) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=pos).collect();
            let Ok(line) = std::str::from_utf8(&line) else {
                // A newline always terminates an SSE frame, so an invalid
                // line cannot be part of a split character — skip it.
                continue;
            };
            if let Some(payload) = line.trim_end().strip_prefix("data:") {
                let payload = payload.trim();
                if payload == "[DONE]" {
                    events.extend(self.finish());
                } else if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    events.extend(self.on_chunk(&v));
                }
            }
        }
        events
    }

    fn on_chunk(&mut self, v: &Value) -> Vec<String> {
        let mut events = self.ensure_started();
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            if let Some(p) = u.get("prompt_tokens").and_then(Value::as_u64) {
                self.input_tokens = p;
            }
            if let Some(c) = u.get("completion_tokens").and_then(Value::as_u64) {
                self.output_tokens = c;
            }
        }
        if let Some(choice) = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        {
            if let Some(text) = choice.pointer("/delta/content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events.extend(self.text_delta(text));
                }
            }
            if let Some(calls) = choice
                .pointer("/delta/tool_calls")
                .and_then(Value::as_array)
            {
                for call in calls {
                    events.extend(self.tool_call_delta(call));
                }
            }
            if let Some(fr) = choice
                .get("finish_reason")
                .filter(|f| !f.is_null())
                .and_then(Value::as_str)
            {
                self.stop_reason = Some(map_stop_reason(fr).to_string());
            }
        }
        events
    }

    /// Emit a text delta, opening a text block first if none is open. A
    /// text delta arriving while a tool block is open closes that block and
    /// opens a fresh text block — a `tool_use` block must never receive
    /// `text_delta` events.
    fn text_delta(&mut self, text: &str) -> Vec<String> {
        let mut events = Vec::new();
        if let Some(open) = self.block_open {
            // A tool block is open: close it before starting text. Text
            // blocks reuse their own block while open (checked below), so
            // only a tool-opened block gets here.
            if self.tool_blocks.iter().any(|&b| b == Some(open)) {
                events.push(self.event(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": open }),
                ));
                self.block_open = None;
            }
        }
        let index = match self.block_open {
            Some(i) => i,
            None => {
                let i = self.next_index;
                self.next_index += 1;
                self.block_open = Some(i);
                events.push(self.event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": i,
                        "content_block": { "type": "text", "text": "" },
                    }),
                ));
                i
            }
        };
        events.push(self.event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": index,
                "delta": { "type": "text_delta", "text": text },
            }),
        ));
        events
    }

    /// Emit Anthropic tool_use block events from one OpenAI incremental
    /// tool-call delta. Tool calls interleave with text deltas, so an
    /// arriving tool fragment closes any open text block; deltas for the
    /// same OpenAI `index` keep appending to one Anthropic block, and a new
    /// `index` (or one we have not seen) opens a new `tool_use` block with
    /// `input_json_delta` partial-JSON fragments.
    fn tool_call_delta(&mut self, call: &Value) -> Vec<String> {
        let oa_index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
        let mut events = Vec::new();
        // Every block (text or tool) is allocated from one monotonic
        // counter, so parallel tool calls never collide or reuse indexes.
        if self.tool_blocks.len() <= oa_index as usize {
            self.tool_blocks.resize(oa_index as usize + 1, None);
        }
        let index = match self.tool_blocks[oa_index as usize] {
            Some(index) => index,
            None => {
                let index = self.next_index;
                self.next_index += 1;
                self.tool_blocks[oa_index as usize] = Some(index);
                // Opening a new tool block closes whichever block is open —
                // text or an earlier tool — since Anthropic blocks cannot
                // receive events after their stop.
                if let Some(open) = self.block_open {
                    if open != index {
                        events.push(self.event(
                            "content_block_stop",
                            json!({ "type": "content_block_stop", "index": open }),
                        ));
                    }
                }
                self.block_open = Some(index);
                let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                events.push(self.event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": {},
                        },
                    }),
                ));
                index
            }
        };
        // Anthropic blocks are strictly sequential: once a block is closed,
        // it can never be reopened. When a fragment arrives for a tool block
        // that already stopped (parallel calls interleaving across block
        // boundaries), keep the stream legal by appending to the block that
        // is still open — never to a stopped index.
        let index = if self.block_open == Some(index) {
            index
        } else {
            self.block_open.unwrap_or(index)
        };
        if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
            if !args.is_empty() {
                events.push(self.event(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": args },
                    }),
                ));
            }
        }
        events
    }

    fn ensure_started(&mut self) -> Vec<String> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        vec![self.event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": Value::Null,
                    "stop_sequence": Value::Null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                },
            }),
        )]
    }

    fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = self.ensure_started();
        if let Some(open) = self.block_open {
            events.push(self.event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": open }),
            ));
            self.block_open = None;
        }
        events.push(self.event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": self
                        .stop_reason
                        .clone()
                        .unwrap_or_else(|| "end_turn".to_string()),
                    "stop_sequence": Value::Null,
                },
                "usage": {
                    "output_tokens": self.output_tokens,
                    "input_tokens": self.input_tokens,
                },
            }),
        ));
        events.push(self.event("message_stop", json!({ "type": "message_stop" })));
        events
    }

    /// Frame one Anthropic SSE event exactly as Anthropic documents it.
    fn event(&self, name: &str, data: Value) -> String {
        format!("event: {}\ndata: {}\n\n", name, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn events_names(events: &[String]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| {
                e.strip_prefix("event: ")?
                    .lines()
                    .next()
                    .map(str::to_string)
            })
            .collect()
    }

    fn event_data(event: &str) -> Value {
        let data = event
            .lines()
            .find(|l| l.starts_with("data: "))
            .expect("event frame carries a data line");
        serde_json::from_str(&data["data: ".len()..]).expect("data line is valid JSON")
    }

    #[test]
    fn system_string_becomes_a_system_message() {
        let req = json!({
            "model": "llama3",
            "system": "you are terse",
            "max_tokens": 128,
            "messages": [{ "role": "user", "content": "hello" }]
        });
        let out = translate_request(&req).unwrap();
        assert_eq!(out["model"], "llama3");
        assert_eq!(out["max_tokens"], 128);
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][0]["content"], "you are terse");
        assert_eq!(out["messages"][1]["content"], "hello");
        assert!(out.get("stop").is_none());
        assert!(out.get("temperature").is_none());
    }

    #[test]
    fn system_blocks_join_into_one_system_message() {
        let req = json!({
            "model": "llama3",
            "system": [
                { "type": "text", "text": "rule one" },
                { "type": "text", "text": "rule two" }
            ],
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let out = translate_request(&req).unwrap();
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][0]["content"], "rule one\nrule two");
    }

    #[test]
    fn multi_turn_messages_keep_their_roles_and_order() {
        let req = json!({
            "model": "llama3",
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "part a" },
                    { "type": "text", "text": "part b" }
                ]},
                { "role": "assistant", "content": "answer" },
                { "role": "user", "content": "again" }
            ]
        });
        let out = translate_request(&req).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "part a\npart b");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "answer");
        assert_eq!(msgs[2]["content"], "again");
    }

    #[test]
    fn sampling_params_map_including_stop_sequences() {
        let req = json!({
            "model": "llama3",
            "max_tokens": 64,
            "temperature": 0.2,
            "top_p": 0.9,
            "stop_sequences": ["END", "STOP"],
            "messages": [{ "role": "user", "content": "x" }]
        });
        let out = translate_request(&req).unwrap();
        assert_eq!(out["temperature"], 0.2);
        assert_eq!(out["top_p"], 0.9);
        assert_eq!(out["stop"], json!(["END", "STOP"]));
        assert_eq!(out["max_tokens"], 64);
    }

    #[test]
    fn streaming_flag_passes_through() {
        let req = json!({
            "model": "llama3",
            "stream": true,
            "messages": [{ "role": "user", "content": "x" }]
        });
        assert_eq!(translate_request(&req).unwrap()["stream"], true);
    }

    #[test]
    fn tool_definitions_translate_to_openai_functions() {
        let req = json!({
            "model": "llama3",
            "tools": [{
                "name": "read",
                "description": "read a file",
                "input_schema": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }
            }],
            "messages": [{ "role": "user", "content": "x" }]
        });
        let out = translate_request(&req).unwrap();
        let tools = out["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "read");
        assert_eq!(tools[0]["function"]["description"], "read a file");
        assert_eq!(tools[0]["function"]["parameters"]["type"], "object");
        assert_eq!(tools[0]["function"]["parameters"]["required"][0], "path");
        assert!(out.get("tool_choice").is_none());
    }

    #[test]
    fn server_side_tool_types_are_rejected_with_a_clear_error() {
        let req = json!({
            "model": "llama3",
            "tools": [{ "type": "web_search_20250305", "name": "web_search" }],
            "messages": [{ "role": "user", "content": "x" }]
        });
        let err = translate_request(&req).unwrap_err();
        assert!(
            err.contains("web_search_20250305"),
            "error names the type: {err}"
        );
    }

    #[test]
    fn tool_choice_variants_map_to_openai_tool_choice() {
        let base = json!({
            "model": "llama3",
            "tools": [{ "name": "read", "input_schema": {} }],
            "messages": [{ "role": "user", "content": "x" }]
        });
        let mut auto = base.clone();
        auto["tool_choice"] = json!({ "type": "auto" });
        assert_eq!(translate_request(&auto).unwrap()["tool_choice"], "auto");

        let mut any = base.clone();
        any["tool_choice"] = json!({ "type": "any" });
        assert_eq!(translate_request(&any).unwrap()["tool_choice"], "required");

        let mut named = base.clone();
        named["tool_choice"] = json!({ "type": "tool", "name": "read" });
        let out = translate_request(&named).unwrap();
        assert_eq!(out["tool_choice"]["type"], "function");
        assert_eq!(out["tool_choice"]["function"]["name"], "read");
    }

    #[test]
    fn assistant_tool_use_block_becomes_a_tool_calls_message() {
        let req = json!({
            "model": "llama3",
            "messages": [
                { "role": "user", "content": "read the file" },
                { "role": "assistant", "content": [
                    { "type": "text", "text": "reading now" },
                    { "type": "tool_use", "id": "toolu_1", "name": "read",
                      "input": { "path": "/tmp/x" } }
                ]}
            ]
        });
        let out = translate_request(&req).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "reading now");
        let calls = msgs[2]["tool_calls"].as_array().unwrap();
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(calls[0]["id"], "toolu_1");
        assert_eq!(calls[0]["type"], "function");
        assert_eq!(calls[0]["function"]["name"], "read");
        // input object -> JSON-stringified arguments
        let args: Value =
            serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args, json!({ "path": "/tmp/x" }));
    }

    #[test]
    fn user_tool_result_block_becomes_a_role_tool_message() {
        let req = json!({
            "model": "llama3",
            "messages": [
                { "role": "user", "content": "read the file" },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "toolu_1", "name": "read", "input": {} }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1",
                      "content": "file contents here" }
                ]}
            ]
        });
        let out = translate_request(&req).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "toolu_1");
        assert_eq!(msgs[2]["content"], "file contents here");
    }

    #[test]
    fn tool_use_arguments_round_trip_through_the_response() {
        let openai = json!({
            "id": "c4",
            "choices": [{
                "message": {
                    "content": "let me check",
                    "tool_calls": [{
                        "id": "call_9",
                        "type": "function",
                        "function": { "name": "read",
                                      "arguments": "{\"path\":\"/tmp/x\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 9 }
        });
        let out = translate_response(&openai, "llama3");
        assert_eq!(out["stop_reason"], "tool_use");
        let content = out["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "let me check");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "call_9");
        assert_eq!(content[1]["name"], "read");
        // JSON-stringified arguments -> parsed input object
        assert_eq!(content[1]["input"], json!({ "path": "/tmp/x" }));

        // Round-trip: feed the tool_use block back through request translation.
        let req = json!({
            "model": "llama3",
            "messages": [
                { "role": "assistant", "content": [content[1].clone()] }
            ]
        });
        let rt = translate_request(&req).unwrap();
        let call = &rt["messages"][0]["tool_calls"][0];
        assert_eq!(call["function"]["name"], "read");
        let args: Value =
            serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args, json!({ "path": "/tmp/x" }));
    }

    #[test]
    fn stream_text_then_tool_call_produces_the_correct_block_sequence() {
        let mut t = StreamTranslator::new("llama3");
        let mut events = Vec::new();
        events.extend(
            t.feed_bytes(b"data: {\"choices\":[{\"delta\":{\"content\":\"Let me look.\"}}]}\n\n"),
        );
        // Open a tool call: first delta carries id + name, then argument
        // fragments arrive incrementally.
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]}}]}\n\n",
        ));
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
        ));
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"/tmp/x\\\"}\"}}]}}]}\n\n",
        ));
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":12}}\n\n",
        ));
        events.extend(t.feed_bytes(b"data: [DONE]\n\n"));

        let names = events_names(&events);
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start", // text
                "content_block_delta", // text
                "content_block_stop",  // text closed when tool starts
                "content_block_start", // tool_use
                "content_block_delta", // input_json_delta partial
                "content_block_delta", // input_json_delta rest
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );

        let tool_start = event_data(&events[4]);
        assert_eq!(tool_start["index"], 1);
        assert_eq!(tool_start["content_block"]["type"], "tool_use");
        assert_eq!(tool_start["content_block"]["id"], "call_1");
        assert_eq!(tool_start["content_block"]["name"], "read");
        assert_eq!(tool_start["content_block"]["input"], json!({}));

        // Partial JSON fragments concatenate to the full arguments string.
        let mut partials = String::new();
        for e in &events[5..7] {
            let d = event_data(e);
            assert_eq!(d["delta"]["type"], "input_json_delta");
            partials.push_str(d["delta"]["partial_json"].as_str().unwrap_or(""));
        }
        let args: Value = serde_json::from_str(&partials).expect("fragments form valid JSON");
        assert_eq!(args, json!({ "path": "/tmp/x" }));

        let delta_msg = event_data(&events[8]);
        assert_eq!(delta_msg["delta"]["stop_reason"], "tool_use");
        assert_eq!(delta_msg["usage"]["output_tokens"], 12);
    }

    #[test]
    fn stream_tool_only_message_skips_the_text_block() {
        let mut t = StreamTranslator::new("m");
        let mut events = Vec::new();
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"ls\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        ));
        events.extend(t.feed_bytes(b"data: [DONE]\n\n"));
        let names = events_names(&events);
        assert_eq!(names[0], "message_start");
        let start = event_data(&events[1]);
        assert_eq!(start["content_block"]["type"], "tool_use");
        assert_eq!(start["index"], 0);
        assert_eq!(*names.last().unwrap(), "message_stop");
        let delta_msg = event_data(&events[events.len() - 2]);
        assert_eq!(delta_msg["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn unsupported_block_type_is_rejected_and_named() {
        let req = json!({
            "model": "llama3",
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "look" },
                    { "type": "image", "source": { "type": "base64", "data": "..." } }
                ]}
            ]
        });
        let err = translate_request(&req).unwrap_err();
        assert!(err.contains("image"), "error names the block type: {err}");
    }

    #[test]
    fn thinking_stays_rejected_with_a_named_error() {
        let req = json!({
            "model": "llama3",
            "thinking": { "type": "enabled", "budget_tokens": 1024 },
            "messages": [{ "role": "user", "content": "x" }]
        });
        assert!(translate_request(&req).unwrap_err().contains("thinking"));
    }

    #[test]
    fn openai_response_becomes_an_anthropic_message() {
        let openai = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "hello there" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 }
        });
        let out = translate_response(&openai, "llama3");
        assert_eq!(out["id"], "chatcmpl-1");
        assert_eq!(out["type"], "message");
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["model"], "llama3");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "hello there");
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["stop_sequence"], Value::Null);
        assert_eq!(out["usage"]["input_tokens"], 11);
        assert_eq!(out["usage"]["output_tokens"], 7);
    }

    #[test]
    fn finish_reason_maps_to_stop_reasons() {
        assert_eq!(map_stop_reason("length"), "max_tokens");
        assert_eq!(map_stop_reason("tool_calls"), "tool_use");
        assert_eq!(map_stop_reason("stop"), "end_turn");
        assert_eq!(map_stop_reason("content_filter"), "end_turn");

        for (finish, stop) in [("length", "max_tokens"), ("stop", "end_turn")] {
            let openai = json!({
                "id": "c1",
                "choices": [{ "message": { "content": "x" }, "finish_reason": finish }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
            });
            let out = translate_response(&openai, "m");
            assert_eq!(out["stop_reason"], stop);
        }
    }

    #[test]
    fn empty_openai_content_yields_no_text_block() {
        let openai = json!({
            "id": "c2",
            "choices": [{ "message": { "content": "" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 0 }
        });
        let out = translate_response(&openai, "m");
        assert_eq!(out["content"].as_array().unwrap().len(), 0);
        assert_eq!(out["usage"]["output_tokens"], 0);
    }

    #[test]
    fn missing_choices_and_usage_do_not_panic() {
        let out = translate_response(&json!({ "id": "c3" }), "m");
        assert_eq!(out["type"], "message");
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["usage"]["input_tokens"], 0);
    }

    /// A two-content-chunk stream plus the final usage frame and [DONE] must
    /// produce the exact Anthropic event sequence with correct framing.
    #[test]
    fn stream_translates_to_the_full_anthropic_event_sequence() {
        let mut t = StreamTranslator::new("llama3");
        let mut events = Vec::new();
        events.extend(t.feed_bytes(
            b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":null}}]}\n\n",
        ));
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
        ));
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
        ));
        events.extend(t.feed_bytes(b"data: [DONE]\n\n"));

        let names = events_names(&events);
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );

        // Framing: each event is exactly "event: <name>\ndata: <json>\n\n".
        for e in &events {
            assert!(e.ends_with("\n\n"), "event is blank-line terminated");
            let mut lines = e.lines();
            let name = lines.next().unwrap().strip_prefix("event: ").unwrap();
            let data = lines.next().unwrap();
            assert!(data.starts_with("data: "));
            assert!(serde_json::from_str::<Value>(&data["data: ".len()..]).is_ok());
            assert_eq!(name, events_names(std::slice::from_ref(e))[0]);
        }

        let start = event_data(&events[0]);
        assert_eq!(start["type"], "message_start");
        assert_eq!(start["message"]["role"], "assistant");
        assert_eq!(start["message"]["model"], "llama3");
        assert_eq!(start["message"]["usage"]["input_tokens"], 0);

        let delta1 = event_data(&events[2]);
        assert_eq!(delta1["type"], "content_block_delta");
        assert_eq!(delta1["index"], 0);
        assert_eq!(delta1["delta"]["type"], "text_delta");
        assert_eq!(delta1["delta"]["text"], "Hel");
        assert_eq!(event_data(&events[3])["delta"]["text"], "lo");

        let delta_msg = event_data(&events[5]);
        assert_eq!(delta_msg["type"], "message_delta");
        assert_eq!(delta_msg["delta"]["stop_reason"], "end_turn");
        assert_eq!(delta_msg["usage"]["output_tokens"], 7);
        assert_eq!(delta_msg["usage"]["input_tokens"], 11);

        assert_eq!(event_data(&events[6])["type"], "message_stop");
    }

    #[test]
    fn length_finish_reason_flows_through_the_stream() {
        let mut t = StreamTranslator::new("m");
        let mut events =
            t.feed_bytes(b"data: {\"choices\":[{\"delta\":{\"content\":\"abc\"}}]}\n\n");
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
        ));
        events.extend(t.feed_bytes(b"data: [DONE]\n\n"));
        let delta_msg = event_data(&events[events.len() - 2]);
        assert_eq!(delta_msg["delta"]["stop_reason"], "max_tokens");
    }

    #[test]
    fn a_usage_only_frame_with_empty_choices_still_opens_the_message() {
        // Ollama sends a role-only chunk first and a choices-less usage frame
        // last; both must land inside a well-formed event sequence.
        let mut t = StreamTranslator::new("m");
        let mut events = t.feed_bytes(
            b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\n",
        );
        events.extend(t.feed_bytes(b"data: [DONE]\n\n"));
        let names = events_names(&events);
        assert_eq!(names[0], "message_start");
        assert_eq!(*names.last().unwrap(), "message_stop");
        let delta_msg = event_data(&events[events.len() - 2]);
        assert_eq!(delta_msg["usage"]["output_tokens"], 3);
    }

    #[test]
    fn a_stream_split_across_chunk_boundaries_still_translates() {
        let mut t = StreamTranslator::new("m");
        let mut events = Vec::new();
        events.extend(t.feed_bytes(b"data: {\"choices\":[{\"del"));
        events.extend(t.feed_bytes(b"ta\":{\"content\":\"hi\"}}]}\n\ndata: [DO"));
        events.extend(t.feed_bytes(b"NE]\n\n"));
        let names = events_names(&events);
        assert!(names.contains(&"content_block_delta".to_string()));
        assert_eq!(names.last().unwrap(), "message_stop");
        assert_eq!(event_data(&events[2])["delta"]["text"], "hi");
    }

    #[test]
    fn a_multibyte_character_split_across_chunks_is_not_corrupted() {
        // "héllo" — é is two bytes (0xC3 0xA9); split the pair across feeds.
        let payload = "data: {\"choices\":[{\"delta\":{\"content\":\"h\u{e9}llo\"}}]}\n\n";
        let bytes = payload.as_bytes();
        let split = bytes.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let mut t = StreamTranslator::new("m");
        let mut events = t.feed_bytes(&bytes[..split]);
        events.extend(t.feed_bytes(&bytes[split..]));
        let text = events
            .iter()
            .filter(|e| e.contains("content_block_delta"))
            .find_map(|e| {
                let line = e.lines().find(|l| l.starts_with("data:"))?;
                let v: Value = serde_json::from_str(&line[5..]).ok()?;
                v["delta"]["text"].as_str().map(str::to_string)
            })
            .unwrap_or_default();
        assert_eq!(text, "h\u{e9}llo");
        assert!(!text.contains('\u{FFFD}'));
    }

    #[test]
    fn text_after_a_tool_call_opens_a_fresh_text_block() {
        let mut t = StreamTranslator::new("m");
        let mut events = Vec::new();
        // Tool call first...
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]}}]}\n\n",
        ));
        // ...then plain text.
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"done!\"}}]}\n\ndata: [DONE]\n\n",
        ));
        // Index 0 must never receive a text_delta.
        for e in &events {
            if e.contains("text_delta") {
                let line = e.lines().find(|l| l.starts_with("data:")).unwrap();
                let v: serde_json::Value = serde_json::from_str(&line[5..]).unwrap();
                assert_ne!(v["index"], 0, "text_delta landed in the tool_use block");
            }
        }
        let starts: Vec<(u64, String)> = events
            .iter()
            .filter(|e| e.contains("content_block_start"))
            .map(|e| {
                let line = e.lines().find(|l| l.starts_with("data:")).unwrap();
                let v: serde_json::Value = serde_json::from_str(&line[5..]).unwrap();
                (
                    v["index"].as_u64().unwrap(),
                    v["content_block"]["type"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(starts, vec![(0, "tool_use".into()), (1, "text".into())]);
    }

    #[test]
    fn parallel_tool_call_fragments_never_reuse_closed_indexes() {
        let mut t = StreamTranslator::new("m");
        let mut events = Vec::new();
        let frame = |idx: u64, args: &str| {
            format!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{idx},\"function\":{{\"arguments\":\"{args}\"}}}}]}}}}]}}\n\n"
            )
        };
        // Fragments interleave across two parallel calls: 0, 1, 0.
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"f\",\"arguments\":\"{\"}}]}}]}\n\n",
        ));
        events.extend(t.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"b\",\"function\":{\"name\":\"g\",\"arguments\":\"{\"}}]}}]}\n\n",
        ));
        events.extend(t.feed_bytes(frame(0, "x}").as_bytes()));
        events.extend(t.feed_bytes(b"data: [DONE]\n\n"));
        // Reconstruct per-index event timelines: no delta may follow a stop
        // on the same index.
        let mut stopped = std::collections::HashSet::new();
        for e in &events {
            let line = e.lines().find(|l| l.starts_with("data:")).unwrap();
            let v: serde_json::Value = serde_json::from_str(&line[5..]).unwrap();
            let idx = v["index"].as_u64().unwrap_or(0);
            match v["type"].as_str().unwrap() {
                "content_block_stop" => {
                    stopped.insert(idx);
                }
                "content_block_delta" => {
                    assert!(!stopped.contains(&idx), "delta after stop on index {idx}");
                }
                _ => {}
            }
        }
    }

    #[test]
    fn round_trip_request_shape_matches_openai_chat_contract() {
        let req = json!({
            "model": "llama3",
            "system": "be brief",
            "max_tokens": 32,
            "stream": true,
            "messages": [
                { "role": "user", "content": "one" },
                { "role": "assistant", "content": "two" },
                { "role": "user", "content": "three" }
            ]
        });
        let out = translate_request(&req).unwrap();
        assert!(out.get("messages").unwrap().is_array());
        assert_eq!(out["messages"].as_array().unwrap().len(), 4); // system + 3
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][3]["role"], "user");
        assert_eq!(out["stream"], true);

        let openai = json!({
            "id": "rt",
            "choices": [{ "message": { "content": "reply" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 4 }
        });
        let resp = translate_response(&openai, "llama3");
        assert_eq!(resp["type"], "message");
        assert_eq!(resp["content"][0]["text"], "reply");
        assert_eq!(resp["usage"]["input_tokens"], 9);
        assert_eq!(resp["usage"]["output_tokens"], 4);
    }
}
