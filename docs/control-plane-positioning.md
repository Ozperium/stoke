# Stoke control-plane positioning

Status: decision memo from the Requesty and ICP discussion.

## Decision

Pursue Stoke as a **self-hosted control plane for AI capacity and agent traffic**.

Do not turn Stoke into a managed 600-model catalogue at this stage.

The first product promise is:

> Connect the local models, remote nodes, and approved provider accounts you already use. Stoke decides what may serve each request, where it runs, what it may cost, and what happened.

This keeps Stoke distinct from a managed model gateway while answering the first-use question: **what will serve my next request?**

## What Requesty validates

Requesty is evidence for a broader paid category: operational control of organizational AI usage. Its public product and customer stories repeatedly describe:

- one gateway replacing direct provider access and scattered keys;
- model/provider choice without application rewrites;
- regional and provider policy;
- unified spend, budgets, and billing;
- routing, fallback, caching, and availability;
- audit logs, session visibility, and tool-call analytics;
- MCP Gateway as an adjacent control surface.

Public sources reviewed:

- [Requesty product](https://www.requesty.ai/)
- [Requesty pricing](https://www.requesty.ai/pricing)
- [Requesty customers](https://www.requesty.ai/customers)
- [ZoomInfo case](https://www.requesty.ai/customers/zoominfo)
- [anwalt.de case](https://www.requesty.ai/customers/anwalt-de)
- [Requesty security](https://www.requesty.ai/security)

The strongest recurring customer problem is:

```text
provider and model sprawl
  → no common visibility
  → direct keys and uncontrolled egress
  → cost and reliability incidents
  → a need for one policy and trace plane
```

### Evidence quality

Requesty names customers and publishes quantified usage and outcomes. That is stronger than generic category copy, but it remains vendor-published evidence. It does **not** establish Requesty's ARR, retention, total market size, or independent verification of every performance claim.

The evidence supports:

- problem evidence: strong;
- format/category evidence: strong;
- commercial evidence: medium — named usage and a paid markup are signals, not independently verified revenue or retention;
- market-size evidence: unavailable from the reviewed pages.

The demand is concentrated after AI usage becomes organizationally distributed: multiple teams or agents, multiple providers/models, meaningful spend or regulated data, or infrastructure that must be shared and governed.

## Stoke versus Requesty

These products operate at overlapping but different layers:

| Dimension | Requesty | Stoke |
|---|---|---|
| Primary offer | Managed AI gateway and provider access | Self-hosted policy and capacity control plane |
| Model supply | Hosted catalogue and provider aggregation | User-attached local, remote, or approved cloud capacity |
| Billing | Provider usage plus Requesty markup | Operator-declared pricing and local/remote capacity accounting |
| Deployment | Requesty-managed service | One Rust binary on infrastructure the operator controls |
| Main decision | Which provider/model should serve the request? | Is this request allowed, and which approved capacity should serve it? |
| Local infrastructure | Adjacent/available through integrations | Core product surface: health, warm state, load, federation |
| MCP | Publicly marketed MCP Gateway | Future tool-governance layer; not shipped as control-plane behavior today |

Stoke must not claim to have Requesty's model catalogue, hosted billing, SSO/RBAC, or MCP governance unless those capabilities are actually implemented and verified.

## The model-supply objection

The buyer's question is valid:

> “I will install a control plane, but how do I attach models to it? Does it have models like the competitors?”

The honest current answer is:

> Stoke ships no models or provider credits. It attaches capacity you already own or explicitly approve: local Ollama models, remote/federated Stoke or Ollama nodes, and configured OpenAI-compatible or Anthropic endpoints. A metered model must have an operator-declared price; otherwise Stoke refuses it rather than pretending the spend is zero.

This is a good security and cost boundary, but the current onboarding is too configuration-oriented. `stoke setup` currently guides local Ollama setup; cloud and federation are available through configuration and documentation rather than a unified attachment flow.

### Required first-value experience

The control plane must expose **Attached capacity**, not only a blank model list:

```text
1. Attach capacity
   - local Ollama
   - remote/federated Stoke or Ollama
   - approved OpenAI-compatible endpoint
   - Anthropic endpoint

2. Select the default model from discovered or explicitly declared inventory.

3. Select the egress policy
   - local/remote only
   - local preferred with explicit cloud fallback
   - approved cloud only

4. Send one request and show
   - model
   - provider/node
   - policy decision
   - tier/egress result
   - cost or declared local/remote accounting
```

If no capacity is attached, the product must say so plainly and fail closed. It must not imply that Stoke supplies a hidden default model.

### Managed capacity is a separate decision

A future “connect managed capacity” option is possible, but it creates a different business:

- provider contracts and pricing updates;
- hosted data-processing and regional commitments;
- billing and credits;
- availability and support obligations;
- direct competition with Requesty and OpenRouter.

Do not add a catalogue merely to remove onboarding discomfort. Test whether missing model supply blocks qualified pilots first.

## Role-based ICP

### Company condition

A 50–500-person software company whose AI usage has crossed from individual experimentation into shared infrastructure. It has at least two of:

- multiple AI agents or teams;
- multiple model/provider accounts;
- local or remote inference capacity that is stranded or difficult to operate;
- regulated or sensitive prompts;
- direct provider keys outside a common policy path;
- spend, quota, retry, or reliability incidents discovered after the fact;
- no common trace of actor, policy decision, route, tool call, and cost.

### Buying committee

| Role | Job | Trigger | Gain | Evidence/metric |
|---|---|---|---|---|
| CTO / VP Engineering | Scale AI adoption without losing risk, spend, or infrastructure control | rollout, incident, or provider sprawl | common control without forcing every team to rewrite clients | rollout time, spend variance, incidents |
| Platform / Cloud / DevEx lead | Operate the shared inference path | multiple agents, providers, or nodes | one endpoint, attached capacity, routing, health, and credentials | integration time, availability, utilization |
| Security / IT / Compliance lead | Approve where prompts and tool calls may go | review, regulated workload, or data-egress concern | explicit allow/deny, fail-closed routes, redaction, audit evidence | unauthorized paths, trace coverage |
| Engineering manager / AI team lead | Ship reliable agent features without infrastructure work | provider outage, model churn, or team friction | stable client integration, predictable budgets, model choice | success rate, latency, support load |
| FinOps influencer | Attribute and constrain usage | invoice sprawl or budget pressure | team/project attribution and hard limits | variance, cost per workload |

FinOps is optional as a distinct role. It should not become a separate ICP when the CTO or platform lead owns the budget.

## Capability truth table

| Domain | Stoke status | Honest wording |
|---|---|---|
| Risk control | Live/partial | Auth, pre-dispatch route policy, tier allowlists, fail-closed refusal, and built-in PII redaction are live. Enterprise IAM/DLP is not. |
| Cost control | Live | Per-key budgets, rate limits, stream-aware reservations, fan-out ceilings, declared pricing, and pre-dispatch refusal are live and smoke-tested. |
| Cost optimization | Partial | Cache, routing, warm/load-aware placement, and local/remote capacity are live. Broad provider price optimization and finance reporting are not a complete product. |
| Infrastructure leverage | Live | Ollama discovery, node health, warm state, load-aware placement, failover, and Stoke federation are live. |
| Model supply | Partial | User-attached capacity and local discovery are live. A unified cloud/federation attachment wizard is not. |
| User/team governance | Partial | Bearer-key identity and per-key policy exist. SSO, enterprise RBAC, and organizational groups are not shipped. |
| MCP governance | Roadmap | Tool-call passthrough exists where supported; that is not MCP server/tool governance. |
| Audit/tracing | Partial | Route/cost receipts, logs, and enforcement events exist. A complete actor → policy → model/provider/node → MCP/tool → outcome trace is not shipped. |

## Product and landing rules

Use one control-plane story with role-specific outcomes, not separate products for security, local capacity, and spend.

Lead with:

> Control AI risk, cost, and capacity before the provider call.

Immediately answer model supply:

> Stoke does not sell a hidden model catalogue. Attach your local Ollama models, remote capacity, or approved provider account; Stoke makes the route and egress policy explicit.

Do not claim:

- that Stoke includes models;
- that local inference is free;
- that Stoke provides Requesty-style hosted provider access;
- that generic `tool_calls` support equals MCP governance;
- that key-prefix logs equal full user tracing;
- that SSO/RBAC or enterprise governance is available today.

## Design-partner test

Test the narrowed thesis with five mid-sized software companies across the four roles.

For each company, ask them to bring one real existing capacity source and one real agent/client. The test is complete only when the participant can:

1. identify the source that should serve the request;
2. attach or configure it without reading the entire architecture documentation;
3. set a local/remote/cloud policy;
4. send a request through Stoke;
5. explain the resulting route, policy decision, and cost boundary.

Proceed with an attachment-flow build if at least three of five qualified teams complete that path and at least two request a continued pilot or agree to run Stoke on a second real workload. Narrow or stop the model-supply expansion if fewer than two can name a concrete attached source or if the recurring request is instead for a hosted model catalogue and unified billing. That would be evidence for a managed-gateway product, not automatically for Stoke's self-hosted wedge.

The panel score is a product-decision instrument only. Role re-basing and the model-attachment experience must be scored against the current artifact before claiming a new gate result.
