# v2-plugin-commands — Design

## Context

See `proposal.md` — Why. The constraints that shape this are the kernel's own,
and all four bite here:

- **Reads are snapshots, writes are commands.** Lua cannot call anything that
  waits. A program that takes 400 ms is four hundred milliseconds a plugin
  cannot be allowed to hold.
- **Anything touching the world runs on a worker.** There are seven already —
  terminal attach, commands, diffs, metrics, git stats, repository reads, update
  checks. This is the eighth, and it should look like the others rather than
  inventing a shape.
- **Capabilities are absent rather than blocked.** Lua has no `os`, no `io`, no
  `package`, no loaders; `thurbox.yml` declares that absence and the linters
  enforce it. This change cannot honour that rule literally — a capability that
  is absent cannot be used — so it has to be honoured in the only way left:
  the capability is *conditionally installed*, and when the condition fails it
  is absent, not present-and-refusing.
- **Four node kinds.** A "complex widget" is composition. Nothing here adds a
  fifth.

Two existing pieces do most of the work: `git::host_shell_c` already runs a
command line on a host through the right launcher (`ssh`, `wsl.exe`, local), and
`session_ops::run_exec_command` already runs one locally for
`AutomationAction::Exec`. `kernel::diff` is the closest structural precedent —
request, worker, poll, publish — and `kernel::repos` is the closest cache
precedent, including its TTL.

## Goals / Non-Goals

**Goals:**

- One program-running mechanism, used identically by a bundled plugin and a
  user's, running on the session's own machine.
- A pane can hold several programs outstanding and draw whichever have landed.
- Asking every frame is the *normal* way to keep a value current, and is cheap.
- The security boundary is a decision a user can see and revoke, not a footnote.

**Non-Goals:**

- Streaming, stdin, PTYs, or anything interactive — that is the shell pane, and
  a run that must be watched is a run that should have been a shell.
- A general job scheduler. Automations already schedule work; this is a read a
  pane makes about the here and now.
- Sandboxing the program itself. Once we run `docker`, `docker` is running with
  the user's authority. The boundary is *whether we run it*, not what it may do.

## Decisions

### D1 — The ask is queued, the answer is a published read

`run(key, program, opts)` queues an ask; results land in a `kernel::runs` store
and are published as `thurbox.runs[key]`.

*Revised while implementing* — see Findings. The ask was going to ride the
existing command bus; it does not.

*Why:* it is the split the kernel already enforces, and it makes the
non-blocking property structural rather than a discipline. The alternative — a
Lua function returning output — cannot be non-blocking without inventing
coroutine plumbing across the Rust boundary, and would put an unbounded wait
inside `render`.

*Rejected:* the `want_*` pattern (as content search uses). That serves **one**
parameterised read per key well; here a plugin needs many, each with its own
program, and encoding a program into a store key is a protocol nobody would
enjoy debugging.

### D2 — The key is chosen by the plugin, namespaced by the kernel

The plugin names a run (`"docker.ps"`); the kernel stores it under
`(plugin, key)`.

*Why:* two plugins asking for `"status"` must not collide, and a plugin must be
able to predict its own key to read the result back. Namespacing in the kernel
means the plugin never has to think about the other plugins.

### D3 — Freshness is a TTL, with an explicit refresh

A result carries the instant it completed. An ask for a run whose result is
younger than the TTL is a no-op; older re-runs it. `refresh = true` overrides.

*Why:* it makes "ask every frame" correct by construction, which is the calling
pattern a pane actually wants — `render` has no other natural place to ask from.
`kernel::repos::Listed::stale()` is the same decision for the same reason.

*Trade-off:* a TTL is a guess. A default around a second suits `git status` and
`docker ps`; `npm outdated` is far more expensive and wants far longer. So the
TTL is **per ask**, not global, with a default — the plugin knows what it is
running and we do not.

### D4 — The host is resolved before the worker starts

`session_ops::resolve_host` (already shared by sync, restart and now the diff)
resolves the backend to `Some(None)` local, `Some(Some(host))` remote, or `None`
for a host `hosts.toml` no longer describes. `None` fails the run.

*Why:* three surfaces have now had the same bug — running a remote path's git
locally. Resolving in one place, before any work, makes the refusal the default
outcome rather than something each caller remembers.

### D5 — Bounds: output cap, timeout, concurrency, all fixed by the kernel

Output capped (~256 KB per stream, truncation flagged), wall-clock timeout
(~30 s default, per-ask override), and a small worker pool (~4) with a FIFO
queue.

*Why kernel-fixed rather than plugin-chosen:* a plugin that could raise its own
caps could take the interface down, and the whole point of the bound is that it
does not depend on the plugin being well written. A per-ask timeout *override*
is allowed within a kernel ceiling, because `npm ci` legitimately takes minutes
and a fixed 30 s would make it unusable.

*Truncation is reported*, not silent: a pane showing half a `git status` while
claiming it is the whole one is worse than a pane saying it could not read it
all.

### D6 — Trust is per plugin, granted by the user, and not a sandbox

A plugin declares `capabilities = { "run" }`. It gets nothing until the user
**trusts that plugin**. Trust is granted and revoked in the settings modal's
Interface tab — the surface that already lists every interface file, what it is,
where it came from and whether it draws.

*The position, stated plainly because it is the whole decision:* thurbox cannot
prevent a malicious plugin. The moment we run `docker` or `npm` on the user's
behalf, that program has the user's authority, and no allow-list or prompt
changes it. What thurbox can do is refuse to run anything **unasked**, and make
the asking specific: this plugin, named, in front of you, revocable. That is the
same bargain a shell profile, an editor plugin or a `direnv` file offers, and it
is the honest one.

*Why per plugin rather than one global switch:* a switch answers "may plugins run
programs", which is not a question anyone can answer usefully — you have three
plugins and you trust one of them. Per-plugin trust answers the question the user
actually has. It also degrades correctly: installing a fourth plugin grants it
nothing, where a global switch would have already said yes on its behalf.

*Why the Interface tab rather than a settings row:* the decision is about a
specific file, and that is the surface where files are listed with their state.
Answering it anywhere else means naming the file twice and hoping the user
matches them up. It also puts trust beside `restore` and `remove`, which are the
other two things you do to a file you are suspicious of.

*Trust is keyed by path, with the trusted contents recorded.* Keying by content
digest alone would revoke trust on every edit, which makes developing your own
plugin unbearable — the common case is a file you are actively writing. Keying by
path alone would hide the case that matters: a trusted file whose contents
changed. So trust is by path, the digest at the moment of trusting is recorded,
and the listing **reports the drift** ("trusted · modified") without blocking.
The user decides what that means; thurbox's job is to notice.

*Persisted with the interface's other user decisions* (`ui.json`, beside the key
rebindings and plugin settings), keyed by absolute path — a repo's `./ui` and the
config directory's are different sets of files and must not share trust.

*Rejected — a global `[features]` switch:* it was the previous draft. Dropped
because it answers a question nobody has, and because the Interface tab makes it
redundant: revoking every plugin is three keystrokes there, and the switch would
be a second place for the answer to live.

*Rejected — an allow-list of permitted programs:* reads as safety, is not.
`sh`, `git` (`-c core.pager=…`), `npm` (arbitrary scripts) and `docker` (mounting
`/`) each reach arbitrary execution, so the list would have to exclude precisely
the programs the feature exists for.

*Rejected — prompting per program:* a pane asking every frame cannot prompt, and
a prompt that appears constantly is a prompt that gets approved blindly.

### D7 — "Absent rather than blocked", kept honest

When the capability is unavailable — undeclared, or the plugin is not trusted —
the `run` function is **not installed** in that plugin's environment, so
`command("run", …)` is a nil call, and `thurbox.yml` declares the verb so the
linters can see it. A refusal at *call* time would be a capability that is
present and lies.

This is also what makes revocation immediate: trust is read when a plugin's
environment is built, and revoking triggers the same reload an edit does, so the
function is gone on the next frame rather than being asked to refuse.

*Consequence, accepted:* a plugin must handle its absence, exactly as it handles
a session having no worktree. The worked example shows how — draw what the
setting is, not a blank pane.

### D8 — The worked example is part of the change, not documentation of it

`docs/examples/` gains a composite pane over at least two programs, parsing
output rather than echoing it. It is checked by a test, as `plugin.lua` already
is.

*Why:* the proposal claims a complex widget is composable from four node kinds.
That claim is either demonstrated or it is a hope. If the example cannot be
written without re-deriving column arithmetic in the plugin, `ui/lib/widgets.lua`
gains what is missing — and finding that out is the point of writing it.

## Risks / Trade-offs

- **A plugin can now run anything the user can** → the declaration makes it
  visible in the inventory, trust makes it specific and revocable, and drift
  reporting makes a changed trusted file noticeable. **Not mitigated, by
  design:** a user who trusts a hostile plugin is compromised, and no amount of
  gating inside thurbox changes that. Stated in the proposal rather than buried,
  because a security story that overclaims is worse than one that is plain.
- **Trust keyed by path inherits across edits** → anything with write access to
  the interface directory can change a trusted file. That is the same authority
  needed to change your shell profile, and the listing reports the drift. The
  alternative — revoking on every edit — would make writing your own plugin
  intolerable, which is the workflow this whole change exists to enable.
- **A pane that asks with a short TTL becomes a busy-loop of processes** → the
  concurrency bound turns it into a queue rather than a fork bomb, and the TTL
  floor is enforced by the kernel, not the plugin.
- **Remote runs are ssh round trips** → they are already bounded by the timeout
  and the pool; a slow host makes a pane stale, not the interface slow, because
  nothing on the render path waits.
- **`git::host_shell_c` quoting differs per transport** (POSIX, `wsl.exe`,
  psmux/PowerShell) → the program is passed as one command line and quoted by
  the existing helper, which the remote-hook path already exercises. Windows
  hosts inherit whatever `psmux_hook_rewrite_supported` says about that path.
- **A run outlives the pane that asked for it** → results are dropped when their
  plugin unloads, and the store is bounded; a reload cannot accumulate them.

## Migration Plan

Additive, and closed by default in the only sense that matters: no existing
plugin declares the capability, and nothing is trusted, so nothing gains
anything. Nothing shipped uses `run` — the worked example is documentation, and
a user who copies it into their interface directory has to trust it, which is the
first time they will meet the mechanism. Rollback is revoking trust.

## Open Questions

- **The default TTL and timeout numbers.** They can be tuned once the example
  exists and there is something real to measure. They change no requirement and
  no task.
- **Whether `ui/lib/widgets.lua` needs a table primitive.** Deliberately left to
  D8: writing the example answers it, and guessing now would either add an
  unused widget or miss the one that is needed.

## Findings from implementing

- **The ask is not a `Command`.** D1 said `command("run", …)` on the existing
  bus. It is its own global instead, for two reasons that only became visible
  once written. The bus carries no attribution — `Command` is a flat queue, and a
  run has to be *namespaced by the plugin that asked* or two panes cannot both
  call theirs `status`. And the bus dispatches writes whose result is
  success-or-failure; a run's result is a value the asker reads back, which is a
  store's shape (`kernel::diff`), not the bus's. Everything downstream of that —
  publishing per plugin, the trust gate, the queue — follows from being separate.
- **`enter` is where a capability lives.** Because every plugin shares one Lua
  state, "installed for the plugins that may use it" has to mean *installed per
  call*. That was implicit in D7 and is the single most load-bearing line of the
  change: `run` is set or nil beside the current-plugin stamp, so a plugin that
  was not granted it sees no function rather than one that refuses.
- **A timeout must race the read, not follow it.** Two bugs in sequence, both
  caught by one test. Draining the pipes to EOF before checking the deadline made
  the timeout unreachable; then killing the child and *joining* the readers hung
  just as long, because `sh -c "sleep 30"` hands its stdout to a grandchild that
  keeps the pipe open. The readers write into shared buffers and are abandoned on
  a timeout. Anything that shells out and waits will meet this; the test asserts
  elapsed time, not just the flag.
- **Trust is keyed by path, attribution by path, `state` by file stem.** The
  three nearly diverged: runs were first attributed by the stem `current` already
  carried, so trusting a plugin granted nothing. `current_path` exists beside
  `current` rather than replacing it, because changing what `state` keys by would
  silently move every plugin's stored state on upgrade.
- **No table primitive was needed** (D8's open question). The worked example's
  aligned, stateful table composes from `widgets.list` plus per-row spans, so
  `ui/lib/widgets.lua` is unchanged. The claim that a complex widget is
  composition survived contact with a real one.
