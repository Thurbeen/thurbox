## 1. One directory resolution

- [x] 1.1 Move `resolve_ui_dir` from `src/bin/thurbox2.rs` into `kernel::bundled`, returning the directory *and* the rule that chose it
- [x] 1.2 Have `thurbox2` call it, so the interface and any report cannot disagree
- [x] 1.3 Cover each rule: the environment override, a `./ui` beside the working directory, the user's own copy

## 2. The example, embedded once

- [x] 2.1 Write `docs/examples/plugin.lua`: a pane that renders, declares a key and a setting, and is commented as a starting point rather than a demonstration
- [x] 2.2 Embed it with `include_str!` so the scaffold and the guide share one artifact
- [x] 2.3 Build a host from it alone in a test and render it, so it cannot rot

## 3. `thurbox-cli plugin`

- [x] 3.1 Add the subcommand module and wire it into the CLI, following `extensions.rs`'s shape
- [x] 3.2 `dir`: the directory in force and the rule that chose it, human by default and machine-readable when piped
- [x] 3.3 `new <name>`: write the starter into the directory in force, refuse an existing file, refuse a name that is not a single safe segment
- [x] 3.4 `check`: load through the real `LuaHost`, report per file, exit non-zero when anything failed
- [x] 3.5 `list`: every file with its origin and whether it is on screen, from `kernel::inventory`
- [x] 3.6 Declare `cli → kernel` path-only in `tests/architecture_rules.rs`, with the reason at the entry

## 4. The guide

- [x] 4.1 Open `docs/PLUGINS.md` with the fast path: where the file goes, the smallest plugin, how to see it, how to check it
- [x] 4.2 Add the traps section — the `state` write-back rule first, then definition order, `and`/`or` over a miss, the float's slot, and `on_action` returning false
- [x] 4.3 Document the `plugin` commands where the directory is discussed, replacing "press F11" as the only answer
- [x] 4.4 Point at the fast path from `CLAUDE.md` and `docs/V2-KERNEL.md`, one line each

## 5. Proof

- [x] 5.1 `dir` reports the override, the checkout and the user copy, and names the rule
- [x] 5.2 `new` writes a loadable plugin, refuses an existing name, and refuses a path-like one
- [x] 5.3 `check` passes on the bundled interface, fails with the file and reason on a broken one, and reports an empty interface as empty rather than failed
- [x] 5.4 `list` reports a shipped file as shipped and an edited one as edited
- [x] 5.5 Lint clean: clippy, fmt, selene, stylua, rustdoc, architecture rules
