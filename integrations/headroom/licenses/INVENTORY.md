# Scoped runtime license inventory

This inventory covers the dependency set actually installed by the optional
worker's pinned setup command. It is not a complete inventory of all Headroom
extras or native build-time dependencies.

| Package/artifact | Version | Installed by worker | License evidence |
|---|---:|---:|---|
| `headroom-ai` official wheel | 0.37.0 | Yes; includes `headroom._core` | [`headroom-ai-LICENSE.txt`](headroom-ai-LICENSE.txt), [`headroom-ai-NOTICE.txt`](headroom-ai-NOTICE.txt) |
| Python standard library | CPython 3.11.15 | Yes | Python distribution license |

The worker uses no Python package dependencies or extras (`--no-deps`). In
particular, `ast-grep-cli` is not installed, and the excluded upstream version
`0.44.1` is never selected. The upstream NOTICE's attributions for unused
optional libraries are retained verbatim, but those libraries are not runtime
dependencies of this worker.

The wheel's native extension may have build-time or bundled third-party
obligations not separately enumerated here. Those licenses are **unknown** from
this scoped inspection; do not treat this file as a complete SBOM or redistribute
other Headroom extras without a broader license review.
