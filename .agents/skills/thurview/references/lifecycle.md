# Lifecycle and storage

## Binding and pins

A review binds to one unit of change: a branch, a pull request, or an
explicit range. Scaffold resolves and records exact base and head commit
ids. Everything is read from those commits, so moving the checkout changes
nothing the reader sees.

Choose the base deliberately:

- bare scaffold: the trunk fork point (`merge-base` with `origin/HEAD`)
- one commit: `--base <head>~1 --head <head>`
- a stack: the branch directly below it

`thurview scaffold --update --review <id>` re-pins from the binding: a branch
follows its local tip, a PR asks GitHub. Publication never moves pins; it
warns when the branch moved past them.

## Statuses

| Status                   | Owner and next action                                    |
| ------------------------ | -------------------------------------------------------- |
| `draft`                  | Agent authors and publishes.                             |
| `awaiting-review`        | Reader reads, asks, comments, decides.                   |
| `awaiting-agent-updates` | Agent addresses threads, resolves them, republishes.     |
| `accepted`               | Terminal. Cannot be republished.                         |
| `rejected`               | Terminal.                                                |

"Ask now" does not change the status. "Submit review" with "Request changes"
sets `awaiting-agent-updates`; with "Approve" sets `accepted`.

Dismissal is separate: the reader removes the review from the active list and
`wait` returns `review-dismissed`. A new publication restores it.

## Threads

Two kinds, chosen by the reader when creating one:

- `ask` mode (a question): delivered at once. `wait` returns `question`.
  Answer with `threads reply`. It stays open until the reader resolves it;
  open questions never block a republish.
- `review` mode (a comment): held as pending until the reader submits. Then
  `wait` returns `awaiting-agent-updates` with the submitted threads.

Targets: a document block (with an optional quoted selection), a file line
on the base or head side, a map node, or the whole review.

`publish` after the first revision requires zero open submitted comment
threads. Resolve a thread only when its requested change is present. Do not
rewrite or merge threads.

```sh
thurview threads list --review <id> [--open]
thurview threads get <threadId> --review <id>
thurview threads reply <threadId> --review <id> --body "<text>"
thurview threads resolve <threadId> --review <id>
```

## Storage

```text
${THURVIEW_HOME:-~/.thurview}/
├── THURVIEW.md              user guidance (optional)
├── server.json              running server, if any
└── reviews/<id>/
    ├── review.md            you edit
    ├── data.yaml            you edit
    ├── map.yaml             you edit
    ├── theme.yaml           you edit (project look; empty = default skin)
    ├── review.json          binding, pins, status, presented revision
    ├── threads.json         threads and decisions (use the CLI)
    └── revisions/<n>/       sealed copies plus compiled document.json, map.json
```

A failed publish leaves the last sealed revision in place. The reader can
switch between revisions in the browser.

## wait

`thurview wait --review <id> [--timeout <s>]` polls the review and returns
`wait.reason` in `question`, `awaiting-agent-updates`, `accepted`,
`rejected`, `review-dismissed`, `review-deleted`, with the threads that need
you. After the timeout it fails with code `TIMEOUT` (exit 1). A question
already answered by the agent is not reported again.

`thurview threads get <id>` truncates bodies over 1500 characters; pass
`--full` when the hint says so.
