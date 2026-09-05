# ADR 0001: Durable budget ledger and "hard cap" crash semantics

- Status: Accepted
- Date: 2026-09-05
- Deciders: Stoke maintainers (security release B, Selsey retest gate)

## Context

Stoke's budget enforcement is currently **process-memory only** (`BudgetGuard`
holds `spend`, `reserved`, `estimated`, rate-limit timestamps, and
`loop_blocked` in `HashMap`s). A restart resets every key's cumulative spend to
$0.00, so a client who can crash or restart the gateway can spend its cap again
— indefinitely, since crash+restart is free. The Selsey security review called
this out (KRYT-2): a cap that resets on restart is not a cap.

Making spend survive restarts changes operational semantics, so the crash
behavior needs an explicit decision, not a default.

## Decision

### 1. The ledger is SQLite (embedded), reservations are transactions

We adopt `rusqlite` (bundled SQLite, no runtime dependency — the binary
dependency-free invariant holds) as the durable ledger, in the gateway process.
The database lives at `~/.stoke/ledger.db` by default and is overridden with
`STOKE_LEDGER_PATH` for tests and multi-instance operators.

Core operations, each in a single SQLite transaction:

| Operation | Meaning |
|---|---|
| `reserve(key_id, amount)` | insert an open reservation row (hold) — fail-closed on any DB error |
| `charge_and_release(key_id, amount)` | atomically add to `spend`, delete the matched reservation row |
| `release(key_id, amount)` | delete reservation rows up to amount (hold given back) |
| `snapshot(key_id)` | durable spend + open reservations |
| `list_unresolved()` | keys with open reservations (operator reconciliation) |
| `operator_reconcile` | delete or charge an unresolved reservation after a crash |
| `reset_period(key_id)` | zero the spend row (operator-initiated only) |

### 2. Crash semantics: "hard" means fail-closed, not auto-forgiven

- A reservation is **written before the provider call** (it already is in the
  in-memory design; now it is durable first).
- On completion, `charge_and_release` atomically converts the hold into spend.
- After a crash, an **unresolved reservation stays open**. The key's committed
  figure includes open reservations, so the money stays held — the gateway
  restarts *more conservative than it crashed*, never less. Auto-expiring stale
  holds is explicitly rejected: it re-opens the over-cap window after every
  crash, which is the bug this ADR exists to close.
- The operator reconciles unresolved holds explicitly (CLI `stoke ledger
  reconcile`). Without reconciliation, an over-cap key keeps refusing — that is
  the documented meaning of "hard cap".

**Documented claim boundary:** with this design Stoke may say "durable
gateway-enforced cap" and must always state the crash semantics next to it
(holds persist, operator reconciles, provider-side spend limits remain the
outer boundary). The word "hard" alone is not permitted in user-facing copy
without that description.

### 3. Keys are identified by `key_id`, never by raw secret

The ledger stores only the key **id**. Raw secrets never touch the database:
not in spend rows, not in reservation rows, not in logs. Each configured
`[[keys]]` policy has a stable `id`; changing the secret while keeping the `id`
preserves spend. A key policy without `id` derives a stable id from a keyed
hash (HMAC-SHA-256 with a per-installation random salt stored in the DB, never
the secret itself) so spend survives config reordering and secret rotation even
when the operator never named the key.

### 4. Fail-closed on storage failure

Any ledger error (corruption, read-only file, full disk) **blocks metered
requests before the provider call** — the same place budget enforcement
already runs. The gateway does not degrade to memory-only mode, because that
would silently restore the restart-bypass this ADR removes. `/health` reports
ledger status so operators see the refusal cause.

### 5. What else survives a restart

Restart must not bypass the other enforcement mechanisms:

- **Rate limit:** the rolling window stores timestamps; the durable ledger
  stores the count within the trailing 60s bucket (per `key_id`). In-flight
  requests at crash time are lost — accepted, conservative.
- **Loop breaker:** an *active* block persists (expiry timestamp as a keyed
  row). Prompt history is keyed hashes + timestamps only — never raw prompts,
  never semantic embeddings (persisting embeddings is deferred pending a
  privacy analysis; after restart, an active block stays active, history
  rebuilds as traffic flows).

### 6. What does not change

- Keys without a budget (`budget_usd` absent) stay unlimited and take no
  reservations — the ledger records nothing for them.
- Subscription (zero-marginal) traffic stays outside the dollar ledger, as
  already shipped.
- The `/v1/budget` response shape gains `key_id` and durable status fields;
  raw key prefixes stop appearing in operator-facing responses.

## Consequences

- +~1.5 MB binary size and fsync cost per admission on metered traffic.
  Accepted: correctness of the cap is the product.
- A corrupted DB takes the metered path down rather than silently unenforcing.
  This is intended behavior and is documented in the ADR and the README.
- Operators get `stoke ledger status` and `stoke ledger reconcile` for the
  post-crash workflow.
- Migration: first boot with an existing config derives `key_id`s and seeds the
  ledger from zero (spend history from before this release does not exist).
  This is called out in the release notes, not silently assumed.

## Alternatives considered

- **Auto-expiry of stale holds** — simplest, rejected: re-opens the over-cap
  window after every crash; the cap becomes advisory.
- **File-append JSON journal** — no concurrent-writer safety, no atomic
  read-modify-write, weaker than SQLite's WAL for the reservation/charge
  race. Rejected.
- **Full Postgres/Redis** — violates the zero-runtime-dependency invariant.
  Rejected.
- **Keep memory-only, document as advisory** — defeats KRYT-2; the Selsey gate
  requires "durable" to mean durable. Rejected.

## Verification plan (release B gate)

1. spend survives a normal restart;
2. SIGKILL after a durable reserve leaves the hold after restart;
3. secret rotation with the same `key_id` preserves spend;
4. corruption / read-only / full-disk paths fail closed;
5. two concurrent reserves cannot cross the limit;
6. the raw key never appears in the DB;
7. an active loop block and the current rate-limit window survive restart.