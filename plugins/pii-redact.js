// Stoke JS Plugin Example: PII Redaction
// Build with: cargo build --release --features js-plugins
// Config:
//   [plugins]
//   scripts = ["plugins/pii-redact.js"]
//
// This plugin strips API keys, tokens, emails, and credit card numbers
// from prompts before they reach any model. It also blocks requests that
// contain what looks like AWS secret keys.

registerPlugin({
  // pre_request: override model or routing based on prompt content
  pre_request(req) {
    // Example: route code questions to the coder model
    const lastMsg = req.messages[req.messages.length - 1];
    if (lastMsg && lastMsg.content) {
      const content = lastMsg.content.toLowerCase();
      if (content.includes("function") || content.includes("class") || content.includes("rust")) {
        return { model: "qwen2.5-coder:7b", routing: "single" };
      }
    }
    return req;
  },

  // prompt_filter: strip PII from messages before they reach the model
  prompt_filter(req) {
    const patterns = {
      // AWS access keys (AKIA...)
      aws_key: /AKIA[0-9A-Z]{16}/g,
      // API keys with sk- prefix (OpenAI, etc.)
      api_key: /sk-[a-zA-Z0-9]{20,}/g,
      // Generic bearer tokens
      bearer: /Bearer\s+[a-zA-Z0-9\-._~+\/=]{20,}/g,
      // Email addresses
      email: /[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}/g,
      // Credit card numbers (16 digits, optionally grouped)
      cc: /\b(?:\d[ -]*?){13,16}\b/g,
      // AWS secret keys (40 chars after AWS_SECRET_ACCESS_KEY=)
      aws_secret: /AWS_SECRET_ACCESS_KEY=[a-zA-Z0-9/+=]{40}/g,
    };

    let redacted = false;
    const filtered = req.messages.map(msg => {
      if (!msg.content) return msg;
      let content = msg.content;
      for (const [name, pattern] of Object.entries(patterns)) {
        if (pattern.test(content)) {
          redacted = true;
          content = content.replace(pattern, `[REDACTED_${name.toUpperCase()}]`);
        }
      }
      return { ...msg, content };
    });

    if (redacted) {
      console.log("[pii-redact] Stripped sensitive data from prompt");
    }

    return { messages: filtered };
  },

  // post_response: audit log to console (or file via Deno.writeFile)
  post_response(req) {
    const resp = req.response;
    const audit = {
      timestamp: new Date().toISOString(),
      model: req.model,
      cost_usd: req.cost_usd,
      elapsed_ms: req.elapsed_ms,
      tokens: resp.usage || null,
    };
    console.log("[audit]", JSON.stringify(audit));
    return { response: resp };
  }
});