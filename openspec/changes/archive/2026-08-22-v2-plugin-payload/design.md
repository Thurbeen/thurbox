## Context

`plugin install` delivers text. `extension_config::fetch_file` returns
`Result<String, String>`; the local half is `read_to_string` and the remote half
decodes curl's stdout through `String::from_utf8_lossy`. A binary or a data file is
therefore not refused but **silently corrupted**, and the only thing standing
between a user and that outcome is `validate_destination` refusing anything but
`.lua`.

The request this answers came from the session shipping a downstream DOOM pane: it
needs a WAD and a program beside its Lua, and today its README has to tell the user
to go and fetch both. The operator's framing was that `plugin install` should
"properly clone the plugin repo which already contains the correct binary", and that
"some plugin repos might have more complex Lua folder structure".

That reframing is the design. `git clone` already delivers arbitrary bytes,
preserves whatever layout the author chose, identifies exactly what it delivered
(the commit), and refuses to clobber a dirty working tree. git is already a hard
dependency — worktrees are the core of a session — and `git::git_command(host, cwd,
args)` is the single invocation helper, already handling the local and remote-host
forms.

Two facts in the tree shape the work, both verified rather than assumed:

- **`build` scans `plugins/*.lua` at the top level only** (`read_dir`, non-recursive,
  `.lua` filter). A cloned repo's pane is invisible to it. But `require` already
  resolves nested module paths from the interface root, so a clone's *modules* work
  today with no change at all.
- **`kernel::watch` watches the interface directory recursively**, and `is_noise`
  filters only editor scratch files (`~`, `.#`, `.goutputstream`). A nested `.git`
  would fire a reload on every git operation. This is a **latent bug already**: it
  affects anyone who runs `git init` in their interface directory to version their
  own panes.

## Goals / Non-Goals

**Goals:**

- A plugin can carry a program and its data, and installing it leaves it usable.
- A repository's own directory layout survives installation unchanged.
- What was installed is identified precisely enough to reproduce elsewhere.
- A plugin delivering several builds can pick the right one itself.
- Nothing that installs today behaves differently.

**Non-Goals:**

- **Bytes through `fetch_file`.** Payload arrives by clone; the fetch-files path
  stays Lua-only. This is what removes the `sha256`, executable-bit and
  platform-matrix questions from the manifest rather than answering them.
- **A size cap or warning.** The operator's decision: installing a plugin is the
  user's, and the size of what they asked for is theirs to know.
- **A post-install command.** Arbitrary code at install time is a bigger consent
  question than "hold a process open on my keystrokes", and it is unnecessary: a
  package that needs building ships a script and lets its pane run it under the
  `program` grant the user already gave — visible, in a pane, killable.
- **Dependency resolution, a registry, or a lockfile spanning plugins.**
- **A forge allowlist.** No guessing that a `github.com` URL means git.
- **Submodules, LFS.** Not refused, not handled; whatever `git clone` does is what
  happens, and a package needing either says so in its own README.

## Decisions

### D1 — A git source is recognised explicitly, never guessed

Three forms, all unambiguous: a `git+` prefix (`git+https://…`), a `.git` suffix, or
the scp-like `git@host:path`. Everything else keeps its current meaning.

A bare `https://github.com/owner/repo` deliberately does **not** clone. That form
already means "a base URL to fetch manifest-named files from", and reinterpreting it
by host would break the existing meaning for one forge while leaving it for others.
`git+` is the escape hatch for a URL that has no `.git` suffix — the spelling cargo
uses, for the same reason.

*Alternative.* A `kind = "git"` field in the spec entry. Rejected: the CLI takes a
source before any spec entry exists, so the form has to carry the answer anyway, and
then the field would be a second place for it to disagree.

### D2 — The clone lands at `ui/<name>/`, and keeps its `.git`

Per the operator: under the interface directory. One tree holds everything the
interface is made of, which is what `plugin list` and the Interface tab already
enumerate, and it means a pane can resolve its own payload from `thurbox.ui_dir`
with no new published path.

Keeping `.git` is my call, and it is the load-bearing one. It buys three things that
are otherwise reimplemented badly:

- `update` is a `fetch` + `checkout`, not a re-download of everything.
- A local edit is **protected**: git refuses to overwrite a dirty tree. That is the
  "an edit is yours to keep" rule the delivery matrix implements for enumerated
  files, done better by the tool that owns it.
- An edit is **recoverable**: `git diff` shows what you changed and `git checkout`
  undoes it. `bundled::restore` cannot do that for a file it never shipped.

The cost is a nested repository inside a config directory, which surprises people and
confuses tooling that walks the tree. Accepted, mitigated by D3, and reversible: an
export-instead variant is a later decision that only changes `update`.

*Alternative.* `git archive` / a tarball export, so no `.git`. Rejected: it makes
`update` a full re-download and throws away the dirty-tree protection, which is the
best property of the whole approach.

### D3 — `.git` becomes watcher noise, which is a fix on its own merits

`is_noise` gains: any event whose path contains a `.git` component is noise.

This is not a concession to D2. Watching `.git` recursively is wrong regardless —
git writes index locks, refs and objects constantly, and a user who versions their
interface directory (a reasonable, likely thing) currently gets reload storms from
their own commits. The change is small, and it is the same class as the existing
carve-out documented in that function: "reading a file is not changing it".

Worth noting the shape of the bug it prevents: the existing comment there explains
that a reader running every frame keeps the debounce window rolling forward so the
reload *never fires*. Git churn during a fetch would do the same thing, so the
symptom is not "reloads too often" but "stops reloading at all while git is busy".

### D4 — The spec entry names the pane; the loader loads what it names

`file = "doom/plugins/40_doom.lua"`, and `build` loads spec-named panes in addition
to `plugins/*.lua`.

The order prefix is read from the **basename**, so `40_` still places it correctly
and nothing about ordering changes. `require` already resolves `doom.lib.util` from
the interface root, so a clone's modules need nothing.

*Alternatives.* (a) The loader scans every installed plugin's `plugins/` — works, but
invents a cross-package ordering rule for no gain, since the spec already knows the
entry point. (b) Copy or symlink the pane up into `plugins/` — rejected outright: the
file then exists twice and the two copies diverge, which is the bug the
single-source-of-truth rules here exist to prevent.

A consequence worth stating: `plugin.toml` inside a cloned repo becomes largely
unnecessary. The spec entry names the entry point and the rest of the repository is
simply present.

### D5 — The lock records the resolved commit

Not the ref asked for. `pin = "main"` installs whatever `main` was, and the lock says
which commit that was, so the same spec plus lock reproduces the same bytes after
`main` moves.

This is what makes a `sha256` field unnecessary rather than merely optional: a commit
identifies every byte delivered, is produced by the source rather than transcribed by
its author, and cannot be maintained inconsistently with the payload. The FNV-1a
per-file digests the lock already carries stay for the fetch path, where they answer
a different question — "did the user edit this" — and are explicitly not an integrity
claim.

### D6 — Platform selection is published, not templated

`thurbox.platform = { os, arch }` from `std::env::consts`, published per frame like
the rest of the snapshot, and declared in `thurbox.yml` so a plugin reading it lints.

Nothing about platforms enters the manifest. The reason is expressiveness: a
substitution template (`dist/doom-{os}-{arch}`) states one rule, while a plugin that
reads its platform can express every rule it actually needs — prefer a binary already
on `PATH`, fall back to a portable build, distinguish a libc variant, refuse
politely. The kernel does not have to model any of that, and the pane already has to
handle "the binary is there and will not run", which is the same branch as "there is
no binary for me".

*Alternatives.* `{os}`/`{arch}` substitution (one rule, and a second knob within a
release); a `when` filter per entry (a small predicate language); one entry per
platform (verbose, and a checksum per platform — which is where the downstream's
question 3 came from and why it disappears here).

### D7 — Install writes what the repository contains; running is still gated

Executable bits included. Stripping them and re-adding them on the trust grant means
fighting git — it restores the bit on every `checkout` — for no security gain: the
bit does not make anything run, the `program` capability does, and that is granted
per file by the user and refused without them.

So the mitigation is honesty rather than machinery, and it belongs in three places
people actually read: `docs/PLUGINS.md`, `ui/AGENTS.md` (what an agent reads before
installing anything), and the install command's own output. **Installing a plugin
from a repository puts that repository's files on your disk.** That is what cloning
anything does, and pretending otherwise is worse than saying it.

### D8 — Two mechanisms, chosen by the source's form

The fetch-files path is untouched and stays Lua-only. `ui-plugins/tasks` and
`ui-plugins/top` are directories inside the thurbox repository rather than
repositories of their own, and cloning all of thurbox to get one example pane would
be absurd — so both mechanisms are genuinely needed, not a transitional state.

Stated as a decision so it is not later mistaken for an oversight: if you need
payload, publish a repository.

### D9 — `sync` clones a missing git entry, non-interactively

A spec that cannot be applied on a fresh machine defeats the lockfile, so
convergence installs what the spec lists regardless of source kind.

The risk that made this a question is that a clone can **prompt** — an SSH
passphrase, a host-key confirmation — and `sync` runs from the command drain, so a
prompt is a frozen interface. The answer is the one the session path already uses:
`crate::shell::SSH_HARDENING_OPTS` (`BatchMode=yes`, `ConnectTimeout`,
`ServerAlive*`) exists precisely so a clone that *would* prompt fails fast with a
message instead. A clone that needs credentials the environment does not already
provide is reported, not waited on.

### D10 — Shallow by default; fetch a recorded commit explicitly

`--depth 1` for a fresh install: it is dramatically faster, and running a pane needs
no history. When the lock records a commit that is not the tip — reproducing a spec
elsewhere — that commit is fetched by id (`git fetch --depth 1 origin <sha>`), which
modern git supports for a reachable object.

The cost is that `git log` and a diff against history are limited inside the working
copy. Accepted: the dirty-tree protection and `git diff` against the *checkout* —
the two properties D2 is for — need no history at all.

### D11 — The pane gets an inventory row; the payload does not

`sources()` walks the bundled top-level files, `lib/` and `plugins/`. A clone at
`ui/<name>/` is therefore invisible to it — which would leave the spec-named pane
**loaded but absent from the inventory**, the exact "a file the tab cannot account
for" problem the tab exists to prevent.

So `sources()` gains the spec-named panes, reported with the origin their entry
names. The rest of the working copy is deliberately **not** walked: a repository can
hold hundreds of files, none of them interface code, and listing them would bury the
panes among a plugin's assets. The tab lists what the interface is made of; a
plugin's payload is what one of those panes reads.

## Risks / Trade-offs

- **A nested `.git` inside a config directory** → D2's accepted cost. Mitigated by
  D3 for the reload problem; the remaining cost is tooling that walks
  `~/.config/thurbox/` finding a repository. Reversible without touching anything
  but `update`.
- **`plugin install` now runs `git`, which can prompt** → A clone from a private
  repository over SSH can block on a passphrase or a host-key prompt, and the
  install runs from the command drain. The SSH hardening the session path already
  uses (`BatchMode=yes`, `ConnectTimeout`) exists for exactly this and is the thing
  to reuse; a clone that would prompt must fail with a message rather than hang the
  interface.
- **A clone is slow and unbounded, and there is no size cap** → The operator's
  decision, recorded. Worth stating that the consequence is a visibly slow install
  rather than a hidden one: the install is a command with output, not a background
  fetch.
- **git owns "your edits are yours" for clones; the delivery matrix owns it for
  enumerated files** → Two preservation mechanisms depending on source kind. Named
  rather than hidden: git is strictly better at it, and unifying them would mean
  reimplementing dirty-tree detection.
- **A repository can contain anything, including a binary nobody can audit** →
  Unchanged from `run` and `program`: thurbox can only refuse to run things unasked.
  What is new is the volume of unauditable bytes, which is why D7 puts the sentence
  in front of the person installing.
- **GPL and redistribution** → A package shipping a GPL-2.0 program obliges its
  publishing repository to carry corresponding source. Not thurbox's obligation, but
  worth a line in the docs where publishing is described, since the mechanism
  invites it.

## Migration Plan

Nothing to migrate. No schema change; no existing spec entry has a git source, so no
existing install changes behaviour. An interface with no installed plugins is
unaffected in every respect.

The `.git` watcher change is strictly a fix: it can only *stop* spurious reloads.

Rollback is removing a git entry from `plugins.toml` and running `sync`, which takes
the working copy back — or deleting `ui/<name>/` by hand, which the next `sync`
reports rather than silently undoing.

## Open Questions

- **Should the `.git`-free variant exist as an option?** D2 keeps `.git` for the
  update and dirty-tree properties. A user who wants none of that in their config
  directory has no way to ask for an export instead. Worth adding only if somebody
  asks; the mechanism would be one flag and a different `update`.
- **Submodules and LFS.** Whatever `git clone` does by default is what happens, which
  for both means "not fetched". A package needing either has to say so in its README
  today. Handling them is a flag away but neither has a caller yet.
- **Does a plugin want a published payload path, distinct from `thurbox.ui_dir`?**
  Today a pane resolves `<ui_dir>/<name>/…` itself, which works and is what the
  downstream package already does. If payload ever moves out of the interface tree
  (the alternative rejected in D2), that resolution is the one line that has to move
  with it, and a published path would have insulated it.
