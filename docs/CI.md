# CI and nightly builds

## One native verification per main update

Pull requests run `.github/workflows/ci.yml`. Main pushes enter
`.github/workflows/release.yml`, which calls that same CI before creating any
archives. CI has no separate main-push trigger, so main does not run the native
suite twice.

Website/installer validation and CI contract tests remain separate, inexpensive
jobs. The native jobs retain the complete core unit/smoke/security suite, the
macOS ARM64 Headroom integration, and V8/JavaScript security checks.

## Website-only updates

A reviewed allowlist covers `landing/`, `docs/`, top-level `README.md`,
`CHANGELOG.md`, `AGENTS.md`, and the two website generator/checker scripts.
Changes limited to those inputs run the website and CI-contract jobs while the
three native CI jobs and four-platform archive matrix are skipped.

Unknown inputs, Cargo files, native source, embedded root `assets/`, integrations,
workflow changes, and native test scripts require full native verification.
Renames include both the removed and added paths.

PR selection uses the PR merge-base and runs the selector from the reviewed base
commit before executing PR-controlled tests. A base without the selector forces
all native checks. Workflow changes themselves still require trusted review;
this is not an immutable external organization policy.

Main selection compares with the **last published nightly**, not merely the
previous push. A website update following an unfinished native update must still
verify and package those unpublished native changes. Missing/non-ancestor nightly
baselines and empty diffs conservatively require full native work.

## Manual verification and stable releases

Manual CI runs always execute native verification. Manual release-workflow runs
also retain full verification before packaging. Neither workflow promotes a new
stable release: `.github/workflows/promote-nightly.yml` remains the explicit
production gate, verifying the exact nightly source, successful reused CI check
names, archive inventory, manifest, and checksums.

A website-only main update intentionally leaves the nightly source unchanged.
The latest published product can therefore be an ancestor of main.

Run the CI-selection and promotion regressions locally with:

```sh
python3 -m unittest scripts.test_ci_changes scripts.test_ci_workflows -v
```

These tests exercise real temporary Git histories and the actual selection and
promotion shell snippets. They do not replace the native/security suites when
native inputs change.
