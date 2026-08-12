# Stoke ICP panel

This is a product-decision instrument, not market validation. The profiles are hypotheses to test with design partners. Each review must only score behaviour evidenced by the current landing, README, config, and runnable tests.

## Gate

Stoke passes only when **each** profile scores at least **170/180**, with both:

- landing: **90/100** or higher;
- product: **80/80** exactly.

A high landing score cannot compensate for a weak product, and a strong implementation cannot compensate for unclear or unverifiable marketing.

### Scoring rubric

| Surface | Points | What earns the score |
| --- | ---: | --- |
| Landing: problem and outcome | 20 | The visitor recognizes their urgent job and the promised result. |
| Landing: clarity and activation | 20 | The boundary, workflow, and first action are clear without architecture archaeology. |
| Landing: trust and proof | 40 | Claims are bounded, reproducible, and distinguish live functionality from illustration/roadmap. |
| Landing: model supply and first value | 20 | The buyer knows what serves the next request, how capacity is attached, what is prohibited, what served it, and what happens when no eligible source exists. |
| Product: job coverage | 30 | Current behavior prevents the ICP's main operational failure. |
| Product: operational fit | 25 | A small team can configure, run, diagnose, and adopt it in its existing stack. |
| Product: safety and proof | 25 | Important failure modes fail closed and the behavior has a runnable proof. |

## Company ICP

A 50–500-person software company whose AI usage has crossed from individual experimentation into shared infrastructure. It has at least two of:

- multiple AI agents or teams;
- multiple model/provider accounts;
- local or remote inference capacity that is stranded or difficult to operate;
- regulated or sensitive prompts;
- direct provider keys outside a common policy path;
- spend, quota, retry, or reliability incidents discovered after the fact;
- no common trace of actor, policy decision, route, tool call, and cost.

The profiles below are real buying-committee roles, not separate feature-pain ICPs. The score gate remains a product-decision instrument, not market validation.

## Profile 1 — CTO / VP Engineering

**Who:** Economic sponsor in a mid-sized software company scaling AI agents across multiple teams or products.

**Job:** Scale AI adoption without losing control of risk, spend, infrastructure, or provider dependence.

**Current failure:** Each team adds providers, keys, and model integrations independently. Leadership learns about cost, data-egress, or availability problems after the fact.

**Alternatives:** Direct provider access; a managed AI gateway; an internal proxy; a blanket security ban.

**Must see:** A clear first-value path; attached capacity explained; hard risk and spend boundaries; inspectable route/cost evidence; a deployment that does not require replacing existing clients.

**Disqualifiers:** Stoke claims to include models, hosted provider credits, enterprise SSO/RBAC, compliance certification, or full governance where it does not.

## Profile 2 — Platform / Cloud / DevEx lead

**Who:** Technical buyer and operator responsible for the shared inference path and local/cloud capacity.

**Job:** Give agents one stable endpoint while routing across approved local, remote, and cloud capacity by health, warm state, load, and policy.

**Current failure:** Local models are stranded on individual machines; cloud fallback is accidental; provider credentials and topology are hand-maintained.

**Alternatives:** A single Ollama endpoint; static reverse-proxy rules; hand-maintained model routing; a managed model router.

**Must see:** How a model source is attached; discovered model inventory; warm/load-aware placement; health-based failover; multi-machine setup; client compatibility; observable decisions.

**Disqualifiers:** Stoke silently chooses a provider, promises local inference is free, or requires architecture archaeology before the first request.

## Profile 3 — Security / IT / Compliance lead

**Who:** Approval and risk gate for AI usage, sensitive prompts, and provider/tool egress.

**Job:** Allow useful agent workloads while proving where requests may go and refusing unapproved paths before dispatch.

**Current failure:** Direct keys, unclear fallback, incomplete identity, and missing traces make approval impossible. Teams either bypass policy or are blocked.

**Alternatives:** Blanket ban; manually maintained proxy configuration; enterprise AI gateway; direct local inference with no shared enforcement.

**Must see:** Explicit local/remote/cloud egress policy; fail-closed behavior; credential/PII hygiene; authenticated operation; route and policy evidence; honest model-supply boundaries.

**Disqualifiers:** Claims of complete DLP, MCP governance, SSO/RBAC, compliance certification, or full user tracing without shipped evidence.

## Profile 4 — Engineering Manager / AI Team Lead

**Who:** Internal champion and daily consumer responsible for shipping reliable agent features.

**Job:** Keep agents reliable and affordable while changing models/providers without rewriting every client integration.

**Current failure:** Provider outages, model churn, retry loops, and team-level spend limits become application work or incident response.

**Alternatives:** Application-side retry limits; provider alerts; direct SDK integrations; a generic observability platform; manual key rotation.

**Must see:** One compatible endpoint; a model source that can actually serve the next request; pre-dispatch loop/spend controls; predictable failover; route/cost receipt; runnable proof.

**Disqualifiers:** Stoke only observes after the provider call, claims to improve model quality, or leaves a newly installed gateway with no attached capacity and no clear next step.

## Capability evidence to label during scoring

For every profile, score the current evidence—not the intended roadmap:

- **risk control:** live/partial — auth, tier allowlists, fail-closed refusal, and PII redaction exist; enterprise IAM/DLP does not;
- **cost control:** live — budgets, rate limits, stream-aware reservations, fan-out limits, declared pricing, and refusal are smoke-tested;
- **cost optimization:** partial — cache, routing, and local/remote placement exist; broad finance reporting does not;
- **infrastructure leverage:** live — node discovery, health, warm/load-aware placement, failover, and federation exist;
- **model supply:** partial — user-attached local/remote/cloud capacity exists, but a unified attachment wizard does not;
- **user/team governance:** partial — bearer-key identity and per-key policy exist; SSO/RBAC/groups do not;
- **MCP governance:** roadmap — generic `tool_calls` passthrough is not MCP server/tool governance;
- **audit/tracing:** partial — receipts, logs, and enforcement events exist; a complete actor → policy → provider/node → tool → outcome trace does not.

The model-supply question is a scored landing criterion, not a footnote: the landing and onboarding must explain that Stoke attaches capacity and ships no hidden model catalogue. See [`control-plane-positioning.md`](control-plane-positioning.md) for the decision memo and design-partner test.

## Operating rule

Be a hard critic: missing evidence earns zero; do not infer capability from intent, roadmap, mock UI, generic architecture, or polished copy. After every material landing or product iteration, run:

```sh
python3 scripts/icp_panel.py > /tmp/stoke-icp-score.json
```

Use the lowest-scoring surface/persona's **single next change** as input to the next iteration. Do not optimize copy alone when the stated blocker is missing product behavior, and do not add product surface solely to make a score rise. Re-run the full panel after the change. The raised score gate is satisfied only when every project-supplied persona meets all three thresholds above, including an exact 80/80 product score.
