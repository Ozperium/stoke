# Cache identity

Stoke caches only deterministic, non-streaming `single` requests. The exact key
hashes the caller scope, resolved model, and the complete serialized effective
request. Message roles, boundaries, and forwarded request fields therefore stay
part of exact identity; changing `tools`, `tool_choice`, `response_format`,
`reasoning`, `seed`, or `max_tokens` does not reuse an exact entry.

The original eligibility gate remains in force: temperature above `0.01`, a
missing/empty `messages` array, or messages whose extracted text is empty are
not cached. This intentionally bypasses all-multimodal/non-text message shapes
rather than inventing a lossy prompt identity.

Semantic matching is opt-in (`STOKE_SEMANTIC_CACHE` plus
`STOKE_EMBED_MODEL`). Its identity hashes the same request metadata with only
string message `content` removed. The role, message metadata, model, scope, and
all other request settings must still match. Unsupported message shapes return
`None` and bypass semantic embedding and matching. Exact lookup always runs
first. Embedding generation is gated on a supported semantic identity, so an
unsupported request never calls Ollama for an embedding.

## Verification commands

From the repository root:

```sh
cargo test --offline --locked cache::scope_tests -- --test-threads=1 \
  > /tmp/cache-identity-focused.log 2>&1
CARGO_BUILD_JOBS=2 cargo test --offline --locked \
  > /tmp/cache-identity-full.log 2>&1
STOKE_BIN="$PWD/target/debug/stoke" python3 scripts/smoke_cache_identity.py
```

The smoke harness uses only a local stdlib mock provider. It checks one
identical same-caller hit, distinct role/message-boundary requests, and a
`max_tokens` change. It is a wire identity check, not an inference, cost, or
savings benchmark.

## Verification receipt

- Focused cache tests: exit `0`; `13 passed`, `0 failed` (other targets were
  filtered).
- Full suite: exit `0`; target results were `63`, `245`, `2`, `6`, and `0`
  passed, with `0 failed` in each target.
- Binary smoke: exit `0`; identical pair produced `hit` with one provider call,
  both negative cases bypassed cache, and the final mock provider call count was
  `4`.
- The earlier parent RED collision is recorded separately in
  `/Users/pawloz/projects/stoke-efficiency-1-7/cache-identity-red.log`; this
  receipt reports only the current focused/full/smoke GREEN results and does not
  claim a new RED→GREEN run by this worker.
