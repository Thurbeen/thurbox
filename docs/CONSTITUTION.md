# Constitution

Non-negotiable rules that define what Thurbox **must** always be. Each principle has an automated enforcement mechanism — if it can't be enforced, it doesn't belong here.

## Principles

### 1. Crash-free operation

Errors are displayed in the UI (status bar / footer), never via panics. The only panic path is the emergency terminal-restore hook in `main.rs`, which exists to leave the user's terminal in a usable state if something truly unexpected happens.

### 2. Module isolation

Dependency flow is strictly one-directional:

```
session  (no project-local imports)
claude   → session
ui       → session
app      → session, claude, ui
```

`claude` and `ui` never import each other. This keeps the side-effect layer (PTY management) completely decoupled from the rendering layer.

### 3. Zero-warning policy

Both `clippy` and `rustdoc` run with warnings promoted to errors. If it compiles with warnings, CI fails.

### 4. Permissive licenses only

All dependencies must carry licenses from the allowlist in `deny.toml`. Copyleft crates are rejected at PR time.

### 5. Zero known vulnerabilities

`cargo-deny` advisories and `cargo-audit` block merges when known CVEs affect the dependency tree.

### 6. Conventional commits

Every commit message is validated against the Conventional Commits spec by `cocogitto`. Non-conforming commits are rejected by the `commit-msg` hook.

### 7. TEA as the single architectural pattern

The Elm Architecture (`Event -> Message -> update -> view -> Frame`) is the only sanctioned control-flow pattern. No ad-hoc event handlers, no component-local state, no callback chains.

### 8. PTY-first session model

Claude Code sessions run inside real PTYs (`portable-pty`). We never mock, emulate, or screen-scrape a fake terminal. The PTY is the source of truth.

### 9. Logging never touches stdout

Stdout belongs to the TUI. All diagnostic output goes to the log file at `~/.local/share/thurbox/thurbox.log`.

### 10. Test-driven development (Red, Green, Refactor)

All features and bug fixes follow the TDD/BDD cycle:

1. **Red** — Write a failing test that defines the expected behavior.
2. **Green** — Write the minimum code to make the test pass.
3. **Refactor** — Clean up while keeping tests green.

Tests are written *before* or *alongside* the implementation, never as an afterthought. If a bug is reported, the fix starts with a test that reproduces it.

### 11. Deterministic CI — scripts over LLMs

CI pipelines must be reproducible and deterministic. Every check is a script or tool that produces the same result given the same input. LLM-generated judgments (code review bots, AI-powered linters) are never gating — they may advise, but deterministic tools (`clippy`, `nextest`, `cargo-deny`, `cog`) make the pass/fail decision. Changes to CI configuration require careful review because a broken pipeline affects every contributor.

## Enforcement Map

| Principle | Enforced by | Config file |
|-----------|-------------|-------------|
| Crash-free operation | Code review + `#[deny(clippy::unwrap_used)]` (planned) | `clippy.toml` |
| Module isolation | `cargo-pup` | `pup.ron` |
| Zero warnings | `clippy -D warnings` + `RUSTDOCFLAGS="-D warnings"` | CI + pre-commit |
| Permissive licenses | `cargo-deny check bans licenses` | `deny.toml` |
| Zero vulnerabilities | `cargo-deny check advisories` + `cargo-audit` | `deny.toml` |
| Conventional commits | `cocogitto` (`cog verify`) | `cog.toml` |
| TEA pattern | `cargo-pup` rules + code review | `pup.ron` |
| PTY-first model | Code review | — |
| Logging off stdout | Code review | — |
| TDD (Red/Green/Refactor) | `cargo-nextest` + code review | `.config/nextest.toml` |
| Deterministic CI | Scripts and tools only; no LLM-gated checks | CI config + pre-commit |
