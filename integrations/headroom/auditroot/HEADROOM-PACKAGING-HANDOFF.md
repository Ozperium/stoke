# Headroom packaging handoff

## Result

The optional worker now installs the official `headroom-ai` 0.37.0 macOS arm64
ABI3 wheel with a SHA-256 pin and `--no-deps`. The worker's top bootstrap loads
only the native extension, the reviewed Python modules, and the one vendored
adapter patch. It avoids executing Headroom's broad eager package `__init__`, so
this worker has no Python dependency beyond the standard library and the pinned
wheel.

Portable setup command (tested only on **macOS arm64, CPython 3.11.15**):

```sh
python3.11 -m venv .headroom-venv
.headroom-venv/bin/python -m pip install --require-hashes --no-deps \
  -r requirements-optional.txt
```

The lock is intentionally platform-specific because the official native wheel
is platform-specific. Add another wheel/hash and run the same clean-room smoke
test before claiming support for another OS, architecture, or Python version.

## Upstream provenance and reconciliation

- Published artifact: official PyPI `headroom-ai` 0.37.0 wheel, filename and hash
  are in `artifact-hash-manifest.json`.
- Pilot `direct_url.json` recorded the same wheel hash:
  `b4392f68a8d02d74c62c1734cf5bf327511dcc72678f01669f44f0612944d59c`.
- Reviewed source location supplied for the audit was not a Git checkout; no
  upstream commit identifier was available. This is recorded as **commit:
  unavailable**, not guessed.
- Comparing the supplied reviewed source snapshot with published 0.37.0 found
  three changed Python files: `providers/codex/endpoints.py`,
  `proxy/tool_schema_compaction.py`, and `transforms/recursive_json.py`.
  `transforms/smart_crusher.py` was byte-identical to the published wheel.
- Only `transforms/recursive_json.py` is needed by this worker: it adds the
  reviewed JSON-in-string envelope route with duplicate/non-finite rejection,
  data-equivalence validation, and byte-preserving outer metadata splice. The
  local patch is byte-for-byte the reviewed source file; it is not a replacement
  compressor or a new framework.

## Runtime/license inventory

Actually installed by the tested command:

| Component | Version | Runtime role | License evidence |
|---|---:|---|---|
| `headroom-ai` | 0.37.0 | Python modules plus `headroom._core` native extension | `licenses/headroom-ai-LICENSE.txt`, `licenses/headroom-ai-NOTICE.txt` |
| Python standard library | CPython 3.11.15 | worker HTTP/JSON/bootstrap | Python distribution license, not vendored |

No Headroom extras, `ast-grep-cli` (including compromised 0.44.1), model files,
telemetry stores, or third-party Python dependencies were installed in the clean
environment. The published NOTICE names optional libraries that are not part of
this worker's runtime and remains preserved for attribution. This is a scoped
worker inventory, not a complete license inventory of the unused Headroom
extras or of every native build-time dependency; any such unenumerated native
license obligations remain **unknown** and must be audited before redistributing
those components.

## Verification evidence

- Fresh clean venv: `pip install --require-hashes --no-deps -r
  requirements-optional.txt` succeeded; installed package list contained
  `headroom-ai==0.37.0` plus venv tooling only.
- Actual worker compressor in that clean venv compressed a JSON array, reduced
  UTF-8 bytes, and preserved the exact non-whitespace lexeme stream including
  string whitespace and escapes.
- The same worker preserved outer bytes around a JSON-in-string `output` envelope
  while replacing only the inner JSON string.
- Plain prose passed through unchanged.
- Launcher `--off` ran with the clean venv (no optional imports) against a
  temporary valid TOML and `/usr/bin/true`, returning exit code 0. A system
  Python 3.9 check was blocked by its missing stdlib `tomllib`; Python 3.11 is
  the declared tested interpreter.

`auditroot` contains manifests and this handoff only. Do not ship private pilot
receipts, databases, logs, or compiled artifacts from the audit environment.
