//! Anthropic Messages -> OpenAI chat/completions translation.
//!
//! Lets regular OpenAI-compatible local providers (tier local/remote, e.g.
//! Ollama) serve `/v1/messages` clients such as Claude Code. The request is
//! translated Anthropic -> OpenAI, dispatched to `{base_url}/chat/completions`,
//! and the response (non-streaming or SSE) is translated back so the client
//! sees a normal Anthropic Messages payload.
//!
//! Fail-closed by design: anything this slice cannot represent faithfully —
//! tool blocks, images, tool definitions — is rejected with a clear 400 naming
//! the unsupported feature, never silently mangled.

use serde_json::{json, Value};

/// Translate an Anthropic Messages request body into an OpenAI
/// chat/completions request body.
///
/// Supported: `model` passthrough, `system` (string or text-block array),
/// `messages` with string or text-block content, `max_tokens`, `temperature`,
/// `top_p`, `stop_sequences` -> `stop`, `stream`. Anything else structural
/// (tools, tool_choice, thinking, non-text blocks) is a hard error.
pub fn translate_request(req: &Value) -> Result<Value, String> {
    for key in ["tools", "tool_choice", "thinking"] {
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
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            let text = content_text(m.get("content").unwrap_or(&Value::Null))?;
            messages.push(json!({ "role": role, "content": text }));
        }
    }

    let mut out = json!({ "model": model, "messages": messages });
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

/// Map an OpenAI `finish_reason` to an Anthropic `stop_reason`.
pub fn map_stop_reason(finish_reason: &str) -> &'static str {
    match finish_reason {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        _ => "end_turn",
    }
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
    let (text, finish) = choice
        .map(|c| {
            (
                c.pointer("/message/content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                c.get("finish_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("stop")
                    .to_string(),
            )
        })
        .unwrap_or_else(|| (String::new(), "stop".to_string()));
    let content = if text.is_empty() {
        vec![]
    } else {
        vec![json!({ "type": "text", "text": text })]
    };
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
    block_open: bool,
    finished: bool,
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: Option<String>,
    partial: String,
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
            block_open: false,
            finished: false,
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            partial: String::new(),
        }
    }

    /// Feed raw upstream bytes; returns complete Anthropic SSE event frames.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Vec<String> {
        self.partial.push_str(&String::from_utf8_lossy(bytes));
        let mut events = Vec::new();
        while let Some(pos) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=pos).collect();
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
                    events.push(self.event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": { "type": "text_delta", "text": text },
                        }),
                    ));
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

    fn ensure_started(&mut self) -> Vec<String> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        self.block_open = true;
        vec![
            self.event(
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
            ),
            self.event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": { "type": "text", "text": "" },
                }),
            ),
        ]
    }

    fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = self.ensure_started();
        if self.block_open {
            events.push(self.event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": 0 }),
            ));
            self.block_open = false;
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
    fn tool_definition_is_rejected_with_a_clear_error() {
        let req = json!({
            "model": "llama3",
            "tools": [{ "name": "read", "input_schema": {} }],
            "messages": [{ "role": "user", "content": "x" }]
        });
        let err = translate_request(&req).unwrap_err();
        assert!(err.contains("tools"), "error names the feature: {err}");
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
        let req2 = json!({
            "model": "llama3",
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "t1", "name": "read", "input": {} }
                ]}
            ]
        });
        assert!(translate_request(&req2).unwrap_err().contains("tool_use"));
        let req3 = json!({
            "model": "llama3",
            "messages": [
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": "ok" }
                ]}
            ]
        });
        assert!(translate_request(&req3)
            .unwrap_err()
            .contains("tool_result"));
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
