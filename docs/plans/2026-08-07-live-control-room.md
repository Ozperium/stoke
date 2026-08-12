# Live Control Room Implementation Plan

> **For Hermes:** Execute with strict red-green-refactor cycles.

**Goal:** Add a local, authenticated `/ui` that shows Stoke enforcement decisions live without a TypeScript or Node build step.

**Architecture:** The existing Axum process owns a bounded in-memory event feed. Askama renders the page and event fragments; vendored HTMX opens an SSE connection and swaps fragments into the page. UI routes use the existing fail-closed auth policy, accepting browser Basic Auth with the API key as the password while API routes remain Bearer-only.

**Tech Stack:** Rust, Axum, Askama, HTMX, HTMX SSE extension, plain CSS.

---

### Task 1: Event feed and rendering

**Files:**
- Create: `src/dashboard.rs`
- Create: `templates/dashboard.html`
- Create: `templates/dashboard_summary.html`
- Create: `templates/dashboard_event.html`

1. Write unit tests for bounded retention, counters, and HTML escaping.
2. Run the targeted tests and confirm they fail because the dashboard module is absent.
3. Implement the minimum event store and Askama views.
4. Run the targeted tests and confirm they pass.

### Task 2: Browser authentication and routes

**Files:**
- Modify: `src/main.rs`
- Modify: `src/budget.rs`
- Modify: `Cargo.toml`

1. Write tests for Basic Auth password extraction and key validation.
2. Confirm the tests fail.
3. Add `/ui`, `/ui/summary`, `/ui/events`, and embedded asset routes.
4. Keep `/health` as the only anonymous operational endpoint; return a Basic Auth challenge for unauthorized UI requests.
5. Confirm tests pass.

### Task 3: Instrument enforcement decisions

**Files:**
- Modify: `src/main.rs`

1. Add tests for event outcome classification.
2. Confirm failure.
3. Record loop/budget/rate refusals, cache hits, successful dispatches, and upstream failures.
4. Confirm tests pass.

### Task 4: End-to-end verification

**Files:**
- Modify: `scripts/smoke.sh`

1. Add assertions for the dashboard HTML, embedded HTMX asset, summary fragment, and SSE response.
2. Run the smoke assertion and confirm failure before route implementation is complete.
3. Run `cargo test`, `cargo build --release`, `./scripts/smoke.sh`, and `./scripts/smoke_federation.sh`.
4. Start Stoke locally, send real requests through the mock provider, and verify that allowed and blocked decisions appear in `/ui`.
