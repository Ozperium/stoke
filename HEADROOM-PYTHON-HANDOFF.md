# Headroom Python boundary handoff

Scope: `integrations/headroom/worker.py`, `launch.py`, and their Python tests.

## Runtime contract

- The launcher generates one fresh `STOKE_HEADROOM_TOKEN` with
  `secrets.token_urlsafe(32)` for each enabled run.
- The token is passed to both the worker and Stoke. The worker accepts only the
  exact `Authorization: Bearer <token>` value on `/health` and `/compress`.
  Tokens are never included in worker responses or logs.
- The worker binds `127.0.0.1` and, with `--port 0`, owns ephemeral-port
  selection. It writes exactly one ready line to the inherited `--ready-fd`:
  `{"port":<actual-bound-port>}`. The launcher validates that message, then
  performs an authenticated health check before starting Stoke; it does not
  reserve a free port itself.
- The HTTP server is single-threaded. Per-connection socket reads have a five
  second timeout.
- POST bodies must have one decimal `Content-Length`, no `Transfer-Encoding`,
  and be at most 1 MiB. Duplicate/malformed framing fails closed. The JSON
  envelope rejects duplicate keys and must be exactly `{outputs: [...]}` with
  at most 128 string outputs. Payload/parser failures return generic errors and
  do not expose exception text.
- Launcher worker environments are credential-minimal and offline-oriented;
  Stoke retains its inherited environment. The source TOML is immutable in both
  enabled and `--off` modes. The launcher owns and removes a temporary config,
  and signal/failure cleanup stops both children.
- `--off` does not import the worker or Headroom and does not validate/start a
  worker. Worker interpreter selection is checked for executable permission and
  a successful Python version probe (Python 3.10+).

## Rust integration handoff

The Rust client must send the same per-run token as
`Authorization: Bearer $STOKE_HEADROOM_TOKEN` and stay within the same 1 MiB
request and 128-output bounds before calling `/compress`. The endpoint contract
is local, numeric IPv4 loopback only, authenticated health/compress, and
pre-request filtering of eligible tool-result outputs; this worker does not
implement streamed provider-response rewriting.

## Packaging coordination

The worker's existing top-level `UPSTREAM` import/path strategy remains
intentionally untouched for the packaging/dependency worker. That worker must
supply the package-relative upstream source/dependency strategy and remove the
remaining external-source assumption before release. The launcher default now
points at the package-relative `integrations/headroom/upstream` location.

## Verification

Focused pilot-venv run: **15 tests passed** across `test_worker.py` and
`test_launcher.py`, including auth, duplicate keys, framing, size/output caps,
read timeout, ready-port startup, interpreter validation, crash cleanup, signal
cleanup/port release, config immutability, and `--off` behavior.
