//! Reading token usage out of a live SSE stream.
//!
//! A streamed response is forwarded to the client byte-for-byte, so the only
//! chance to learn what it cost is to watch it go past. Providers report usage
//! at the very end of the stream, in a shape that differs by wire format.
//!
//! The scanner is a passive tap: it never modifies or delays a byte. It buffers
//! only the tail of an incomplete line, because a TCP chunk boundary lands
//! wherever it likes — in the middle of `data: {"usage":` as readily as between
//! two events.

use serde_json::Value;

/// A partial line is held until its newline arrives. A provider that never
/// sends one must not be able to grow this without bound.
const MAX_PARTIAL_LINE: usize = 256 * 1024;

/// Which dialect of usage reporting to expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// OpenAI: a final `data:` frame carries `usage: {prompt_tokens, completion_tokens}`.
    /// Only sent when the request asked for it (`stream_options.include_usage`).
    OpenAi,
    /// Anthropic: `message_start` carries `message.usage.input_tokens`, and each
    /// `message_delta` carries a cumulative `usage.output_tokens`.
    Anthropic,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl Usage {
    pub fn to_openai_json(self) -> Value {
        serde_json::json!({
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.prompt_tokens + self.completion_tokens,
        })
    }
}

/// Watches SSE bytes go past and remembers the usage the provider reported.
pub struct UsageScanner {
    wire: Wire,
    partial: Vec<u8>,
    prompt_tokens: u64,
    completion_tokens: u64,
    /// We saw *some* usage. Not the same as seeing the final tally.
    seen: bool,
    /// We saw the frame that carries the FINAL token counts. Anthropic reveals
    /// its input tokens up front, in `message_start`, long before the output
    /// count is known — so a stream cut short still yields a plausible-looking
    /// usage object. Treating that as a measurement would silently under-bill a
    /// disconnected stream and never flag it as a guess.
    final_seen: bool,
    /// Complete `data:` frames carrying content, excluding `[DONE]`. Counted here
    /// rather than by scanning raw chunks, because a chunk boundary lands wherever
    /// it likes — including inside the literal `data:`.
    frames: u64,
    /// A line exceeded MAX_PARTIAL_LINE and was dropped. Usage may have been in it.
    lost: bool,
}

impl UsageScanner {
    pub fn new(wire: Wire) -> Self {
        Self {
            wire,
            partial: Vec::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            seen: false,
            final_seen: false,
            frames: 0,
            lost: false,
        }
    }

    /// Feed the next chunk of raw stream bytes. Never blocks, never allocates
    /// per event beyond the JSON parse of a `data:` line.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                let line = std::mem::take(&mut self.partial);
                self.on_line(&line);
            } else {
                if self.partial.len() >= MAX_PARTIAL_LINE {
                    // Refuse to buffer an unbounded line. Drop it and remember
                    // that we did, so the caller does not mistake "no usage
                    // reported" for "nothing was spent".
                    self.partial.clear();
                    self.lost = true;
                    continue;
                }
                self.partial.push(b);
            }
        }
    }

    fn on_line(&mut self, line: &[u8]) {
        let line = match std::str::from_utf8(line) {
            Ok(s) => s.trim(),
            Err(_) => return,
        };
        let payload = match line.strip_prefix("data:") {
            Some(p) => p.trim(),
            None => return, // `event:`, `id:`, comments, blank separators
        };
        if payload.is_empty() || payload == "[DONE]" {
            return;
        }
        self.frames += 1;
        let v: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => return,
        };
        match self.wire {
            Wire::OpenAi => self.on_openai(&v),
            Wire::Anthropic => self.on_anthropic(&v),
        }
    }

    fn on_openai(&mut self, v: &Value) {
        // `usage` is null on every frame but the last, so a plain `get` is not enough.
        let Some(u) = v.get("usage").filter(|u| u.is_object()) else { return };
        let p = u.get("prompt_tokens").and_then(Value::as_u64);
        let c = u.get("completion_tokens").and_then(Value::as_u64);
        if let (Some(p), Some(c)) = (p, c) {
            self.prompt_tokens = p;
            self.completion_tokens = c;
            self.seen = true;
            // OpenAI only emits usage once, on the terminal frame.
            self.final_seen = true;
        }
    }

    fn on_anthropic(&mut self, v: &Value) {
        match v.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(u) = v.pointer("/message/usage") {
                    // Cache reads and creations are billed as input.
                    let input = sum_u64(u, &["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]);
                    self.prompt_tokens = input;
                    if let Some(out) = u.get("output_tokens").and_then(Value::as_u64) {
                        self.completion_tokens = out;
                    }
                    // Input tokens are final; the output count is not.
                    self.seen = true;
                }
            }
            Some("message_delta") => {
                if let Some(u) = v.get("usage") {
                    // output_tokens here is cumulative for the message, not a delta.
                    if let Some(out) = u.get("output_tokens").and_then(Value::as_u64) {
                        self.completion_tokens = out;
                        self.seen = true;
                        self.final_seen = true;
                    }
                    // Some responses only reveal input token counts at the end.
                    let input = sum_u64(u, &["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]);
                    if input > 0 {
                        self.prompt_tokens = self.prompt_tokens.max(input);
                    }
                }
            }
            _ => {}
        }
    }

    /// The provider's FINAL usage report. `None` means nothing may be billed as
    /// a measurement — either the provider never reported, or the stream ended
    /// before the frame carrying the final tally arrived.
    pub fn usage(&self) -> Option<Usage> {
        self.final_seen.then_some(Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
        })
    }

    /// Whatever was seen so far, final or not. Sharpens the fallback estimate:
    /// a disconnected Anthropic stream still told us its real input-token count.
    pub fn partial(&self) -> Option<Usage> {
        self.seen.then_some(Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
        })
    }

    /// Complete content frames seen. Approximates completion tokens when the
    /// provider reports nothing.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// True if a line was dropped for exceeding the buffer bound.
    pub fn lost_data(&self) -> bool {
        self.lost
    }
}

fn sum_u64(v: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .filter_map(|k| v.get(*k).and_then(Value::as_u64))
        .sum()
}

/// Ask an OpenAI-compatible provider to report usage on the final stream frame.
///
/// Only worth doing where the answer changes something: a provider on a free
/// tier costs nothing per token, so we leave its request — and therefore the
/// bytes its clients receive — exactly as they were. A caller who already asked
/// for usage keeps their own settings.
pub fn request_stream_usage(body: &Value, tier: &str, provider_type: &str) -> Value {
    if crate::cost::is_free_tier(tier) || provider_type == "anthropic" {
        return body.clone();
    }
    let mut out = body.clone();
    let Some(obj) = out.as_object_mut() else { return out };
    if obj.contains_key("stream_options") {
        return out; // the caller has an opinion; do not override it
    }
    obj.insert(
        "stream_options".into(),
        serde_json::json!({ "include_usage": true }),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(wire: Wire, chunks: &[&str]) -> UsageScanner {
        let mut s = UsageScanner::new(wire);
        for c in chunks {
            s.feed(c.as_bytes());
        }
        s
    }

    #[test]
    fn openai_reads_usage_from_the_final_frame() {
        let s = scan(Wire::OpenAi, &[
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":null}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}\n\n",
            "data: [DONE]\n\n",
        ]);
        assert_eq!(s.usage(), Some(Usage { prompt_tokens: 11, completion_tokens: 7 }));
    }

    #[test]
    fn a_usage_frame_split_across_chunk_boundaries_is_still_read() {
        // The whole reason this is a stateful scanner: TCP does not respect JSON.
        let s = scan(Wire::OpenAi, &[
            "data: {\"choices\":[],\"usa",
            "ge\":{\"prompt_tokens\":3,\"comple",
            "tion_tokens\":5}}\n",
        ]);
        assert_eq!(s.usage(), Some(Usage { prompt_tokens: 3, completion_tokens: 5 }));
    }

    #[test]
    fn a_boundary_inside_the_word_data_is_still_read() {
        let s = scan(Wire::OpenAi, &["da", "ta: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n"]);
        assert_eq!(s.usage(), Some(Usage { prompt_tokens: 1, completion_tokens: 2 }));
    }

    #[test]
    fn openai_null_usage_is_not_usage() {
        let s = scan(Wire::OpenAi, &["data: {\"choices\":[{\"delta\":{}}],\"usage\":null}\n"]);
        assert_eq!(s.usage(), None, "a null usage field must not read as zero tokens");
    }

    #[test]
    fn a_stream_that_never_reports_usage_yields_none() {
        let s = scan(Wire::OpenAi, &["data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n", "data: [DONE]\n"]);
        assert_eq!(s.usage(), None);
        assert!(!s.lost_data());
    }

    #[test]
    fn malformed_json_and_comments_are_ignored() {
        let s = scan(Wire::OpenAi, &[
            ": keep-alive comment\n",
            "event: ping\n",
            "data: {not json\n",
            "data: {\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2}}\n",
        ]);
        assert_eq!(s.usage(), Some(Usage { prompt_tokens: 2, completion_tokens: 2 }));
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        let s = scan(Wire::OpenAi, &["data: {\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":6}}\r\n\r\n"]);
        assert_eq!(s.usage(), Some(Usage { prompt_tokens: 4, completion_tokens: 6 }));
    }

    #[test]
    fn an_unbounded_line_is_dropped_and_flagged() {
        let mut s = UsageScanner::new(Wire::OpenAi);
        s.feed(b"data: ");
        s.feed(&vec![b'x'; MAX_PARTIAL_LINE + 10]);
        assert!(s.lost_data(), "must not buffer an unbounded line silently");
        assert_eq!(s.usage(), None);
    }

    #[test]
    fn anthropic_takes_input_from_message_start_and_output_from_the_last_delta() {
        let s = scan(Wire::Anthropic, &[
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"hi\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n",
        ]);
        assert_eq!(s.usage(), Some(Usage { prompt_tokens: 25, completion_tokens: 42 }));
    }

    #[test]
    fn anthropic_counts_cache_tokens_as_input() {
        // Cache reads are billed. Dropping them under-reports the bill.
        let s = scan(Wire::Anthropic, &[
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":90,\"output_tokens\":0}}}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n",
        ]);
        assert_eq!(s.usage(), Some(Usage { prompt_tokens: 100, completion_tokens: 5 }));
    }

    #[test]
    fn anthropic_message_delta_output_is_cumulative_not_additive() {
        let s = scan(Wire::Anthropic, &[
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":10}}\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":20}}\n",
        ]);
        assert_eq!(s.usage().unwrap().completion_tokens, 20, "20, not 30");
    }

    #[test]
    fn an_anthropic_stream_cut_before_message_delta_is_not_a_measurement() {
        // message_start reveals the input tokens immediately. Billing that as a
        // measurement would silently under-charge every disconnected stream and
        // never flag it — the exact failure this whole path exists to prevent.
        let s = scan(Wire::Anthropic, &[
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":900,\"output_tokens\":1}}}\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"partial answ\"}}\n",
        ]);
        assert_eq!(s.usage(), None, "no final tally arrived — must not read as measured");
        assert_eq!(
            s.partial(),
            Some(Usage { prompt_tokens: 900, completion_tokens: 1 }),
            "but the real input count is known and should sharpen the estimate"
        );
    }

    #[test]
    fn frames_are_counted_by_parsed_line_not_by_scanning_chunks() {
        // `data:` split across a chunk boundary must still count as one frame.
        let s = scan(Wire::OpenAi, &[
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\nda",
            "ta: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n",
            "data: [DONE]\n",
        ]);
        assert_eq!(s.frames(), 2, "[DONE] is not a content frame, and the split frame counts");
    }

    #[test]
    fn content_containing_the_literal_data_prefix_does_not_inflate_the_frame_count() {
        let s = scan(Wire::OpenAi, &["data: {\"choices\":[{\"delta\":{\"content\":\"data: fake\"}}]}\n"]);
        assert_eq!(s.frames(), 1);
    }

    #[test]
    fn openai_usage_frame_is_final_immediately() {
        let s = scan(Wire::OpenAi, &["data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n"]);
        assert!(s.usage().is_some());
    }

    #[test]
    fn free_tier_requests_are_left_untouched() {
        let body = serde_json::json!({"model": "m", "stream": true});
        assert_eq!(request_stream_usage(&body, "local", "openai_compatible"), body);
        assert_eq!(request_stream_usage(&body, "remote", "openai_compatible"), body);
    }

    #[test]
    fn metered_requests_ask_for_usage() {
        let body = serde_json::json!({"model": "m", "stream": true});
        let out = request_stream_usage(&body, "cloud", "openai_compatible");
        assert_eq!(out.pointer("/stream_options/include_usage"), Some(&Value::Bool(true)));
    }

    #[test]
    fn a_callers_own_stream_options_are_not_overridden() {
        let body = serde_json::json!({"stream_options": {"include_usage": false}});
        let out = request_stream_usage(&body, "cloud", "openai_compatible");
        assert_eq!(out.pointer("/stream_options/include_usage"), Some(&Value::Bool(false)));
    }
}
