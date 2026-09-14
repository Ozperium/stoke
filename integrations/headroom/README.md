# Optional upstream Headroom integration

This directory provides two stdlib-facing entrypoints:

- `worker.py`: binds only to numeric loopback (`127.0.0.1`), serves `GET /health`
  and `POST /compress` with exactly `{outputs:[strings]}` → `{outputs:[strings]}`.
  It calls the reviewed upstream `headroom.transforms.smart_crusher.SmartCrusher`
  with `lossless_only=True`, `with_compaction=False`, and CCR disabled. The
  upstream Rust path performs lossless JSON whitespace compaction; the worker
  accepts a result only when every non-whitespace lexeme is unchanged and the
  UTF-8 payload shrinks. Unsupported, duplicate-key, non-JSON, and ordinary prose
  values pass through byte-for-byte. JSON-in-string `output` envelopes use the
  reviewed `route_embedded_json` adapter; outer metadata is not rewritten.
- `launch.py`: foreground launcher. It starts the worker, waits for health,
  writes an owned temporary copy of the supplied TOML with:

  ```toml
  [plugins.headroom]
  enabled = true
  url = "http://127.0.0.1:PORT/compress"
  timeout_ms = 1000
  ```

  then starts the supplied Stoke binary in the user's config directory and
  stops both on exit. The source TOML is never modified. `--off` starts plain
  Stoke directly and does not load Headroom or require its venv.

## Run

```sh
python3.11 launch.py --stoke-bin /path/to/stoke --config /path/to/stoke.toml
```

`--worker-python` selects the Python executable used for the optional worker;
when omitted it defaults to the launcher's `sys.executable`. The launcher keeps
the source config basename and bytes untouched, writes an owned sibling temp
config, and passes its exact path as `STOKE_CONFIG` to Stoke. This makes an
arbitrary `--config` basename authoritative while Stoke still runs from the
user's config directory. `--off` uses the same mechanism, but changes only the
effective Headroom `enabled` value to `false`; an existing `[plugins.headroom]`
table is updated in place rather than duplicated.

No provider, auth, model, proxy, or inference settings are created here; the
supplied config is copied verbatim except for the owned plugin table. This is a
launcher, not a native Stoke CLI subcommand.

## Reproducible optional environment

The worker intentionally uses only the native extension and the reviewed Python
adapter; it does not use Headroom's optional extras or its broad package import
surface. Create a fresh environment and install the single official wheel with
its required hash:

```sh
python3.11 -m venv .headroom-venv
.headroom-venv/bin/python -m pip install --require-hashes --no-deps \
  -r requirements-optional.txt
```

The lock is tested for **CPython 3.11 on macOS arm64**. The wheel is tagged
`cp310-abi3-macosx_11_0_arm64`; other Python versions/platforms need a separately
reviewed lock entry and test. Do not install `headroom-ai` with extras. The
published metadata excludes `ast-grep-cli==0.44.1`; this worker does not install
`ast-grep-cli` at all.

Use this dedicated, unchanged environment for supported runs, and launch with
`.headroom-venv/bin/python launch.py --stoke-bin /path/to/stoke --config /path/to/stoke.toml`.
The installation hash is not runtime attestation: the launcher does not re-hash
installed code. Upgrading/replacing packages or setting `HEADROOM_UPSTREAM_SOURCE`
requires a separate review; these are not supported release configurations.

The official wheel supplies `headroom._core`. The small vendored patch under
`headroom_patches/headroom/transforms/recursive_json.py` is the reviewed
JSON-in-string envelope adapter; its provenance and SHA-256 are in
`auditroot/HEADROOM-PACKAGING-HANDOFF.md`. `HEADROOM_UPSTREAM_SOURCE` remains an optional
explicit audit override, but the default installation has no machine-local
source path. The package's Apache license and published NOTICE are retained in
`licenses/`; the inventory is limited to artifacts actually used by this worker
and does not claim a complete inventory of Headroom's unused extras.

No provider credentials, model files, telemetry stores, pilot receipts, database
files, logs, or compiled artifacts belong in this directory. `--off` is launcher
behavior and must remain stdlib-only: it must not import this worker or require
this environment.
