# Interface delta

Every review answers the reviewer's first question before anything else:
*what can the system do now that it could not before, and what did it
cost?*

Interfaces are the honest answer, because an interface is where a
capability becomes visible to the rest of the system. A change that adds a
feature almost always moves a public surface. A change that moves none is
internal work, and saying that plainly is a real finding — it tells the
reviewer to read the diff for structure or behaviour rather than for
contract.

The section is called **Interface delta**. It goes immediately after the
landing section, before every other `##`, and is never `{collapsed}`.

## What counts as an interface

A **surface a consumer outside the changed unit binds to by name**. An
entry earns its place only when all three hold:

1. **It has a name the consumer writes down.** A symbol they import, a
   command or flag they type, a route they call, a config key they set, an
   event kind they match, a field they emit or parse, a column another
   process reads back.
2. **Base and head disagree about it.** The name, the shape, or the
   contract behind it — a signature, a default, the set of permitted
   values, what is guaranteed on error.
3. **The consumer is outside the unit that changed.** Another module,
   another process, another repository, the user. A symbol only its own
   file calls is not an interface.

The test in one question: *could a consumer's code or command line stop
working, start working, or need editing because of this?* No means no
entry.

Not entries: a renamed private helper, a new test, a moved block, a
reworded comment, a field with a default nobody sets, the inside of a
function whose signature held.

## Where to look

Sweep the diff once per surface kind. Each leaves its own markers in the
added and removed lines:

- **Exported code**: `pub`, `export`, `__all__`, public members of a public
  type. A widened or narrowed visibility is an entry on its own.
- **Command line**: subcommands, flags, arguments, exit codes, and the
  shape of what the command prints when a script parses it.
- **Network and messaging**: routes, methods, status codes, RPC names,
  event and message kinds.
- **Persistence and formats**: schemas, migrations, file formats,
  serialized field names, anything another program reads back.
- **Configuration**: environment variables, config keys, defaults,
  permitted values.
- **Extension points**: hooks, plugin APIs, the traits or interfaces a
  consumer implements.
- **Packaging**: binary names, package exports, minimum supported
  versions.

## How to write it

Three groups, in this order, and only the ones that have entries:

```markdown
## Interface delta

**Removed**

- `thurview publish --strict` is gone; every publish is strict now, so a
  script passing it gets a usage error. Nothing replaces it.
  [flag table](anchor:publish-flags)

**Changed**

- `thurview wait` now returns `question` only once per answered thread; a
  poller that re-read the same question stops seeing it.
  [dedupe check](anchor:wait-dedupe)

**Added**

- `thurview publish` gains `--dry-run`: validates and prints diagnostics
  without sealing a revision. [flag registration](anchor:dry-run-flag)
```

Removed first. A removal is the most review-worthy thing a change can
carry and must never read like an addition.

Each entry is one line and carries three things:

1. **The consumer's name for it**, in backticks, spelled the way the
   consumer spells it. `thurview publish --dry-run`, not
   `PublishOptions.dryRun`.
2. **What it means for them** — one clause on what they can now do, must
   now do, or can no longer do.
3. **An anchor on the surface itself** — the declaration, the flag
   registration, the route table, the key in the schema. Added and changed
   entries anchor at head. A removed surface exists only at base, so its
   anchor needs `graph: base`.

A **removed** entry also says what replaces it, or that nothing does. That
is the sentence the reviewer needs to judge whether a contract broke.

## Why the anchor is the rule

The anchor is what keeps this section derived rather than asserted.
`publish` rejects an anchor whose file and lines do not exist at the pinned
commit, so an entry that no code backs cannot ship, and one describing a
surface that moved goes stale loudly instead of quietly.

**If you cannot anchor an entry at the surface it names, it is not an
interface change — delete it.**

## When nothing moved

Say so. Do not manufacture a feature. Two honest forms:

```markdown
## Interface delta

No interface moved. `thurview publish` keeps its flags, its exit codes and
its TOON shape; the change is internal to the anchor validator. Read the
diff for structure, not for contract.
```

```markdown
## Interface delta

No interface moved. [`thurview wait`](anchor:wait-cmd) keeps its flags and
its output; what changed is what it does when a thread was already
answered — it no longer reports that question again. Read the diff for
behaviour, not for contract.
```

The second form names the unchanged surface whose behaviour changed. A bug
fix belongs there: the reviewer's question is whether the new behaviour is
right, not what they must update.

## Making the design legible

With the delta on the page the reviewer can ask the questions the diff
hides — is this surface coherent, is it minimal, does it leak internals,
did a removal break a contract. Where the answer is evident from the
surface itself, add one line under the groups as an observation:

> The three new flags are all forms of "do not write"; one `--dry-run`
> would cover them.

State it and stop. No grade, no score, no checklist of style rules.
thurview is a guided explanation, not a pass/fail bug hunt: an observation
the reviewer can act on is in scope, a rubric is not.
