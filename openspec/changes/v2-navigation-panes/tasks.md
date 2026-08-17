## 1. Reads

- [x] 1.1 Publish a session's details: agent, directory, branch, backend, parent, worktrees
- [x] 1.2 Publish git working-tree state, distinguishing "not computed" from "clean"
- [x] 1.3 Expose directory entries under a session's working directory, marked file or directory
- [x] 1.4 Expose a file's text, bounded in size
- [x] 1.5 Refuse any path outside the session's working directory

## 2. Decoration

- [x] 2.1 Let a plugin declare the slot it decorates
- [x] 2.2 Pass that slot's rendered tree to the decorator and draw what it returns
- [x] 2.3 Draw the undecorated tree when a decorator fails, and report it
- [x] 2.4 Apply several decorators on one slot in a deterministic order
- [x] 2.5 A userland helper for walking a tree and matching on identity

## 3. Plugins

- [x] 3.1 An info pane showing the selected session's details
- [x] 3.2 A file viewer over the exposed reads, with no filesystem capability
- [x] 3.3 A search pane matching sessions, tasks and automations
- [x] 3.4 Highlight matches in place, via decoration, in panes search does not own
- [x] 3.5 Activating a result focuses the owning pane and selects the thing
- [x] 3.6 Declare every key through the registry

## 4. Proof

- [x] 4.1 A path outside the session directory is refused
- [x] 4.2 A decorator restyles rows in a pane it does not own
- [x] 4.3 A failing decorator leaves the pane drawn
- [x] 4.4 A query matching several kinds offers all of them

## 5. Columns (added during implementation)

The original plan gave every pane the centre slot, so a pane could only be
reached by *replacing* the agent view. v1's whole point is the opposite — the
side panels sit BESIDE the terminal — so reaching parity needed an arrangement
these tasks never specified.

- [x] 5.1 Reproduce v1's column model in `layout.lua`: sessions 18% · info 15% ·
      centre · tasks 20% · files 20%, with v1's 80/120-column breakpoints
- [x] 5.2 Keep panel visibility outside the plugins, since layout resolves
      before render (`lib/panels.lua`, backed by `store` so it survives reload)
- [x] 5.3 Give each column v1's toggle chord, showing AND focusing it
      (info `ctrl+b`/`f2`, files `ctrl+e`/`f3`, tasks `ctrl+w`/`f5`)
- [x] 5.4 Skip a closed column when cycling focus, and move focus off a column
      the user just closed
- [x] 5.5 Give the centre-slot panes v1's openers (help `ctrl+g`/`f1`, themes
      `ctrl+y`/`f4`, settings `ctrl+,`/`f6`, review `ctrl+x`/`f7`, shell `f8`)
- [x] 5.6 Move the kernel's plugin-reload key off `f5`, which v1 spends on the
      tasks panel
