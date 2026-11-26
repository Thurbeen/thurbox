# Fleet's release workflows, as a fixture

Fleet's release and CI workflows from before it joined this repository. They
do not run here: GitHub reads workflows only from `.github/workflows/`, and
this repository releases thurbox from its own `cd.yml`. `fleet-dev`'s tests
hold the release tooling (`cargo xtask release`, `scripts/fleet/install-release.sh`)
to these recipes until Fleet's release joins this repository's pipeline.

## Decisions in `cd.yml`

- A failing or missing `cog` fails the `parse` step. An unknown decision is
  never "no release".
- A release needs a change to a shipped path since the last tag. A forced
  dispatch skips this check.
- Only `main` publishes, and only while `.github/release-readiness.toml` says
  `ready = true`. A false, missing or invalid file holds the release.
- A cut behind the highest tag, or a prerelease, is not marked latest. The
  installers follow "latest".
- The build matrix lists the targets `crates/fleet-dev/src/release.rs`
  packages. The tooling tests fail a list that differs.
- The build job requires `rustup` on the runner image. It never pipes a
  downloaded installer into a shell.
- The build job tests the tagged commit before it packages it, because a push
  to `main` can arrive without a pull request.
- The publish job signs SLSA build provenance for every archive and the
  checksum file. `gh attestation verify <file> --repo <owner>/fleet` checks it.
