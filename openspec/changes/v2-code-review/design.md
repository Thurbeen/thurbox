## Context

See `proposal.md`. This is the first consumer of a *plugin-fed* surface, and so
the test of design.md D3 from `v2-plugin-kernel`: that some panes are
geometry-first and are not trees at all.

`session::review::parse_unified_diff` and `git::diff_against` already exist and
are pure; only the plumbing is new.

## Goals / Non-Goals

**Goals:**

- Diffs computed off the render path and read from a snapshot.
- The diff body as cells, proving a dense pane needs no new node kinds.
- The changed-files list as an ordinary tree pane beside it.

**Non-Goals:**

- Comments and review marks. They have storage in v1 (`review_comments`,
  `review_marks`) and want a form; the body has to exist first.
- Syntax highlighting. It is a property of the cells the plugin produces, so it
  can arrive without any kernel change — which is the point of the surface.
- Side-by-side layout. Same: a plugin decision, once the body renders at all.

## Decisions

### D1 — Diffs are computed on a worker, keyed by session

A diff shells out to git, so it cannot run on the snapshot refresh. A small
store computes per session on a background thread and publishes when ready,
exactly as the terminal attach does — the third instance of the same shape, and
the argument for making it a pattern rather than three ad-hoc mechanisms.

*Consequence.* "Not computed yet" is a real state a plugin must render, which is
why the spec insists it be distinguishable from "no changes".

### D2 — The body is cells; the file list is a tree

The changed-files list is rows with identity — a tree, like any list. The body
is positioned by character measurement against a resolved width, so it is a
surface. Splitting them that way is what keeps the file list decoratable and
selectable while the body stays free-form.

## Risks / Trade-offs

**A large diff is a large allocation.** → Capped, and the cap is reported rather
than silently truncating.

**Cells lose the identity a tree carries**, so the body cannot be decorated or
clicked line-by-line. → That is the trade D3 accepts. Anything needing identity
(the file list) stays a tree.

## Findings from implementing

**D3 holds: the body needed no new node kinds.** The diff renders as a
plugin-fed `surface` — one span per line, coloured by the plugin — and the
changed-files list stays a tree beside it. The node catalog is still four. The
prior attempt could not express this pane at all and grew its catalog trying;
the split into "structure-first tree, geometry-first surface" is what made the
difference.

**Third instance of the same worker pattern, which makes it a pattern.**
Attaching a terminal, running a command and computing a diff all do the same
thing: touch the world on a background thread and let the UI read the result.
That is worth naming in the kernel docs rather than being rediscovered a fourth
time.

**`publish` grew from two parameters to six in one change, and churned every
test each time.** It is now a `Published` struct. Worth doing earlier next time:
the signal was the second growth, not the sixth.

**Colours came from theme roles that already existed.** `diff_added`,
`diff_removed` and `branch_name` are part of the 31-role palette carried over
from v1, so the review pane is themed by every preset without anyone adding a
role for it. That is the payoff of porting the whole palette rather than the
handful the bare core happened to use.
