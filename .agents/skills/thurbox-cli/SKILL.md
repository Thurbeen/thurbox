---
name: thurbox-cli
description: The thurbox-cli binary: its subcommands (editor, config, version, update, notify and notify send, perf, plugin, ui, schema, doctor, migrate), its output formats and exit codes. Use when changing or driving thurbox-cli. Sessions are Fleet Tasks, driven with `fleet`.
---

# thurbox-cli

*Working reference indexed by `AGENTS.md`. The rationale behind these decisions is owned by the docs under `docs/`; a change that invalidates what this says updates it in the same PR.*

## What it drives

Sessions are Fleet Tasks (ADR-41); their command line is `fleet`, not
thurbox-cli. thurbox-cli covers the interface's own files and a running interface:

| Subcommand | What it does |
| --- | --- |
| `editor get/set/mode` | the editor `Ctrl+O` opens, in `interface.json` |
| `config validate/show` | strict parse of every config file; the effective configuration and where each value comes from |
| `version [--check]`, `update [--force]` | the running version; the newest release of this major line |
| `notify [--test]`, `notify send` | which desktop-notification backend would deliver, and a sample; deliver one notification, with an optional `--key`/`--min-interval` dedupe |
| `perf [--plugins]` | the perf snapshot a running interface publishes to `perf.json` |
| `plugin …` | interface plugins: where they live, install, sync, check |
| `ui …`, `schema` | bounded requests to a running interface (`src/ui_control.rs`) |
| `doctor` | each Fleet `settings.toml` lists (`[fleets.<name>]`, or `fleet` on this machine): reachable, version, harnesses and places; the multiplexer when a Fleet runs here; the interface's telemetry export from `[telemetry]` and the OTEL_* variables, with where each value comes from; and a warning for a left-over `hosts.toml`, which nothing reads. Every check that is not ok carries a `fix` (JSON) and a `fix:` line |
| `migrate` | thurbox 2.x state into Fleet: reads `thurbox.db` with the system `sqlite3 -readonly -json` (no SQLite in thurbox's tree), applies each live session as a `dispatch: false` Task and each UTC `cron` spawn automation as a suspended CronTask through `fleet apply --create-only`, each labelled with its 2.x id so a second run skips it; removes retired `settings.toml` keys (keeping `settings.toml.v2`); prints `hosts.toml` as `[runtimes.mux.places]` tables and `agents.toml` as harness profiles, never writing Fleet's config; writes `thurbox.db.migrated`, which silences the startup notice. `--dry-run` changes nothing |

## Output and exit codes

- Human text in a terminal, TOON down a pipe; `--json`, `--pretty`, `--toon`
  and `--text` force one. `--fields` picks a list's columns; `--full` keeps
  long text whole.
- stdout carries exactly one document. A command whose report is the answer
  and still fails (`config validate`) prints the report and exits 1, with the
  reason on stderr.
- A failure that printed nothing is a structured error on stdout:
  `{"error", "error_kind", "suggestion"}` and `help` next steps.
- Exit codes: `0` success, `1` ran and failed, `2` usage (clap's own
  wording), `3` ambiguous (several running interfaces and none chosen).
- A bare `thurbox-cli` prints its help.

## Where the code lives

`src/bin/thurbox-cli.rs` is the entrypoint; `src/cli/mod.rs` parses and
dispatches; each subcommand is one file under `src/cli/`.
