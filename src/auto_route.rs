//! Auto-routing v2: classify the request, then OPTIMIZE — score every
//! eligible (model, node) candidate on cost, predicted latency, and the
//! user's stated preference order, and pick the best under the requested mode.
//!
//! Principles carried over from v1:
//! - **Stoke ships zero model names.** Candidates come from explicit
//!   `[auto_route]` config (role = ordered candidate list; first entry is the
//!   preference) or from models discovered on the user's own nodes.
//! - Unresolved → reject with instructions, never guess.
//! - This module decides WHAT runs; placement (`nodes.rs`) decides WHERE.
//!   Scoring uses per-node stats to *predict*, but the final node choice is
//!   placement's — both read the same registry, so they agree.
//!
//! Modes (selected by pseudo-model name): `auto` balances cost and speed,
//! `auto-cheap` weights cost, `auto-fast` weights speed. Preference order
//! always contributes: earlier entries in a role list win ties.

use serde_json::Value;

use crate::config::Config;
use crate::cost::Pricer;
use crate::nodes::{DiscoveredModel, NodeRegistry};

/// Heuristic constants (not model knowledge — physics guesses, overridden by
/// measurement as soon as the registry has real numbers).
const COLD_LOAD_MS: f64 = 8_000.0;
const UNKNOWN_NODE_BASE_MS: f64 = 3_000.0;
const DEFAULT_TPS: f64 = 25.0;

#[derive(Debug, Clone, PartialEq)]
pub enum PromptClass {
    Code,
    Reasoning,
    Chat,
    LongContext,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Balanced,
    Cheap,
    Fast,
}

impl Mode {
    /// Parse from the requested pseudo-model name ("auto", "auto-cheap", …).
    /// Tolerant of case, surrounding whitespace, and an Ollama-style `:tag`
    /// suffix a client UI may append — so "Auto-Cheap" or "auto-cheap:latest"
    /// don't silently fall back to Balanced and drop the user's cost intent.
    pub fn from_model_name(name: &str) -> Mode {
        let n = name.trim().to_lowercase();
        let n = n.split(':').next().unwrap_or(&n);
        match n {
            "auto-cheap" => Mode::Cheap,
            "auto-fast" => Mode::Fast,
            _ => Mode::Balanced,
        }
    }
    /// (w_cost, w_speed, w_preference)
    fn weights(self) -> (f64, f64, f64) {
        match self {
            Mode::Balanced => (1.0, 1.0, 0.4),
            Mode::Cheap => (2.5, 0.5, 0.3),
            Mode::Fast => (0.4, 2.5, 0.3),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Balanced => "auto",
            Mode::Cheap => "auto-cheap",
            Mode::Fast => "auto-fast",
        }
    }
}

/// One scored (model, node) candidate — kept for the receipts even when
/// rejected, so every decision can show the road not taken.
#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub model: String,
    pub node: String,
    pub warm: bool,
    pub tier_rank: u8,
    pub est_cost_usd: f64,
    pub predicted_ms: f64,
    pub score: f64,
    /// Some(reason) = ineligible (context too small, no tool support, …).
    pub excluded: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AutoDecision {
    pub class: PromptClass,
    pub pattern: String,
    pub model: String,
    pub vote_models: Vec<String>,
    pub reason: String,
    pub chosen: Option<ScoredCandidate>,
    pub not_taken: Vec<ScoredCandidate>,
    /// Counterfactual cost of the configured quality model for this request's
    /// token estimate — the honest basis for the savings receipts. 0 when no
    /// quality model is configured (then no savings are ever claimed).
    pub counterfactual_usd: f64,
}

/// Concatenate all message text, handling both OpenAI content forms: a plain
/// string, or an array of parts (`[{"type":"text","text":...}, ...]`). Dropping
/// the array form silently mis-sizes prompts and skips fit exclusions.
pub fn extract_text(messages: &[Value]) -> String {
    let mut out: Vec<String> = Vec::new();
    for m in messages {
        match m.get("content") {
            Some(Value::String(s)) => out.push(s.clone()),
            Some(Value::Array(parts)) => {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                        out.push(t.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    out.join(" ")
}

/// Classify a prompt into a category. No LLM call — pure heuristics.
pub fn classify(messages: &[Value]) -> PromptClass {
    let combined = extract_text(messages).to_lowercase();

    let est_tokens = combined.len() / 4;
    if est_tokens > 4000 {
        return PromptClass::LongContext;
    }

    let code_signals = [
        "```", "def ", "function ", "class ", "import ", "console.log",
        "implement", "bug fix", "refactor", "api endpoint", "sql",
        "python", "javascript", "typescript", "rust", "go ", "java ",
        "compile", "syntax", "algorithm",
        "write a function", "write a class",
    ];
    let code_score = code_signals.iter().filter(|s| combined.contains(*s)).count();

    let reasoning_signals = [
        "prove", "derive", "mathematic", "calculate", "solve for",
        "step by step", "logic", "theorem", "equation", "integral",
        "probability", "statistic", "optimize", "complexity",
        "how much", "how long", "how many",
        "solve", "show your work", "show reasoning",
    ];
    let reasoning_score = reasoning_signals.iter().filter(|s| combined.contains(*s)).count();

    if code_score >= reasoning_score && code_score > 0 {
        PromptClass::Code
    } else if reasoning_score > 0 {
        PromptClass::Reasoning
    } else {
        PromptClass::Chat
    }
}

/// Resolved role → ordered candidate lists. Empty = unresolved.
#[derive(Debug, Clone, Default)]
pub struct RouteOpts {
    pub fast: Vec<String>,
    pub coder: Vec<String>,
    pub reasoner: Vec<String>,
    pub long_context: Vec<String>,
    pub quality: Vec<String>,
    pub vote_models: Vec<String>,
    pub quality_mode: bool,
    pub test_code: String,
    pub entry_point: String,
}

impl RouteOpts {
    pub fn has_tests(&self) -> bool {
        !self.test_code.is_empty() && !self.entry_point.is_empty()
    }

    /// Resolve candidate lists from config + discovery. Synchronous.
    pub fn resolve(
        config: &Config,
        registry: &NodeRegistry,
        extra: &serde_json::Map<String, Value>,
    ) -> Self {
        let ar = &config.auto_route;
        let discovered = registry.discovered();

        // Naming-convention heuristics over the user's own discovered models —
        // used only when a role has no explicit config.
        let by_name = |needles: &[&str]| -> Vec<String> {
            let mut hits: Vec<&DiscoveredModel> = discovered
                .iter()
                .filter(|d| {
                    let n = d.name.to_lowercase();
                    needles.iter().any(|s| n.contains(s))
                })
                .collect();
            hits.sort_by_key(|d| (!d.warm, d.tier_rank));
            let mut out: Vec<String> = Vec::new();
            for h in hits {
                if !out.contains(&h.name) {
                    out.push(h.name.clone());
                }
            }
            out.truncate(3);
            out
        };
        let first_available = || -> Vec<String> {
            let mut all: Vec<&DiscoveredModel> = discovered.iter().collect();
            all.sort_by_key(|d| (!d.warm, d.tier_rank));
            all.first().map(|d| vec![d.name.clone()]).unwrap_or_default()
        };

        let mut fast = ar.fast_vec();
        if fast.is_empty() {
            if let Some(dm) = config.default_model.clone() {
                fast = vec![dm];
            } else {
                fast = first_available();
            }
        }

        let coder = if ar.coder_vec().is_empty() { by_name(&["coder", "code"]) } else { ar.coder_vec() };
        let reasoner = if ar.reasoner_vec().is_empty() { by_name(&["reason", "think", "r1"]) } else { ar.reasoner_vec() };
        let long_context = if ar.long_context_vec().is_empty() {
            by_name(&["128k", "256k", "1m", "long"])
        } else {
            ar.long_context_vec()
        };
        let quality = ar.quality_vec(); // config-only, never guessed

        let vote_models = if !ar.vote_models.is_empty() {
            ar.vote_models.clone()
        } else {
            let mut locals: Vec<&DiscoveredModel> = discovered.iter().filter(|d| d.tier_rank == 0).collect();
            locals.sort_by_key(|d| !d.warm);
            let mut out: Vec<String> = Vec::new();
            for l in locals {
                if !out.contains(&l.name) {
                    out.push(l.name.clone());
                }
            }
            out.truncate(5);
            out
        };

        Self {
            fast,
            coder,
            reasoner,
            long_context,
            quality,
            vote_models,
            quality_mode: extra.get("quality_mode").and_then(|v| v.as_bool()).unwrap_or(false),
            test_code: extra.get("test_code").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            entry_point: extra.get("entry_point").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        }
    }
}

/// Everything the scorer knows about the request.
pub struct RequestFacts {
    pub prompt_tokens_est: u64,
    pub gen_tokens_est: u64,
    pub has_tools: bool,
    pub mode: Mode,
}

/// Score all candidates for an ordered model list. Returns candidates sorted
/// best-first (excluded ones at the end, with reasons).
fn score_list(
    models: &[String],
    discovered: &[DiscoveredModel],
    pricer: &Pricer,
    facts: &RequestFacts,
) -> Vec<ScoredCandidate> {
    let usage = serde_json::json!({
        "prompt_tokens": facts.prompt_tokens_est,
        "completion_tokens": facts.gen_tokens_est,
        "total_tokens": facts.prompt_tokens_est + facts.gen_tokens_est,
    });

    let mut cands: Vec<ScoredCandidate> = Vec::new();
    for (pref_idx, model) in models.iter().enumerate() {
        let entries: Vec<&DiscoveredModel> =
            discovered.iter().filter(|d| d.matches(model)).collect();

        let est_cost_usd = pricer.calculate(model, Some(&usage)).cost_usd;

        if entries.is_empty() {
            // Not discovered anywhere — legitimate for cloud/quality targets
            // reached through an unpolled provider. Capability unknown.
            cands.push(ScoredCandidate {
                model: model.clone(),
                node: "(undiscovered)".to_string(),
                warm: false,
                tier_rank: 2,
                est_cost_usd,
                predicted_ms: UNKNOWN_NODE_BASE_MS
                    + facts.gen_tokens_est as f64 / DEFAULT_TPS * 1000.0,
                score: f64::MAX, // filled in normalization pass
                excluded: None,
            });
            let c = cands.last_mut().unwrap();
            c.score = pref_idx as f64; // provisional; normalized below
            continue;
        }

        for d in entries {
            // Fit constraints — hard exclusions with reasons kept for receipts.
            let mut excluded = None;
            if let Some(ctx) = d.context_length {
                if facts.prompt_tokens_est + facts.gen_tokens_est > ctx {
                    excluded = Some(format!("context window {} too small", ctx));
                }
            }
            if excluded.is_none() && facts.has_tools && d.tools == Some(false) {
                excluded = Some("no structured tool_calls support".to_string());
            }

            // Sanitize measured stats: a hostile/buggy (possibly federated)
            // peer must never inject NaN/inf/subnormal that poisons scoring.
            let sane = |v: Option<f64>, lo: f64, hi: f64| -> Option<f64> {
                v.filter(|x| x.is_finite() && *x >= lo && *x <= hi)
            };
            let ttft_base = sane(d.ttft_ms, 1.0, 600_000.0).unwrap_or(UNKNOWN_NODE_BASE_MS / 4.0);
            let ttft = if d.warm { ttft_base } else { COLD_LOAD_MS + ttft_base };
            let tps = sane(d.tps, 0.1, 100_000.0).unwrap_or(DEFAULT_TPS);
            let predicted_ms = ttft + facts.gen_tokens_est as f64 / tps * 1000.0;

            cands.push(ScoredCandidate {
                model: model.clone(),
                node: d.node.clone(),
                warm: d.warm,
                tier_rank: d.tier_rank,
                est_cost_usd,
                predicted_ms,
                score: pref_idx as f64, // provisional preference index
                excluded,
            });
        }
    }

    // Normalize + combine. Denominators are computed over ELIGIBLE candidates
    // only — an excluded candidate's extreme cost/latency must not rescale the
    // real contenders (which would let the preference term flip the winner).
    let elig = |sel: &dyn Fn(&ScoredCandidate) -> f64| {
        cands
            .iter()
            .filter(|c| c.excluded.is_none())
            .map(sel)
            .filter(|v| v.is_finite())
            .fold(0.0_f64, f64::max)
    };
    let max_cost = elig(&|c| c.est_cost_usd).max(1e-9);
    let max_ms = elig(&|c| c.predicted_ms).max(1.0);
    let (w_cost, w_speed, w_pref) = facts.mode.weights();
    for c in &mut cands {
        let pref = c.score;
        // clamp ratios into [0,1] so an excluded outlier can't exceed the scale
        let cost_term = (c.est_cost_usd / max_cost).clamp(0.0, 1.0);
        let ms_term = (c.predicted_ms / max_ms).clamp(0.0, 1.0);
        c.score = w_cost * cost_term + w_speed * ms_term + w_pref * pref;
        if !c.score.is_finite() {
            c.score = f64::MAX; // defensive: never let NaN reach the comparator
        }
    }
    cands.sort_by(|a, b| {
        (a.excluded.is_some(), a.score)
            .partial_cmp(&(b.excluded.is_some(), b.score))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    cands
}

/// Decide pattern + model for a classified prompt, with full receipts.
pub fn decide(
    messages: &[Value],
    opts: &RouteOpts,
    discovered: &[DiscoveredModel],
    pricer: &Pricer,
    facts: &RequestFacts,
) -> AutoDecision {
    let class = classify(messages);

    // Candidate list for the class; quality mode overrides when configured.
    let (role_name, mut models): (&str, Vec<String>) =
        if opts.quality_mode && !opts.quality.is_empty() {
            ("quality", opts.quality.clone())
        } else {
            match class {
                PromptClass::Code => ("code", opts.coder.clone()),
                PromptClass::Reasoning => ("reasoning", opts.reasoner.clone()),
                PromptClass::LongContext => ("long-context", opts.long_context.clone()),
                PromptClass::Chat => ("chat", opts.fast.clone()),
            }
        };
    if models.is_empty() {
        models = opts.fast.clone(); // unresolved role falls back to fast
    }

    // Counterfactual = list price of the quality model — but only when that
    // model could actually have served THIS request (fits context, has tools if
    // needed). Otherwise the "you would have paid" claim is false and would
    // overstate savings on /v1/budget. Undiscovered quality models (reached via
    // an unpolled cloud provider) have unknown capability → assumed eligible.
    let counterfactual_usd = opts.quality.first().map(|q| {
        let q_fits = discovered
            .iter()
            .filter(|d| d.matches(q))
            .fold(None::<bool>, |acc, d| {
                let fits = d.context_length.map(|ctx| facts.prompt_tokens_est + facts.gen_tokens_est <= ctx).unwrap_or(true)
                    && !(facts.has_tools && d.tools == Some(false));
                Some(acc.unwrap_or(false) || fits)
            });
        // None = not discovered anywhere (unknown capability) → assume eligible.
        if q_fits == Some(false) {
            return 0.0;
        }
        let usage = serde_json::json!({
            "prompt_tokens": facts.prompt_tokens_est,
            "completion_tokens": facts.gen_tokens_est,
            "total_tokens": facts.prompt_tokens_est + facts.gen_tokens_est,
        });
        pricer.calculate(q, Some(&usage)).cost_usd
    }).unwrap_or(0.0);

    if models.is_empty() {
        return AutoDecision {
            class,
            pattern: "single".into(),
            model: String::new(),
            vote_models: vec![],
            reason: "no candidates: set default_model or [auto_route] roles".into(),
            chosen: None,
            not_taken: vec![],
            counterfactual_usd,
        };
    }

    // Always score first, so fit exclusions (context/tools) and discovery are
    // honored even on the validated-pattern path — the primary model must be a
    // candidate the scorer would actually pick, never a blind models.first().
    let scored = score_list(&models, discovered, pricer, facts);
    let chosen = scored.iter().find(|c| c.excluded.is_none()).cloned();

    // Validated pattern when tests ride along, and a fit primary model exists.
    if class == PromptClass::Code && opts.has_tests() && opts.vote_models.len() >= 2 {
        if let Some(c) = &chosen {
            return AutoDecision {
                class,
                pattern: "cascade_test".into(),
                model: c.model.clone(),
                vote_models: opts.vote_models.clone(),
                reason: format!(
                    "Code with tests → validated fallback (primary {} on {})",
                    c.model, c.node
                ),
                chosen: Some(c.clone()),
                not_taken: vec![],
                counterfactual_usd,
            };
        }
        // else fall through to the standard exclusion-reporting path below
    }

    let not_taken: Vec<ScoredCandidate> = scored
        .iter()
        .filter(|c| {
            chosen
                .as_ref()
                .map(|ch| !(ch.model == c.model && ch.node == c.node))
                .unwrap_or(true)
        })
        .take(4)
        .cloned()
        .collect();

    match chosen {
        Some(c) => AutoDecision {
            class,
            pattern: "single".into(),
            model: c.model.clone(),
            vote_models: vec![],
            reason: format!(
                "{} role, {} mode → {} on {} (est ${:.4}, ~{:.1}s{})",
                role_name,
                facts.mode.label(),
                c.model,
                c.node,
                c.est_cost_usd,
                c.predicted_ms / 1000.0,
                if c.warm { ", warm" } else { ", cold" },
            ),
            chosen: Some(c),
            not_taken,
            counterfactual_usd,
        },
        None => AutoDecision {
            class,
            pattern: "single".into(),
            model: String::new(),
            vote_models: vec![],
            reason: format!(
                "all {} candidates excluded: {}",
                role_name,
                scored
                    .iter()
                    .filter_map(|c| c.excluded.as_ref().map(|e| format!("{}: {}", c.model, e)))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            chosen: None,
            not_taken: scored.into_iter().take(4).collect(),
            counterfactual_usd,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dm(name: &str, node: &str, tier: u8, warm: bool) -> DiscoveredModel {
        DiscoveredModel {
            name: name.into(),
            node: node.into(),
            tier_rank: tier,
            warm,
            context_length: None,
            tools: None,
            tps: None,
            ttft_ms: None,
        }
    }

    fn facts(mode: Mode) -> RequestFacts {
        RequestFacts { prompt_tokens_est: 500, gen_tokens_est: 500, has_tools: false, mode }
    }

    fn opts() -> RouteOpts {
        RouteOpts {
            fast: vec!["fixture-fast".into()],
            coder: vec!["fixture-coder".into(), "fixture-coder-small".into()],
            reasoner: vec!["fixture-reasoner".into()],
            long_context: vec!["fixture-long".into()],
            quality: vec![],
            vote_models: vec!["fixture-a".into(), "fixture-b".into()],
            quality_mode: false,
            test_code: String::new(),
            entry_point: String::new(),
        }
    }

    #[test]
    fn test_classify_code() {
        let msgs = vec![json!({"role": "user", "content": "Implement a Python function to sort a list"})];
        assert_eq!(classify(&msgs), PromptClass::Code);
    }

    #[test]
    fn test_classify_long_context() {
        let long = "a".repeat(20000);
        let msgs = vec![json!({"role": "user", "content": long})];
        assert_eq!(classify(&msgs), PromptClass::LongContext);
    }

    #[test]
    fn warm_candidate_beats_cold_same_price() {
        let discovered = vec![
            dm("fixture-coder", "laptop", 0, false),
            dm("fixture-coder", "studio", 1, true),
        ];
        let msgs = vec![json!({"role": "user", "content": "refactor this rust code"})];
        let d = decide(&msgs, &opts(), &discovered, &Pricer::default(), &facts(Mode::Balanced));
        let c = d.chosen.expect("chosen");
        assert_eq!(c.node, "studio", "warm node should win: {:?}", d.reason);
    }

    #[test]
    fn preference_order_wins_ties() {
        // both candidates cold, same (zero) cost — first-listed must win
        let discovered = vec![
            dm("fixture-coder", "laptop", 0, false),
            dm("fixture-coder-small", "laptop", 0, false),
        ];
        let msgs = vec![json!({"role": "user", "content": "implement a python function"})];
        let d = decide(&msgs, &opts(), &discovered, &Pricer::default(), &facts(Mode::Balanced));
        assert_eq!(d.chosen.unwrap().model, "fixture-coder");
    }

    #[test]
    fn context_window_excludes_and_falls_through() {
        let mut small = dm("fixture-coder", "laptop", 0, true);
        small.context_length = Some(600); // prompt 500 + gen 500 > 600
        let big = dm("fixture-coder-small", "laptop", 0, false);
        let discovered = vec![small, big];
        let msgs = vec![json!({"role": "user", "content": "implement a python function"})];
        let d = decide(&msgs, &opts(), &discovered, &Pricer::default(), &facts(Mode::Balanced));
        let c = d.chosen.unwrap();
        assert_eq!(c.model, "fixture-coder-small", "must fall through to fitting model");
        assert!(d.not_taken.iter().any(|n| n.excluded.is_some()));
    }

    #[test]
    fn tools_request_excludes_non_tool_models() {
        let mut no_tools = dm("fixture-coder", "laptop", 0, true);
        no_tools.tools = Some(false);
        let ok = dm("fixture-coder-small", "laptop", 0, false);
        let discovered = vec![no_tools, ok];
        let msgs = vec![json!({"role": "user", "content": "implement a python function"})];
        let mut f = facts(Mode::Balanced);
        f.has_tools = true;
        let d = decide(&msgs, &opts(), &discovered, &Pricer::default(), &f);
        assert_eq!(d.chosen.unwrap().model, "fixture-coder-small");
    }

    #[test]
    fn quality_mode_uses_quality_list_and_counterfactual() {
        let mut o = opts();
        o.quality = vec!["glm-5.2:cloud".into()]; // priced in the table
        o.quality_mode = true;
        let discovered = vec![dm("fixture-coder", "laptop", 0, true)];
        let msgs = vec![json!({"role": "user", "content": "solve step by step"})];
        let d = decide(&msgs, &o, &discovered, &Pricer::default(), &facts(Mode::Balanced));
        assert_eq!(d.chosen.unwrap().model, "glm-5.2:cloud");
        assert!(d.counterfactual_usd > 0.0, "counterfactual priced from quality[0]");
    }

    #[test]
    fn cheap_mode_prefers_zero_marginal_over_paid() {
        let mut o = opts();
        // user explicitly allows a paid alternate after the local preference —
        // in cheap mode the local must win even if the paid one is warm/fast
        o.coder = vec!["glm-5.2:cloud".into(), "fixture-coder".into()];
        let discovered = vec![dm("fixture-coder", "laptop", 0, true)];
        let msgs = vec![json!({"role": "user", "content": "implement a python function"})];
        let d = decide(&msgs, &o, &discovered, &Pricer::default(), &facts(Mode::Cheap));
        assert_eq!(d.chosen.unwrap().model, "fixture-coder");
    }

    #[test]
    fn excluded_candidate_does_not_contaminate_normalization() {
        // A(pref0) predicted slow-ish, B(pref1) faster; plus an EXCLUDED huge-
        // latency candidate C. Without eligible-only normalization, C's max_ms
        // would compress terms and let A win auto-fast. B must still win.
        let mut a = dm("fixture-coder", "laptop", 0, true);
        a.tps = Some(20.0); a.ttft_ms = Some(300.0);
        let mut b = dm("fixture-coder-small", "laptop", 0, true);
        b.tps = Some(60.0); b.ttft_ms = Some(150.0);
        let mut c = dm("fixture-coder", "studio", 1, false); // excluded by ctx
        c.tps = Some(1.0); c.ttft_ms = Some(300.0); c.context_length = Some(10);
        let mut o = opts();
        o.coder = vec!["fixture-coder".into(), "fixture-coder-small".into()];
        let msgs = vec![json!({"role":"user","content":"implement a python function"})];
        let mut f = facts(Mode::Fast);
        f.prompt_tokens_est = 500; f.gen_tokens_est = 500;
        let d = decide(&msgs, &o, &[a, b, c], &Pricer::default(), &f);
        assert_eq!(d.chosen.unwrap().model, "fixture-coder-small", "faster eligible model must win auto-fast");
    }

    #[test]
    fn hostile_nan_perf_does_not_panic() {
        let mut bad = dm("fixture-coder", "laptop", 0, true);
        bad.tps = Some(5e-324); // subnormal → would make predicted_ms huge/inf
        let good = dm("fixture-coder-small", "laptop", 0, true);
        let mut o = opts();
        o.coder = vec!["fixture-coder".into(), "fixture-coder-small".into()];
        let msgs = vec![json!({"role":"user","content":"implement a python function"})];
        // must not panic, must return a finite choice
        let d = decide(&msgs, &o, &[bad, good], &Pricer::default(), &facts(Mode::Fast));
        assert!(d.chosen.is_some());
    }

    #[test]
    fn array_content_is_classified_and_sized() {
        let big = "word ".repeat(6000); // ~30k chars → LongContext
        let msgs = vec![json!({"role":"user","content":[{"type":"text","text": big}]})];
        assert_eq!(classify(&msgs), PromptClass::LongContext, "array content must be seen");
        assert!(extract_text(&msgs).len() > 20000);
    }

    #[test]
    fn cascade_test_respects_fit_exclusions() {
        // coder[0] can't fit; tests present → must NOT blindly pick coder[0]
        let mut small = dm("fixture-coder", "laptop", 0, true);
        small.context_length = Some(50);
        let big = dm("fixture-coder-small", "laptop", 0, true);
        let mut o = opts();
        o.coder = vec!["fixture-coder".into(), "fixture-coder-small".into()];
        o.test_code = "assert True".into();
        o.entry_point = "f".into();
        let msgs = vec![json!({"role":"user","content":"implement python function def f(): pass  # code"})];
        let mut f = facts(Mode::Balanced);
        f.prompt_tokens_est = 100; f.gen_tokens_est = 100;
        let d = decide(&msgs, &o, &[small, big], &Pricer::default(), &f);
        assert_eq!(d.pattern, "cascade_test");
        assert_eq!(d.model, "fixture-coder-small", "must skip the unfitting primary");
    }

    #[test]
    fn mode_parsing_tolerates_near_misses() {
        assert_eq!(Mode::from_model_name(" Auto-Cheap "), Mode::Cheap);
        assert_eq!(Mode::from_model_name("auto-fast:latest"), Mode::Fast);
        assert_eq!(Mode::from_model_name("auto"), Mode::Balanced);
    }

    #[test]
    fn ineligible_quality_model_zeroes_counterfactual() {
        // quality model discovered but can't do tools on a has_tools request →
        // no savings should be claimed against an impossible alternative.
        let mut q = dm("glm-5.2:cloud", "laptop", 0, true);
        q.tools = Some(false);
        let chosen = dm("fixture-coder", "laptop", 0, true);
        let mut o = opts();
        o.quality = vec!["glm-5.2:cloud".into()];
        let msgs = vec![json!({"role":"user","content":"implement a python function"})];
        let mut f = facts(Mode::Balanced);
        f.has_tools = true;
        let d = decide(&msgs, &o, &[q, chosen], &Pricer::default(), &f);
        assert_eq!(d.counterfactual_usd, 0.0, "no counterfactual for an ineligible quality model");
    }

    #[test]
    fn nothing_resolved_yields_empty_model() {
        let o = RouteOpts::default();
        let msgs = vec![json!({"role": "user", "content": "hi"})];
        let d = decide(&msgs, &o, &[], &Pricer::default(), &facts(Mode::Balanced));
        assert!(d.model.is_empty());
    }

    #[test]
    fn receipts_list_not_taken() {
        let discovered = vec![
            dm("fixture-coder", "laptop", 0, false),
            dm("fixture-coder", "studio", 1, true),
            dm("fixture-coder-small", "laptop", 0, true),
        ];
        let msgs = vec![json!({"role": "user", "content": "refactor this rust code"})];
        let d = decide(&msgs, &opts(), &discovered, &Pricer::default(), &facts(Mode::Balanced));
        assert!(d.chosen.is_some());
        assert!(!d.not_taken.is_empty(), "receipts must show the road not taken");
    }
}
