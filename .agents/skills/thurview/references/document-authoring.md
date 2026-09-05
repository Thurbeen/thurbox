# Document authoring

## Reader contract

The reader sees the original request and your document. Not your reasoning,
not the implementation session, not the names you used while working.
Jargon coined during implementation is noise to them.

The H1 is the review's title in the browser and on the home page. Make it
short and specific to the change ("Publish pipeline: single mount"), never
generic.

Write short. Decide what to leave out before deciding what to put in. Every
sentence costs the reader attention; spend it on what they cannot get from the
diff: intent, the shape of the change, risk, what was left out and why.

Write in plain, direct sentences. One idea per sentence. No praise of the
change ("robust", "comprehensive"). Describe it.

## Structure

Open with a landing section between the H1 and the first H2:

```markdown
# Audit every login

**Summary**

- login() now records every attempt before checking the user
- failed attempts are logged too, which the old code skipped
- one new module, no interface change

**Why**

Support could not tell whether a locked-out user had tried at all. The
request was "log every attempt, not only successes".
```

Then fewer than five further sections when practical. Pick those that fit:

- requirements
- design
- interface change
- lifecycle or data flow
- state or storage
- testing evidence
- decision log

Add implementation detail only where it lets the reader check an important
claim. In the decision log, keep the user's requirements in the user's words
and add the implementation decisions that shaped the result.

Do not invent user-impact risks. When risk depends on usage you do not know,
say so or ask.

Collapse optional detail with `{collapsed}` at the end of an H2:

```markdown
## Edge cases {collapsed}
```

Progressive disclosure: every `##` heading is a section the reader can fold.

## Evidence

Every claim about code carries an anchor. An anchor is a named source range
at the pinned base or head commit, defined in `data.yaml`. Link prose to it
with an anchor link; the browser opens the range beside the document:

```markdown
The [spawn site](anchor:spawn) falls back to 120x30.
```

Show code inline only when the reader must see it to follow the main claim:

````markdown
```peek
spawn
```
````

Use the smallest range that proves the claim. Read the range from the pinned
commit before you anchor it. `publish` rejects an anchor whose file or lines
do not exist at that commit, and an anchor link to an anchor with no `peek`.

A claim you cannot anchor is a question, not a fact. Write it as one.

## Diagrams

Use the fenced components for behaviour that prose explains badly:

- `sequence` for temporal behaviour across actors
- `callstack` for call-flow differences between base and head
- `database` for persisted-state structure and the operations on it

Each message, frame and operation carries an anchor, so the reader can open
the code behind every arrow. See [Components](components.md).

Add a diagram only when it materially helps. A document with one good
sequence diagram beats one with four.

## Files you edit

Only `review.md`, `data.yaml`, `map.yaml` and `theme.yaml` in the review
directory. Never
edit `review.json`, `threads.json` or `revisions/`. Threads change only
through `thurview threads`.
