## Context

See `proposal.md`. Everything here exists in v1 as working code that happens to
live in `app`/`ui`: `notifications.rs`, `clipboard.rs`, and the link detection
in `ui::links`. This change exposes them, it does not reimplement them.

## Goals / Non-Goals

**Goals:**

- Copy, link-open and notifications as commands and reads.
- A shell as a second session-backed surface, proving the primitive generalises.

**Non-Goals:**

- Mouse. Node identity makes click targeting *possible*, but routing mouse
  events needs an event model the kernel does not have — events carrying node
  identity, and a way to deliver them to a plugin. That is its own change, and
  pretending otherwise would leave a half-built input path.
- The perf HUD. It wants counters the kernel does not publish yet.

## Decisions

### D1 — Links are read from the terminal, not from a tree

A terminal is a surface, so its text is not in any tree a decorator could walk.
Link detection therefore happens kernel-side over the vt100 screen and is
published as a list, which is also where OSC 8 rich-text links live — those have
no plain-text form at all, so a tree-based approach could never have found them.

### D2 — Opening falls back to copying, and says so

On a host with no browser — a remote session, a bare tty — spawning an opener
goes nowhere. v1 learned to copy instead and toast the outcome. Same here: the
command reports which it did, and the plugin renders that.

## Risks / Trade-offs

**Notifications only fire while the interface is running.** Same as v1: the
observer is the render loop. Documented rather than solved.

**Copy needs a real clipboard or a cooperating terminal.** → `clipboard.rs`
already handles both, including the OSC 52 path that reaches the user's own
clipboard from a remote host.

## Findings from implementing

**Copy has to run on the UI thread, and that is the `!Send` guarantee working.**
The vt100 screen lives beside a VM the compiler will not let cross a thread
boundary, and the clipboard wants the tty. So copy joins theme and settings as a
command applied by the loop rather than dispatched to a worker. Design.md D10
described this as making a rule into a compile error; here it decided an
implementation.

**Notification edge detection belongs where status is read.** v1 learned that
computing "should we notify" anywhere other than where `SessionStatus` is
derived lets the rule drift from the icon in the list. The notifier therefore
observes the snapshot, and `observe` returns which sessions it raised for — so
the rule is testable without a desktop, which v1's never was.

**Link detection is stuck behind a module move.** The logic is pure, but it
lives in `ui::links`, which the kernel cannot import and which `v2-retire-v1`
deletes. It wants to move to `session` — where `hyperlink` already is, for the
OSC 8 half of the same problem — so both halves can use one implementation.
Duplicating ~50 lines into the kernel would have been the wrong kind of
progress, so the dependent tasks are marked rather than faked.

**Mouse was a stated non-goal and stays one.** Node identity makes click
targeting *possible*, but routing events needs an event model the kernel does
not have. Half-building it would leave an input path that looks finished.

### D3 — Mouse was promoted from a non-goal, and cost less than the design feared

The design said routing mouse events needed "an event model the kernel does not
have — events carrying node identity, and a way to deliver them to a plugin".

That was true of *click targeting*, and false of *selection*. Dragging over a
terminal needs only screen positions and the rect the surface was painted into;
crossterm delivers the first and the surface provider already tracked the
second. No node-level event routing was involved, so it landed as loop state
rather than as a plugin API.

**Click targeting remains a non-goal**, and node identity still makes it
possible when a consumer appears.

### D4 — Click targeting landed, and node identity was the whole event model

The consumer appeared. What D3 called "an event model the kernel does not have"
turned out to be one function: the paint walk already knows each node's rect, so
recording a hitbox for every node carrying identity costs a `Vec` push and gives
a registry that cannot drift from the cells it covers — where v1 maintains the
same registry by hand, one `record_click` per renderer.

Delivery needed no new vocabulary either. v1's `ClickAction` enum reduces to
three verbs the kernel can honour without knowing what a pane is, each naming an
entry point the *keyboard* already uses, and each spelled as a prefix on the
`role` a node was carrying anyway: `action:<id>` (v1's `Global`), `key:<chord>`
(v1's `ModalButton`, replayed through the real key handler so a button and its
letter cannot diverge), and `focus:<plugin>` (v1's `FocusPane` and `CentralTab`,
which in v2 are the same act because a `switch` slot shows whoever has focus).
Everything else — a list row — goes to the plugin that painted it via `on_click`,
the counterpart of `on_key`. No fourth node kind, and no field added to any node.

*What this cost elsewhere.* Outcome messages were being written to
`layout_error`, which `draw` resets on every successful arrangement — so the
copy, paste and link-open toasts had never actually been visible. They now have a
field and a TTL of their own, painted on the row above the footer: the Lua-owned
arrangement reserves no status row the way v1's `split_vertical` does, and that
row is a pane's bottom border, so nothing legible and no hitbox is covered.

*Consequence worth knowing.* `ctrl+c` becomes conditionally reserved: it copies
when there is a selection and reaches the agent otherwise. That is v1's rule,
and it is the one chord where thurbox wins against the terminal — justified
because a selection on screen is unambiguous evidence of what the user meant.

*Why paste is bracketed.* A multi-line paste sent raw arrives at an agent
prompt as a series of submissions, firing on the first newline. Wrapping it in
`ESC[200~`/`ESC[201~` makes it text. v1 learned this the same way.
