## 1. Reads

- [x] 1.1 Compute a session's diff on a worker, never on the render path
- [x] 1.2 Publish the diff, distinguishing not-yet-computed from empty
- [x] 1.3 Publish per-file paths with added and removed counts
- [x] 1.4 Cap a very large diff and report the cap rather than truncating silently

## 2. Plugins

- [x] 2.1 A changed-files pane as an ordinary tree list
- [x] 2.2 A review body as a plugin-fed surface
- [x] 2.3 Scroll the body from the height the pane was given
- [x] 2.4 Declare every key through the registry

## 3. Proof

- [x] 3.1 The body is a surface, not a tree — no new node kinds
- [x] 3.2 Not-yet-computed renders differently from no changes
- [x] 3.3 A diff's files and counts reach the pane
