# Fleet's release workflows, as a fixture

Fleet's release and CI workflows from before it joined this repository. They
do not run here: GitHub reads workflows only from `.github/workflows/`, and
this repository releases thurbox from its own `cd.yml`. `fleet-dev`'s tests
hold the release tooling (`cargo xtask release`, `scripts/fleet/install-release.sh`)
to these recipes until Fleet's release joins this repository's pipeline.
