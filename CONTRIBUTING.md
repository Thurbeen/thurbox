# Contributing

Fleet uses a Rust workspace and one repository-only command for validation:

```sh
cargo xtask check
cargo xtask check --fix
cargo xtask check --list
```

Required gate tools:

- Rustup and the exact toolchain in `rust-toolchain.toml`.
- cargo-nextest, cargo-deny and rumdl at the versions CI pins.
- cocogitto (`cog`) at the version the pinned `cocogitto-action` installs; the
  release tooling tests run it.
- Git for tracked-file checks, and `prek` for the commit hooks.
- Python 3.11 or newer with `tomllib` on PATH. Release workflow fixtures execute
  the same TOML readiness check as publication. CI selects Python 3.13.17.

Install hooks with `prek install`. Hooks invoke focused areas of the same
gate; publish runs the full gate. Missing tools or a missing workspace fail
instead of skipping checks. Use `--locked` for builds and tests; commit
`Cargo.lock`.

## Validation

The gate checks Markdown and local documentation links, parses tracked YAML,
validates every workflow's trust and aggregation rules, then runs rustfmt,
Clippy on all targets and features with warnings denied, nextest, doctests,
warning-denied rustdoc, and cargo-deny's advisory, license and source policy.
The nextest listing and run use the same `ci` profile. At least one active
test must run in each required `architecture*` and `e2e_*` suite family;
ignored or filtered tests do not satisfy the requirement.

Tests belong next to the crate they exercise. Use isolated config, data,
processes and bounded fake tools. The store suite covers concurrent writers,
leases, crash recovery and Unknown observations. The CLI suites drive the
built binary through resource lifecycles. The architecture suite checks the
allowed graph, purity and status writers, and mutates fixtures to prove each
rule detects a forbidden edge. Tooling's E2E suite runs the actual gate against
a tiny workspace with deleted, ignored and filtered suites, failing tests,
failing doctests, malformed formatting and warning-denied lint/docs.

Native CI runs the full gate on Linux x86_64, Linux aarch64 and macOS
Apple Silicon (`macos-15`). README.md lists the release targets and what
each has passed. Every release runner must have its Rust checks in CI. Do not
duplicate a full gate for a reviewer when exact-tree evidence is already
available.

## Review rules

Read `AGENTS.md` and `crates/CONTRACTS.md`. Keep pure controllers free of I/O,
status fields owned, Unknown explicit, leases fenced, effect intents durable,
and resolved secrets off disk and argv. All process execution uses fleet-exec.
A new crate needs a graph entry and meaningful tests. Avoid unrelated cleanup.

Work from a branch in an isolated worktree. Publish through the `publish`
skill after normal signed commits and independent local exact-SHA review.
CI and independent current-head review must pass before any authorized merge.
Squash merge only; use a conventional PR title without a duplicate `(#N)`.
Local-only tasks deliver commits and handovers without pushing.

Never publish operator data or machine identity. Keep running deployments
untouched. The deliberate cutover procedure belongs in README.md; changing
source is not authorization to deploy it.

## Workflows and release

All actions use full commit SHAs. Contributor text reaches scripts through
environment variables, never GitHub expression interpolation in `run`.
All Checks runs `if: always()` and requires success from every declared job;
skipped, cancelled, failed or missing results fail. PR Title reruns on edits.
The release workflow uses bash with pipefail and a protected `release`
environment, and tests the tagged commit before packaging it. Release jobs
restore no pull-request caches.

Releases follow the thurbox pattern, driven by cocogitto and `cog.toml`.
Every push to `main` runs the release workflow:

1. `check-release` asks `cog bump --auto --disable-bump-commit --dry-run`
   for the next version.
   `feat`, `fix`, `perf` and breaking changes bump. The squash-merged pull
   request title is the commit that decides.
2. The same job releases that version only when shipped files changed since
   the last tag, the ref is `main`, and the readiness file says
   `ready = true`. A false, missing or invalid file holds the release.
3. `create-tag` runs `cog bump` and pushes the version tag.
4. Every release runner checks out that tag, tests it, builds, packages,
   installs its archive and runs offline first-run acceptance.
5. `generate-changelog` writes the release notes with `cog changelog`.
6. `publish` attests the artifacts and creates the release on the tag.

A missing or failing `cog` fails the workflow; it never means "no release".
A commit that already carries a version tag releases nothing. A dispatch on
`main` with an explicit `version` cuts that version. Prereleases are marked
as such and never marked latest. No person pushes a version tag, and no tag
moves. A tag whose build fails stays without a release; the next releasable
push takes the next version. `workspace.package.version` stays `0.0.0-dev`;
the build stamps the tag's version into the binary. The protected release
environment and provenance attestation remain required. No release step
installs onto an existing deployment.

On a `main` checkout, `cog bump --auto --disable-bump-commit --dry-run` shows
the next version; `cog.toml` allows bumps on `main` only.

```sh
cargo xtask release title 'feat: resource orchestration' 123
cargo xtask release smoke --binary target/release/fleet --version v1.2.3
cargo xtask release package --target x86_64-unknown-linux-musl --version v1.2.3 --binary target/release/fleet --out dist
cargo xtask release checksums --version v1.2.3 dist
```

Packaging writes deterministic archives with the binary and LICENSE; the
pinned VEGA starter, its license and provenance are embedded in that binary.
Release assets also include the installer and the VEGA manifest/provenance.
Checksums require every supported target and cover those metadata assets.
Smoke requires the exact binary version; `--first-run-dir NEW_DIRECTORY` also
runs `init` with an empty PATH and isolated native config/data homes, then
requires the starter, license and provenance. Offline installer tests cover
clean install,
update, lock refusal, checksum failure, version mismatch, and uninstall.
No dry run creates a tag, uploads production artifacts, or invents credentials.

`.sonarcloud.properties` describes source and test roots for automatic
analysis; it excludes private state and generated output. Keep automatic
analysis as the single analysis path. Cargo lock maintenance and pinned
Actions/toolchain updates belong to Renovate.

## Documentation ownership

README.md owns installation, resource examples and deliberate cutover.
AGENTS.md owns working rules. This file owns contribution and release gates.
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) owns the overall structure and
boundaries.
[docs/migrating-from-v1.md](docs/migrating-from-v1.md) owns the mapping from
v1 concepts, configuration and commands to v2.
[docs/controllers](docs/controllers/README.md) owns controller resources, state
machines and interfaces. `crates/CONTRACTS.md` owns integration
guarantees and implementation coverage; `crates/architecture.toml` is the
executable graph policy. Keep each claim in its owning document and update
stale links when moving a file.
