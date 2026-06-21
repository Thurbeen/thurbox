# Development

How to set up a reproducible thurbox dev environment and run the app in an
isolated sandbox.

## 1. Toolchain — the dev environment

### Recommended: Nix flake

The `flake.nix` pins the whole toolchain CI uses — the Rust toolchain (read from
`rust-toolchain.toml`), `tmux`, `shellcheck`, `bats`, Node, `cargo-nextest`,
`cargo-deny`, `cocogitto`, `just`, and the demo stack (`vhs`/`ffmpeg`/`ttyd`).

```bash
# one-time, if not done already: enable flakes
#   mkdir -p ~/.config/nix && echo 'experimental-features = nix-command flakes' >> ~/.config/nix/nix.conf

nix flake lock        # one-time: generate/commit flake.lock (pins inputs)
nix develop           # enter the pinned shell
# ...or, with direnv installed, once:
direnv allow          # auto-enters the shell on `cd` (see .envrc)
```

A couple of tools aren't packaged in nixpkgs yet (`prek`, `rumdl`, nightly
`cargo-pup`); the shell prints a hint to install them via
`scripts/install-dev-tools.sh`.

### Fallback: no Nix

```bash
scripts/install-dev-tools.sh   # cargo-binstall/cargo install the dev tools
prek install                   # install the git hooks
```

You'll also need, from your package manager: `tmux >= 3.2`, `shellcheck`,
`bats`, Node + npm (website linters), and `git`.

## 2. Everyday tasks — `just`

`just` (in the dev shell) is the task entrypoint — run `just` for the list:

| Task | What it does |
|------|--------------|
| `just build` | build the dev binaries (`thurbox` + `thurbox-cli`) |
| `just test` | `cargo nextest run --all` |
| `just lint` | fmt-check + clippy + cargo-deny + rumdl + shellcheck |
| `just fmt` | format Rust + website |
| `just arch` | architecture-rule + rustdoc checks |
| `just hooks-install` | `prek install` |
| `just smoke` | black-box TUI smoke test |
| `just sandbox*` | dev runtime sandbox (below) |

## 3. Runtime sandbox — run thurbox isolated

The sandbox runs the dev build (`0.0.0-dev` → `dev_build` cfg, which uses a
`thurbox-dev` tmux socket) with **thurbox's own config/data redirected** into the
sandbox (via `THURBOX_CONFIG_DIR` / `THURBOX_DATA_DIR`), so it never touches your
real `~/.config/thurbox` or sessions. It **keeps your real `HOME`**, so your
authenticated agent CLIs (`claude`/`codex`/`gemini`/…) work normally — and it puts
the dev `target/debug` first on `PATH`, so an agent's status hook calls *this*
`thurbox-cli` and writes to the sandbox DB the TUI reads.

```bash
scripts/dev/sandbox.sh                 # persistent "default" profile, launch the TUI
scripts/dev/sandbox.sh --fresh         # throwaway env, wiped on exit
scripts/dev/sandbox.sh --profile foo   # a named persistent profile
scripts/dev/sandbox.sh --isolate-home  # full hermetic isolation (fresh HOME; agents have NO creds)
scripts/dev/sandbox.sh --shell         # a shell with the sandbox env (run thurbox-cli by hand)
scripts/dev/sandbox.sh -- session list # run a thurbox-cli command in the sandbox
scripts/dev/sandbox.sh --clean [name]  # kill + wipe a persistent profile
```

Or via `just`: `just sandbox`, `just sandbox-fresh`, `just sandbox-shell`,
`just sandbox-clean [profile]`.

**Isolation flavors:**

- **thurbox-only (default)** — real `HOME`/agents; only `thurbox-config` +
  `thurbox-data` (+ a private `TMUX_TMPDIR`) are redirected. Use this to dev with
  your real, logged-in agents without polluting your real thurbox state.
- **full (`--isolate-home`)** — also overrides `HOME` + `XDG_*`, so the env is
  hermetic and agents boot with no credentials. This is what `scripts/demo/
  record.sh` and `scripts/dev/tui-smoke-test.sh` use (via
  `tbx_sandbox_init_full`).

**Profile lifetimes:**

- **Persistent** profiles live under `target/dev-sandbox/<profile>/` (gitignored;
  `cargo clean` or `--clean` removes them). Their tmux socket dir is kept short
  under `$XDG_RUNTIME_DIR` (AF_UNIX socket paths are length-limited, and the
  repo's `target/` path is often too long). Sessions survive across runs.
- **Fresh** (`--fresh`) is a `mktemp` dir wiped on exit — same isolation the
  demo/smoke scripts use.

The isolation logic is one helper, `scripts/dev/lib/sandbox-env.sh`, sourced by
`scripts/dev/sandbox.sh`, `scripts/demo/record.sh`, and
`scripts/dev/tui-smoke-test.sh` (one source of truth).

### Example: watch a session's status hook end-to-end

```bash
scripts/dev/sandbox.sh --shell
# inside the sandbox shell (thurbox/thurbox-cli target the sandbox):
thurbox-cli session create --name demo --repo-path "$PWD" --agent claude
thurbox-cli session signal --state blocked --session <id>   # what an agent hook does
thurbox-cli session list --json | jq '.[].name'
```
