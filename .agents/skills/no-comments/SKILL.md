---
name: no-comments
description: Write no comments in code. Use whenever you write or edit code in this repository (Rust, Lua, shell, JS, TOML, nix, CSS), and whenever you review a diff that adds a comment.
---

# No comments

Comments in code are a liability. Write none.

Express intent through names, types, small functions, tests and module
structure. When you touch code, delete the comments near your change unless
they fall under an exception below.

## Exceptions

- Legal or license headers.
- Non-obvious behavior forced by an external dependency, platform, vendor, or
  protocol we cannot reshape, including `// SAFETY:` notes that justify an
  unsafe block.
- Lint suppressions, such as `// prettier-ignore`, only when their rule is
  faulty, pedantic, or style-only.
- Directives a tool reads: generated-region markers, type annotations, and
  shebangs.
- Doc comments that define a public API contract.
- Issue or RFC links that explain a constraint code cannot express.

## MUST KILL

When our own code needs explanation, replace the comment with one marker on the
exact symbol. Surprises in our own code are not an exception: kill them with a
rename, extract, type, or rearchitecture that makes the behavior obvious
without prose, and until then mark the symbol:
`MUST KILL: <rename|extract|type|rearchitect> — <what would make it obvious>`.

```rust
// MUST KILL: type — a `Seconds` newtype would make the unit obvious
fn ttl(v: u64) -> u64 { v }
```

```lua
-- MUST KILL: rename — `pending_since` says what `t0` hides
local t0 = os_clock()
```

```sh
# MUST KILL: extract — `wait_for_socket` would name this retry loop
for _ in 1 2 3; do sleep 1; done
```

## Examples

A "why" comment becomes a name:

```rust
// before: retry because the socket may not exist yet
let n = 3;
// after
let socket_connect_attempts = 3;
```

A comment becomes a function:

```rust
// before: drop sessions that are dead and older than a day
sessions.retain(|s| !(s.dead && s.age > DAY));
// after
sessions.retain(|s| !s.is_expired());
```

A platform quirk stays, with the external fact in one line:

```rust
// tmux < 3.3 rejects `extended-keys-format`, so set it best-effort.
let _ = set_option("extended-keys-format", "csi-u");
```

## Review

Reviewers flag any new comment outside the exceptions: thurview reads
`docs/thurbox/review.md`, which points here.
Formatting is automatic: let `cargo fmt` and `stylua` wrap and indent.
