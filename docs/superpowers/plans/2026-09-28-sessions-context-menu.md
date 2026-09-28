# Sessions Context Menu Implementation Plan

<!-- markdownlint-disable MD013 -->

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A right-click on a session row opens a menu of that session's actions at the pointer.

**Architecture:** Three small kernel additions — the press's screen cell on
`hit`, an anchor on `float` (`at`), and an `on_outside` hook for the float that
holds the pointer — then a generic Lua menu float (`64_menu.lua`, fed through
`store.menu`) and an `on_context` on the sessions pane that opens it. Every
entry runs an existing action through `command("action")`.

**Tech Stack:** Rust (ratatui, mlua), Lua 5.4 interface plugins, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-09-28-sessions-context-menu-design.md`

## Global Constraints

- MSRV 1.75, edition 2021; `cargo fmt --all` and
  `cargo clippy --all-targets --all-features -- -D warnings` clean.
- Module dependency allowlist (`tests/architecture_rules.rs`) unchanged: no new
  Rust module.
- Lua gates clean: `selene ui`, `stylua --check ui`,
  `lua-language-server --check ui --configpath "$PWD/.luarc.json" --checklevel=Warning`,
  `scripts/ci/check-lua-types.sh`, `scripts/ci/check-lua-std.sh`.
- Comments explain *why*; no `TODO`/`FIXME`; no commented-out code.
- Commits follow the repo's conventional-commit form
  (`feat(core): …`, `feat(ui): …`, `docs(ui): …`); never name any tool or AI.
- Test-first: every task starts with a failing test.
- Docs that a change invalidates are updated in the same branch (Task 6).

## Review Focus

- A right press near the bottom-right corner: the menu must open up and to the
  left and stay whole on screen (Task 2 `float_rect` tests).
- A terminal narrower or shorter than the menu: the menu is clamped inside the
  area, never drawn off it (Task 2 sweep + `a_float_wider_than_the_screen…`).
- A press outside the menu that lands on a session row or a terminal: it must
  close the menu and do nothing else — no selection, no focus change (Task 3
  e2e: the pane beneath stays `none`).
- Choosing an entry that opens its own float (rename, fork, confirm): the menu
  must already be gone, not drawn under it (Task 4: float is `None` right after
  `enter`).
- A right press on a repo header or on an empty list: no menu at all (Task 5
  `a_right_press_on_no_row_opens_nothing`).

---

## File map

| File | Change |
|---|---|
| `src/kernel/host/mod.rs` | `Click.screen_x/screen_y`; `Float.at`; `pointer_hook` sets the new fields; `LuaHost::on_outside` |
| `src/kernel/host/load.rs` | `read_float` reads `at` |
| `src/coordinator/mouse.rs` | `click_at` fills screen cells; `Grab::Outside`; `dispatch_outside` |
| `src/coordinator/draw.rs` | `float_rect` honours `at`; `anchor_span` |
| `src/coordinator/chrome.rs` | `float_rect` unit tests (existing test module) |
| `src/kernel/perf.rs` | doc comment on `Hook::Click` names `on_outside` |
| `src/kernel/bundled.rs` | ships `plugins/64_menu.lua` |
| `ui/lib/thurbox.d.lua` | `Hit.screen_x/screen_y`, `thurbox.FloatAt`, `Float.at`, `on_outside` |
| `ui/plugins/64_menu.lua` | **new** — the menu float |
| `ui/plugins/10_sessions.lua` | `on_context` + the entry list |
| `tests/mouse.rs` | hook-level tests for tasks 1–3 |
| `tests/terminal_pane.rs` | explicit `Click` literal gains the new fields |
| `tests/tui_e2e.rs` | end-to-end: right press → anchored float → outside press closes it |
| `tests/context_menu.rs` | **new** — the menu float and the sessions opener |
| `tests/plugin_lifecycle.rs` | `64_menu.lua` is an on-demand float |
| docs (Task 6) | `docs/PLUGINS.md`, `ui/README.md`, `ui/AGENTS.md`, `extensions/ui-skill/SKILL.md`, `docs/PERFORMANCE.md`, `docs/FEATURES.md`, `.agents/skills/thurbox-kernel/SKILL.md`, `ui/lib/modal.lua` header |

---

### Task 1: The press's screen cell on `hit`

**Files:**

- Modify: `src/kernel/host/mod.rs:346-368` (`Click`), `:1945-1980` (`pointer_hook`)
- Modify: `src/coordinator/mouse.rs:557-569` (`click_at`)
- Modify: `ui/lib/thurbox.d.lua:213-222` (`thurbox.Hit`)
- Modify: `tests/terminal_pane.rs:640-650`, `tests/mouse.rs:582-594` (explicit `Click` literals)
- Test: `tests/mouse.rs`

**Interfaces:**

- Produces: `Click { screen_x: u16, screen_y: u16, .. }` (Rust); `hit.screen_x`, `hit.screen_y` (Lua integers, 0-based absolute cells, same origin as crossterm's `mouse.column/row`).

- [ ] **Step 1: Write the failing test**

Append to `tests/mouse.rs`, after `each_button_reaches_its_own_hook` (reuses the file's `host_with`, `index_of`, `on`, `said` helpers):

```rust
/// A pane that says where on the SCREEN the press landed.
const WHERE: &str = r#"
return {
  name = "where",
  slot = "sessions",
  order = 10,
  render = function()
    return { type = "text", text = state.said or "" }
  end,
  on_context = function(hit)
    state.said = "at:" .. hit.screen_x .. "," .. hit.screen_y
    return true
  end,
}
"#;

/// `hit.x`/`hit.y` are inside the node; a menu opened at the pointer needs the
/// cell on the screen, which a pane cannot work out — it knows neither where its
/// slot sits nor where the node landed in it.
#[test]
fn a_press_carries_the_screen_cell_it_landed_on() {
    let (_home, host) = host_with(&[("10_where.lua", WHERE)]);
    let index = index_of(&host, "where");
    let click = thurbox::kernel::host::Click {
        screen_x: 33,
        screen_y: 7,
        ..on("a")
    };
    assert!(host.on_context(index, &click).expect("context"), "handled");
    assert!(
        said(&host, index).contains("at:33,7"),
        "the screen cell must reach the hook: {}",
        said(&host, index)
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run --test mouse a_press_carries_the_screen_cell`
Expected: FAIL to compile — `struct Click has no field named screen_x`.

- [ ] **Step 3: Add the fields**

In `src/kernel/host/mod.rs`, inside `pub struct Click`, after `pub h: u16,`:

```rust
    /// The cell the press landed on, on the SCREEN rather than in the node.
    /// What a menu opened at the pointer is anchored to (`float.at`): a pane
    /// knows neither where its slot sits nor where the node landed in it.
    pub screen_x: u16,
    pub screen_y: u16,
```

In `pointer_hook`, after the `table.set("h", …)` line:

```rust
        table
            .set("screen_x", click.screen_x)
            .map_err(|e| fail(e.to_string()))?;
        table
            .set("screen_y", click.screen_y)
            .map_err(|e| fail(e.to_string()))?;
```

In `src/coordinator/mouse.rs`, `click_at`, after `h: target.rect.height,`:

```rust
            screen_x: x,
            screen_y: y,
```

In `tests/mouse.rs` `row_click` and `tests/terminal_pane.rs` `bar_press`, add
`screen_x: 0, screen_y: 0,` after `h: …,` (the compiler lists every literal
that needs it; there are no others at the time of writing).

In `ui/lib/thurbox.d.lua`, `thurbox.Hit`, after `---@field h integer`:

```lua
---@field screen_x integer The pressed cell on the screen, 0-based — what `float.at` takes.
---@field screen_y integer
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run --test mouse --test terminal_pane`
Expected: PASS, including `a_press_carries_the_screen_cell_it_landed_on`.

- [ ] **Step 5: Commit**

```bash
git add src/kernel/host/mod.rs src/coordinator/mouse.rs ui/lib/thurbox.d.lua tests/mouse.rs tests/terminal_pane.rs
git commit -m "feat(core): give a pointer hook the screen cell it was pressed on"
```

---

### Task 2: `float.at` — a float anchored to a point

**Files:**

- Modify: `src/kernel/host/mod.rs:401-423` (`Float` + `Default`)
- Modify: `src/kernel/host/load.rs:303-331` (`read_float`)
- Modify: `src/coordinator/draw.rs:733-759` (`float_rect`, new `anchor_span`)
- Modify: `ui/lib/thurbox.d.lua:171-176` (`thurbox.Float`, new `thurbox.FloatAt`)
- Test: `src/coordinator/chrome.rs` (existing `mod tests`), `tests/mouse.rs`

**Interfaces:**

- Consumes: nothing from Task 1.
- Produces: `Float { at: Option<(u16, u16)>, .. }`; Lua `float = { at = { x = integer, y = integer }, cols = …, rows = … }`.
- [ ] **Step 1: Write the failing tests**

In `src/coordinator/chrome.rs`, `mod tests`, add to the `for float in [ … ]`
list inside `chrome_rects_survive_a_terminal_too_small_for_them`:

```rust
                    Float {
                        cols: Some(20),
                        rows: Some(6),
                        at: Some((width, height)),
                        ..Float::default()
                    },
                    Float {
                        cols: Some(20),
                        rows: Some(6),
                        at: Some((0, 0)),
                        ..Float::default()
                    },
```

and, after that test, these:

```rust
    fn anchored(x: u16, y: u16, cols: u16, rows: u16) -> Float {
        Float {
            cols: Some(cols),
            rows: Some(rows),
            at: Some((x, y)),
            ..Float::default()
        }
    }

    #[test]
    fn an_anchored_float_opens_at_its_point() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(App::float_rect(area, anchored(10, 5, 20, 6)), Rect::new(10, 5, 20, 6));
    }

    /// Past the right edge it opens leftwards, ending on the point — what a
    /// desktop menu does, rather than being cut off.
    #[test]
    fn an_anchored_float_flips_left_at_the_right_edge() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(App::float_rect(area, anchored(70, 5, 20, 6)), Rect::new(51, 5, 20, 6));
    }

    #[test]
    fn an_anchored_float_flips_up_at_the_bottom() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(App::float_rect(area, anchored(10, 22, 20, 6)), Rect::new(10, 17, 20, 6));
    }

    #[test]
    fn an_anchored_float_in_the_corner_flips_both_ways() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(App::float_rect(area, anchored(79, 23, 20, 6)), Rect::new(60, 18, 20, 6));
    }

    #[test]
    fn a_float_wider_than_the_screen_is_pinned_inside_it() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(App::float_rect(area, anchored(40, 10, 200, 6)), Rect::new(0, 10, 80, 6));
    }

    /// The area can start below a band; a point above it is brought down to it.
    #[test]
    fn an_anchor_outside_the_area_is_brought_back_into_it() {
        let area = Rect::new(0, 2, 80, 20);
        assert_eq!(App::float_rect(area, anchored(10, 0, 20, 6)), Rect::new(10, 2, 20, 6));
    }

    #[test]
    fn a_float_with_no_anchor_still_centres() {
        let area = Rect::new(0, 0, 80, 24);
        let float = Float {
            cols: Some(20),
            rows: Some(6),
            ..Float::default()
        };
        assert_eq!(App::float_rect(area, float), Rect::new(30, 9, 20, 6));
    }
```

In `tests/mouse.rs`, append:

```rust
const ANCHORED: &str = r#"
return {
  name = "anchored",
  slot = "float",
  order = 90,
  floats = true,
  render = function()
    return { float = { at = { x = 5, y = 3 }, cols = 10, rows = 4 }, type = "text", text = "x" }
  end,
}
"#;

const MISANCHORED: &str = r#"
return {
  name = "misanchored",
  slot = "float",
  order = 91,
  floats = true,
  render = function()
    return { float = { at = "here" }, type = "text", text = "x" }
  end,
}
"#;

fn float_ctx() -> RenderContext {
    RenderContext {
        width: 80,
        height: 24,
        focused: true,
        elapsed: 0.0,
        frame: 0,
    }
}

#[test]
fn a_float_may_ask_to_open_at_a_point() {
    let (_home, host) = host_with(&[("90_anchored.lua", ANCHORED)]);
    let float = host
        .render(index_of(&host, "anchored"), float_ctx())
        .expect("render")
        .float
        .expect("it floats");
    assert_eq!(float.at, Some((5, 3)));
    assert_eq!((float.cols, float.rows), (Some(10), Some(4)));
}

#[test]
fn a_malformed_anchor_is_reported_by_name() {
    let (_home, host) = host_with(&[("91_misanchored.lua", MISANCHORED)]);
    let error = host
        .render(index_of(&host, "misanchored"), float_ctx())
        .expect_err("a string is not a point");
    assert!(error.message.contains("float.at"), "{}", error.message);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --bin thurbox float && cargo nextest run --test mouse anchor`
Expected: FAIL to compile — `struct Float has no field named at`.

- [ ] **Step 3: Implement**

`src/kernel/host/mod.rs`, in `pub struct Float` after `pub rows: Option<u16>,`:

```rust
    /// The screen cell to open at, instead of the centre: a menu opened by a
    /// press is drawn where the press was. Where it would run off the area it
    /// opens the other way round, then is held inside it (`App::float_rect`).
    pub at: Option<(u16, u16)>,
```

and `at: None,` in `impl Default for Float`.

`src/kernel/host/load.rs`, in `read_float`'s `Float { … }` literal after `rows: …?,`:

```rust
                at: match spec
                    .get::<Value>("at")
                    .map_err(|e| format!("float.at: {e}"))?
                {
                    Value::Nil => None,
                    Value::Table(at) => Some((
                        at.get::<u16>("x").map_err(|e| format!("float.at.x: {e}"))?,
                        at.get::<u16>("y").map_err(|e| format!("float.at.y: {e}"))?,
                    )),
                    other => {
                        return Err(format!(
                            "float.at: expected {{ x, y }}, got {}",
                            other.type_name()
                        ))
                    }
                },
```

Also update `read_float`'s doc line (`load.rs:302`) to mention `at`.

`src/coordinator/draw.rs`: change `float_rect`'s doc to
`/// Place a float: centred, or opened at its \`at\` point.` and replace its
final `Rect { … }` with:

```rust
        let (x, y) = match float.at {
            None => (
                area.x + (area.width - width) / 2,
                area.y + (area.height - height) / 2,
            ),
            Some((x, y)) => (
                anchor_span(x, width, area.x, area.width),
                anchor_span(y, height, area.y, area.height),
            ),
        };
        Rect {
            x,
            y,
            width,
            height,
        }
```

and add after `float_rect`'s `impl` block closes (a free function in the same file):

```rust
/// Where an anchored float starts on one axis: at the point, or ending on it
/// when starting there would run past the area, then held inside the area.
///
/// Flip first, clamp second, so a menu opened in the bottom-right corner grows
/// up and to the left — whole — rather than being shifted under the pointer.
/// `span` never exceeds `len`: `float_rect` has already clamped it.
fn anchor_span(at: u16, span: u16, start: u16, len: u16) -> u16 {
    let end = start.saturating_add(len);
    let at = at.clamp(start, end.saturating_sub(1).max(start));
    let origin = if at.saturating_add(span) <= end {
        at
    } else {
        (at + 1).saturating_sub(span)
    };
    origin.clamp(start, end.saturating_sub(span).max(start))
}
```

(If `float_rect` is inside `impl App`, put `anchor_span` below that `impl`
block, not inside it.)

`ui/lib/thurbox.d.lua`, in `thurbox.Float` add `---@field at? thurbox.FloatAt`
and above the class:

```lua
--- A cell on the screen, as `hit.screen_x`/`hit.screen_y` report it: where an
--- anchored float opens. Past an edge the float opens the other way round.
---@class (exact) thurbox.FloatAt
---@field x integer
---@field y integer
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run --bin thurbox float && cargo nextest run --test mouse`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/kernel/host/mod.rs src/kernel/host/load.rs src/coordinator/draw.rs src/coordinator/chrome.rs ui/lib/thurbox.d.lua tests/mouse.rs
git commit -m "feat(core): let a float open at a point on the screen"
```

---

### Task 3: `on_outside` — tell the holding float that a press missed it

**Files:**

- Modify: `src/kernel/host/mod.rs:1941-1943` (add `on_outside` beside `on_context`)
- Modify: `src/coordinator/mouse.rs:291-299` (`on_click`), `:343-374` (`on_context_click`, `dispatch_context`), `:860-880` (`Grab`, `float_grab`), `:1007-1028` (its unit test)
- Modify: `src/kernel/perf.rs:342` (doc comment)
- Modify: `ui/lib/thurbox.d.lua:302` (declaration)
- Test: `src/coordinator/mouse.rs` `mod tests`, `tests/mouse.rs`, `tests/tui_e2e.rs`

**Interfaces:**

- Consumes: `Click.screen_x/screen_y` (Task 1), `Float.at` (Task 2, e2e only).
- Produces: `LuaHost::on_outside(&self, index: usize, click: &Click) -> Result<bool, PluginError>`; Lua hook `on_outside = function(hit) … end` on a float, `hit.id == nil`, `hit.screen_x/screen_y` set.
- [ ] **Step 1: Write the failing tests**

Replace `a_float_holds_every_press_and_hands_on_only_its_own` in
`src/coordinator/mouse.rs` `mod tests` with:

```rust
    /// Both presses answer to this one rule, so a left and a right press can
    /// never disagree about what a float swallows — or about whom a miss is
    /// told to.
    #[test]
    fn a_float_holds_every_press_and_hands_on_only_its_own() {
        match float_grab(None, painted_by(3)) {
            Grab::Free(target) => assert_eq!(plugin_of(&target), Some(3)),
            _ => panic!("no float is up, so nothing holds the press"),
        }
        match float_grab(Some(7), painted_by(7)) {
            Grab::Held(target) => assert_eq!(target.plugin, 7),
            _ => panic!("a press on the float is the float's"),
        }
        match float_grab(Some(7), painted_by(3)) {
            Grab::Outside(float) => assert_eq!(float, 7, "the miss is told to the float"),
            _ => panic!("a press outside the float is swallowed, not freed"),
        }
        match float_grab(Some(7), None) {
            Grab::Outside(float) => assert_eq!(float, 7),
            _ => panic!("a press on nothing is still swallowed"),
        }
    }
```

Append to `tests/mouse.rs`:

```rust
const DISMISSABLE: &str = r#"
return {
  name = "dismissable",
  slot = "sessions",
  order = 10,
  render = function()
    return { type = "text", text = state.said or "" }
  end,
  on_outside = function(hit)
    state.said = "outside:" .. tostring(hit.id) .. "@" .. hit.screen_x .. "," .. hit.screen_y
    return true
  end,
}
"#;

#[test]
fn a_press_that_misses_a_float_reaches_its_on_outside() {
    let (_home, host) = host_with(&[("10_dismissable.lua", DISMISSABLE)]);
    let index = index_of(&host, "dismissable");
    let click = thurbox::kernel::host::Click {
        screen_x: 4,
        screen_y: 9,
        clicks: 1,
        ..thurbox::kernel::host::Click::default()
    };
    assert!(host.on_outside(index, &click).expect("outside"), "handled");
    assert!(said(&host, index).contains("outside:nil@4,9"), "{}", said(&host, index));
}

/// The safety property again: a float written before the hook existed is not
/// told anything, so it behaves exactly as it always has.
#[test]
fn a_float_without_on_outside_hears_nothing() {
    let (_home, host) = host_with(&[("10_twohanded.lua", TWO_HANDED)]);
    let index = index_of(&host, "twohanded");
    let click = thurbox::kernel::host::Click::default();
    assert!(!host.on_outside(index, &click).expect("outside"), "declined");
    assert!(
        !said(&host, index).contains("left:") && !said(&host, index).contains("right:"),
        "a miss must not reach on_click or on_context: {}",
        said(&host, index)
    );
}
```

Append to `tests/tui_e2e.rs`, after
`the_right_button_reaches_on_context_and_the_left_one_still_reaches_on_click`:

```rust
/// A float pinned where a right press landed, closed by a press anywhere else.
const PINNED: &str = r#"return {
  name = "pinned",
  slot = "float",
  order = 91,
  floats = true,
  focusable = false,
  render = function()
    if not store.pinned then
      return { type = "text", text = "" }
    end
    return { float = { at = store.pinned, cols = 12, rows = 1 }, type = "text", text = "tb-pinned" }
  end,
  on_outside = function(hit)
    store.pinned = nil
    store.outside = (store.outside or 0) + 1
    return true
  end,
}"#;

/// Opens `pinned` at its right press, and paints what reached it.
const PIN_OPENER: &str = r#"return {
  name = "opener",
  slot = "sessions",
  order = 5,
  render = function()
    return {
      type = "text",
      text = "tb-opener-" .. (state.heard or "none") .. "-" .. tostring(store.outside or 0),
      id = "tb-opener",
    }
  end,
  on_click = function(hit)
    state.heard = "left"
    return true
  end,
  on_context = function(hit)
    store.pinned = { x = hit.screen_x, y = hit.screen_y }
    return true
  end,
}"#;

/// The whole road a context menu takes, on the real binary: the screen cell
/// reaches `on_context`, the float opens on that cell, and a press that misses
/// it is told to the float and to nobody else — the pane under that press must
/// not hear it, or closing a menu would also act on whatever was beneath.
#[test]
fn a_float_opens_where_it_was_asked_and_closes_on_a_press_elsewhere() {
    let interface = interface_plus("92_opener.lua", PIN_OPENER);
    std::fs::write(interface.path().join("plugins/91_pinned.lua"), PINNED).expect("add pinned");
    let profile = Profile::new();
    let mut tui = Tui::spawn_with(&profile, 40, 120, |cmd| {
        cmd.env("THURBOX_UI_DIR", interface.path());
    });
    tui.wait_for("tb-opener-none-0");
    let (x, y) = tui.find("tb-opener-none-0");

    tui.press(2, (x + 4, y));
    tui.wait_for("tb-pinned");
    assert_eq!(tui.find("tb-pinned"), (x + 4, y), "the float opens on the pressed cell");

    // Left of the float, on the opener itself: swallowed, and told to the float.
    tui.press(0, (x, y));
    tui.wait_gone("tb-pinned");
    tui.wait_for("tb-opener-none-1");

    let status = tui.quit();
    assert!(status.success(), "exit must be clean: {status:?}");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --bin thurbox a_float_holds && cargo nextest run --test mouse outside`
Expected: FAIL to compile — `no variant Grab::Outside`, `no method on_outside`.

- [ ] **Step 3: Implement**

`src/kernel/host/mod.rs`, after `on_context`:

```rust
    /// Tell the float holding the pointer that a press — either button —
    /// landed outside it.
    ///
    /// The press is spent either way; this only lets the float react, which a
    /// menu needs in order to close. Its own hook for `on_context`'s reason: a
    /// float written before it existed is told nothing, so every modal keeps
    /// swallowing a stray press exactly as it did.
    pub fn on_outside(&self, index: usize, click: &Click) -> Result<bool, PluginError> {
        self.pointer_hook(index, click, "on_outside")
    }
```

Update the doc on `pointer_hook` to `/// The body every pointer hook shares: same payload, different name.`

`src/coordinator/mouse.rs` — replace `enum Grab` and `float_grab`:

```rust
enum Grab {
    /// No float is up; the press goes on down its own path.
    Free(Option<ClickTarget>),
    /// A float is up and the press landed on it.
    Held(ClickTarget),
    /// A float is up and the press missed it: spent, and told to that float
    /// (`on_outside`) so a menu can close.
    Outside(usize),
}

fn float_grab(grabbed: Option<usize>, target: Option<ClickTarget>) -> Grab {
    match grabbed {
        None => Grab::Free(target),
        Some(float) => match target.filter(|target| target.plugin == float) {
            Some(target) => Grab::Held(target),
            None => Grab::Outside(float),
        },
    }
}
```

Keep the `/// What a press may still reach…` doc above `enum Grab`, and change its
last sentence to: "Both buttons ask [`float_grab`], so a left and a right press
cannot come to disagree about what a float swallows or whom a miss is told to."

In `on_click`, the match becomes:

```rust
        let target = match float_grab(self.grabbed, self.target_at(x, y)) {
            Grab::Free(target) => target,
            Grab::Held(target) => {
                self.dispatch_click(target, x, y);
                return;
            }
            Grab::Outside(float) => {
                self.dispatch_outside(float, x, y);
                return;
            }
        };
```

In `on_context_click`, replace the `let (Grab::Free(target) | Grab::Held(target)) = …; if let Some(target) = target { … }` with:

```rust
        match float_grab(self.grabbed, self.target_at(x, y)) {
            Grab::Free(Some(target)) | Grab::Held(target) => {
                self.dispatch_context(target, x, y);
            }
            Grab::Free(None) => {}
            Grab::Outside(float) => self.dispatch_outside(float, x, y),
        }
```

and add after `dispatch_context`:

```rust
    /// Tell the float holding the pointer that a press missed it. Nothing else
    /// hears the press: closing a menu must not also act on what was beneath.
    fn dispatch_outside(&mut self, float: usize, x: u16, y: u16) {
        let click = Click {
            screen_x: x,
            screen_y: y,
            clicks: 1,
            ..Click::default()
        };
        match self.host.on_outside(float, &click) {
            Ok(handled) => {
                if handled {
                    self.dirty = true;
                }
            }
            Err(e) => self.errors.push(e),
        }
    }
```

Run `grep -n "float_grab" src/coordinator/*.rs`: any caller other than the two
above treats `Grab::Outside(_)` as it treated `Grab::Held(None)` (swallowed,
nothing dispatched).

`src/kernel/perf.rs:342`: `/// \`on_click\`, \`on_context\` and \`on_outside\`: the same payload, from the same pointer.`

`ui/lib/thurbox.d.lua`, after the `on_context` field:

```lua
---@field on_outside? fun(hit: thurbox.Hit): boolean A float's: a press of either button that missed it while it held the pointer. `hit.id` is nil.
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run --bin thurbox && cargo nextest run --test mouse && cargo nextest run --test tui_e2e a_float_opens_where`
Expected: PASS (the e2e needs tmux ≥ 3.2 on PATH).

- [ ] **Step 5: Commit**

```bash
git add src/kernel/host/mod.rs src/coordinator/mouse.rs src/kernel/perf.rs ui/lib/thurbox.d.lua tests/mouse.rs tests/tui_e2e.rs
git commit -m "feat(core): tell the float holding the pointer when a press misses it"
```

---

### Task 4: The menu float, `ui/plugins/64_menu.lua`

**Files:**

- Create: `ui/plugins/64_menu.lua`
- Modify: `src/kernel/bundled.rs:100-103` (ship it, after `62_rename`)
- Modify: `tests/plugin_lifecycle.rs:512-517` (it is an on-demand float)
- Test: `tests/context_menu.rs` (new)

**Interfaces:**

- Consumes: `hit.screen_x/screen_y` (Task 1), `float.at` (Task 2), `on_outside` (Task 3), `ui.chord(action) -> string?` (`ui/lib/ui.lua:126`), `command("action", { text = id })`.
- Produces: the contract `store.menu = { at = { x, y }, items = { { label = string, action = string } | "sep", … }, index? = integer }`; entry rows carry `id = "menu-<i>"`, `role = "row"`.
- [ ] **Step 1: Write the failing tests**

Create `tests/context_menu.rs`:

```rust
//! The context menu: the generic float (`ui/plugins/64_menu.lua`) and the
//! sessions pane that opens it.
//!
//! Driven through the host's hooks with the bundled interface, so what is
//! pinned is the Lua contract — `store.menu` in, `command("action")` out. The
//! road from the terminal is `tests/tui_e2e.rs`'s.

use std::path::{Path, PathBuf};

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use thurbox::kernel::command::Command;
use thurbox::kernel::host::{Click, KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::paint::{render_recording, PlaceholderSurfaces};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::Snapshot;
use thurbox::kernel::theme::Themes;

/// Opens a menu of three entries and a rule at its right press.
const OPENER: &str = r#"
return {
  name = "opener",
  slot = "sessions",
  order = 10,
  keys = {
    { key = "x", action = "opener.first", desc = "first", group = "Test" },
  },
  render = function()
    return { type = "text", text = "opener", id = "opener" }
  end,
  on_context = function(hit)
    store.menu = {
      at = { x = hit.screen_x, y = hit.screen_y },
      items = {
        { label = "First", action = "opener.first" },
        { label = "Second", action = "opener.second" },
        "sep",
        { label = "Third", action = "opener.third" },
      },
    }
    return true
  end,
}
"#;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        std::fs::copy(entry.path(), to.join(entry.file_name())).expect("copy");
    }
}

/// The bundled `lib/` and the menu float, plus `OPENER` — nothing else, so no
/// bundled pane can answer for the menu.
fn menu_host() -> (tempfile::TempDir, LuaHost) {
    let home = tempfile::tempdir().expect("tempdir");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
    copy_dir(&source.join("lib"), &home.path().join("lib"));
    std::fs::create_dir_all(home.path().join("plugins")).expect("mkdir");
    std::fs::copy(
        source.join("plugins/64_menu.lua"),
        home.path().join("plugins/64_menu.lua"),
    )
    .expect("copy the menu");
    std::fs::write(home.path().join("plugins/10_opener.lua"), OPENER).expect("write opener");
    let host = LuaHost::new(home.path().to_path_buf());
    assert!(host.error.is_none(), "{:?}", host.error);
    publish(&host, &Snapshot::default());
    (home, host)
}

fn publish(host: &LuaHost, snapshot: &Snapshot) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot,
        attach_errors: &Default::default(),
        inflight: &[],
        themes: &themes,
        registry: &registry,
        diffs: &diffs,
        links: &Default::default(),
        search: None,
        meta: &Default::default(),
        metrics: &Default::default(),
        status_rows: 0,
        can_open: true,
        inventory: &[],
        ui_dir: "ui",
        settings: &Default::default(),
        repos: &repos,
        wants: &Default::default(),
        focus: None,
        selection: None,
        hovered: None,
        printing: &Default::default(),
    })
    .expect("publish");
}

fn index_of(host: &LuaHost, name: &str) -> usize {
    host.plugins
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("{name} should have loaded"))
}

fn ctx() -> RenderContext {
    RenderContext {
        width: 80,
        height: 24,
        focused: true,
        elapsed: 0.0,
        frame: 0,
    }
}

fn right_press_at(x: u16, y: u16, id: Option<&str>) -> Click {
    Click {
        id: id.map(str::to_string),
        role: id.map(|_| "row".to_string()),
        w: 20,
        h: 1,
        screen_x: x,
        screen_y: y,
        clicks: 1,
        ..Click::default()
    }
}

fn key(name: &str) -> KeyPress {
    KeyPress {
        name: name.into(),
        ch: (name.chars().count() == 1).then(|| name.chars().next().unwrap()),
        ..KeyPress::default()
    }
}

/// The menu's text, painted into its own rect, one line per row.
fn menu_text(host: &LuaHost) -> Option<String> {
    let rendered = host.render(index_of(host, "menu"), ctx()).expect("render");
    let float = rendered.float?;
    let (cols, rows) = (float.cols.unwrap_or(40), float.rows.unwrap_or(10));
    let mut hits = Vec::new();
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("terminal");
    terminal
        .draw(|frame| {
            render_recording(frame, frame.area(), &rendered.node, &PlaceholderSurfaces, &mut hits)
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    Some(
        (0..rows)
            .map(|y| (0..cols).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn actions(host: &LuaHost) -> Vec<String> {
    host.drain_commands()
        .into_iter()
        .filter_map(|command| match command {
            Command::Action { action, .. } => Some(action),
            _ => None,
        })
        .collect()
}

fn open(host: &LuaHost) {
    assert!(host
        .on_context(index_of(host, "opener"), &right_press_at(12, 5, Some("opener")))
        .expect("context"));
}

// ── the menu float ──────────────────────────────────────────────────────────

#[test]
fn nothing_floats_until_a_menu_is_asked_for() {
    let (_home, host) = menu_host();
    assert!(menu_text(&host).is_none());
}

#[test]
fn the_menu_opens_at_the_press_sized_to_its_entries() {
    let (_home, host) = menu_host();
    open(&host);
    let float = host
        .render(index_of(&host, "menu"), ctx())
        .expect("render")
        .float
        .expect("the menu floats");
    assert_eq!(float.at, Some((12, 5)));
    assert_eq!(float.rows, Some(6), "four entries and two borders");
    let text = menu_text(&host).expect("drawn");
    for label in ["First", "Second", "Third", "─"] {
        assert!(text.contains(label), "{label} missing:\n{text}");
    }
}

/// The hint is read from the registry, so it follows a rebind; an action with
/// no chord shows none.
#[test]
fn an_entry_shows_the_chord_its_action_is_bound_to() {
    let (_home, host) = menu_host();
    open(&host);
    let text = menu_text(&host).expect("drawn");
    let first = text.lines().find(|l| l.contains("First")).expect("First row");
    assert!(first.trim_end_matches(['│', ' ']).ends_with('x'), "{first}");
}

/// Closed before the action runs, so an action that opens a float of its own is
/// never drawn under a menu that is still up.
#[test]
fn enter_closes_the_menu_and_runs_the_highlighted_entry() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    assert!(host.on_key(menu, &key("enter")).expect("key"));
    assert!(menu_text(&host).is_none(), "the menu must be gone");
    assert_eq!(actions(&host), ["opener.first"]);
}

#[test]
fn moving_down_skips_the_rule() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    host.on_key(menu, &key("down")).expect("key");
    host.on_key(menu, &key("j")).expect("key");
    host.on_key(menu, &key("enter")).expect("key");
    assert_eq!(actions(&host), ["opener.third"]);
}

#[test]
fn moving_past_either_end_stays_put() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    host.on_key(menu, &key("up")).expect("key");
    host.on_key(menu, &key("k")).expect("key");
    host.on_key(menu, &key("enter")).expect("key");
    assert_eq!(actions(&host), ["opener.first"]);
}

#[test]
fn escape_closes_the_menu_and_runs_nothing() {
    let (_home, host) = menu_host();
    open(&host);
    assert!(host.on_key(index_of(&host, "menu"), &key("esc")).expect("key"));
    assert!(menu_text(&host).is_none());
    assert!(actions(&host).is_empty());
}

/// A modal takes every key while it is up; one it does not know is swallowed.
#[test]
fn an_unknown_key_is_swallowed_while_the_menu_is_up() {
    let (_home, host) = menu_host();
    open(&host);
    assert!(host.on_key(index_of(&host, "menu"), &key("q")).expect("key"));
    assert!(menu_text(&host).is_some(), "still open");
}

#[test]
fn a_press_elsewhere_closes_the_menu() {
    let (_home, host) = menu_host();
    open(&host);
    assert!(host
        .on_outside(index_of(&host, "menu"), &right_press_at(70, 20, None))
        .expect("outside"));
    assert!(menu_text(&host).is_none());
    assert!(actions(&host).is_empty());
}

#[test]
fn clicking_an_entry_runs_it() {
    let (_home, host) = menu_host();
    open(&host);
    let menu = index_of(&host, "menu");
    let click = Click {
        id: Some("menu-2".into()),
        role: Some("row".into()),
        clicks: 1,
        ..Click::default()
    };
    assert!(host.on_click(menu, &click).expect("click"));
    assert_eq!(actions(&host), ["opener.second"]);
    assert!(menu_text(&host).is_none());
}

/// The rule, and the frame around the entries, are part of the menu: a press
/// on them is the menu's and does nothing.
#[test]
fn clicking_the_rule_does_nothing() {
    let (_home, host) = menu_host();
    open(&host);
    assert!(host
        .on_click(index_of(&host, "menu"), &Click::default())
        .expect("click"));
    assert!(actions(&host).is_empty());
    assert!(menu_text(&host).is_some(), "still open");
}
```

In `tests/plugin_lifecycle.rs`, add `"plugins/64_menu.lua",` to the
`for float in [ … ]` list after `"plugins/62_rename.lua",`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --test context_menu --test plugin_lifecycle`
Expected: FAIL — `copy the menu: No such file or directory`, and `plugins/64_menu.lua is listed` panics.

- [ ] **Step 3: Write the float**

Create `ui/plugins/64_menu.lua`:

```lua
-- A menu of actions, opened at a point: the context menu's float.
--
-- It knows nothing about what the entries do. A pane that wants a menu leaves
-- it in `store.menu`:
--
--   store.menu = {
--     at = { x = hit.screen_x, y = hit.screen_y },
--     items = { { label = "Rename", action = "sessions.rename" }, "sep", ... },
--   }
--
-- and this draws it at that point and, on a choice, closes and runs the
-- entry's action through `command("action")` -- the palette's road, so an entry
-- does exactly what its chord does, confirmation included. Closed BEFORE the
-- action runs, so an action that opens a float of its own (rename, fork,
-- confirm) is never drawn under a menu that is still up.

local theme = require("lib.theme")
local ui = require("lib.ui")
local widgets = require("lib.widgets")

--- The open menu, if a pane left one.
local function pending()
  local menu = store.menu
  if type(menu) ~= "table" or type(menu.items) ~= "table" or type(menu.at) ~= "table" then
    return nil
  end
  return menu
end

local function choosable(item)
  return type(item) == "table" and type(item.action) == "string"
end

--- The highlighted entry: the one a key moved to, else the first choosable.
local function current(menu)
  if menu.index and choosable(menu.items[menu.index]) then
    return menu.index
  end
  for i, item in ipairs(menu.items) do
    if choosable(item) then
      return i
    end
  end
  return nil
end

--- The next choosable entry from `from` in direction `dir`; `from` at an end.
local function step(menu, from, dir)
  local i = from + dir
  while menu.items[i] ~= nil do
    if choosable(menu.items[i]) then
      return i
    end
    i = i + dir
  end
  return from
end

local function run(menu, i)
  local item = menu.items[i]
  store.menu = nil
  if choosable(item) then
    command("action", { text = item.action })
  end
end

return {
  name = "menu",
  -- A slot the arrangement never places: this only ever floats.
  slot = "float",
  order = 64,
  floats = true,
  -- Reads `store.menu`, the registry and the theme, and writes nothing; a
  -- `store` write bumps the state version the cached tree is keyed on.
  pure = true,
  focusable = false,

  render = function(_)
    local menu = pending()
    if not menu then
      return { type = "text", text = "" }
    end
    local at = current(menu)
    local chords, label_w, chord_w = {}, 0, 0
    for i, item in ipairs(menu.items) do
      if choosable(item) then
        chords[i] = ui.chord(item.action) or ""
        label_w = math.max(label_w, widgets.len(item.label or item.action))
        chord_w = math.max(chord_w, widgets.len(chords[i]))
      end
    end
    -- One column of padding each side, and three between a label and its chord.
    local inner = 1 + label_w + (chord_w > 0 and 3 + chord_w or 0) + 1

    local children = {}
    for i, item in ipairs(menu.items) do
      if choosable(item) then
        local label = item.label or item.action
        local gap = inner - 2 - widgets.len(label) - widgets.len(chords[i])
        local style = { fg = theme.text }
        local hint = { fg = theme.hint }
        if i == at then
          style = {
            bg = theme.role("selection_bg"),
            fg = theme.role("selection_fg"),
            bold = true,
          }
          hint = style
        end
        children[#children + 1] = {
          type = "text",
          len = 1,
          id = "menu-" .. i,
          role = "row",
          text = {
            {
              { text = " " .. label .. string.rep(" ", gap), style = style },
              { text = chords[i] .. " ", style = hint },
            },
          },
        }
      else
        children[#children + 1] = {
          type = "text",
          len = 1,
          text = { { { text = string.rep("─", inner), style = { fg = theme.muted } } } },
        }
      end
    end

    return {
      float = { at = { x = menu.at.x, y = menu.at.y }, cols = inner + 2, rows = #menu.items + 2 },
      type = "box",
      frame = {
        borders = "all",
        border_style = { fg = theme.role("modal_border") },
        style = { bg = theme.role("modal_bg") },
      },
      children = children,
    }
  end,

  on_key = function(key)
    local menu = pending()
    if not menu then
      return false
    end
    local at = current(menu)
    local name = key.key
    if name == "esc" then
      store.menu = nil
    elseif name == "enter" then
      if at then
        run(menu, at)
      end
    elseif at and (name == "down" or (name == "j" and not key.ctrl)) then
      menu.index = step(menu, at, 1)
      store.menu = menu
    elseif at and (name == "up" or (name == "k" and not key.ctrl)) then
      menu.index = step(menu, at, -1)
      store.menu = menu
    end
    -- Everything else is swallowed: a modal that let keys through to the pane
    -- underneath would be a modal in appearance only.
    return true
  end,

  --- An entry runs; the rule and the frame are the menu's and do nothing.
  on_click = function(hit)
    local menu = pending()
    local i = hit.id and tonumber(hit.id:match("^menu%-(%d+)$"))
    if menu and i then
      run(menu, i)
    end
    return true
  end,

  -- A press anywhere else closes it -- what every menu does. The kernel has
  -- already swallowed that press, so it selects or focuses nothing beneath.
  on_outside = function(_)
    if pending() then
      store.menu = nil
    end
    return true
  end,
}
```

In `src/kernel/bundled.rs`, after the `62_rename` entry:

```rust
    (
        "plugins/64_menu.lua",
        include_str!("../../ui/plugins/64_menu.lua"),
    ),
```

- [ ] **Step 4: Run the tests and the Lua gates**

Run:

```bash
cargo nextest run --test context_menu --test plugin_lifecycle
stylua ui && selene ui
lua-language-server --check ui --configpath "$PWD/.luarc.json" --checklevel=Warning
```

Expected: tests PASS; selene/luals report nothing for `64_menu.lua`. If luals
flags a field, fix the declaration in `thurbox.d.lua` it points at (e.g. the
float literal must match `thurbox.FloatAt`), not the plugin.

- [ ] **Step 5: Commit**

```bash
git add ui/plugins/64_menu.lua src/kernel/bundled.rs tests/context_menu.rs tests/plugin_lifecycle.rs
git commit -m "feat(ui): add a menu float opened at a point"
```

---

### Task 5: The sessions pane opens its menu on a right press

**Files:**

- Modify: `ui/plugins/10_sessions.lua` (a `SESSION_MENU` local above `return {`; `on_context` after `on_click` at `:760-772`)
- Test: `tests/context_menu.rs`

**Interfaces:**

- Consumes: `store.menu` contract (Task 4); `ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(id)` (already used by `on_click`).
- Produces: nothing further.
- [ ] **Step 1: Write the failing tests**

Append to `tests/context_menu.rs`:

```rust
// ── the sessions pane opens it ──────────────────────────────────────────────

use thurbox::kernel::snapshot::SessionRow;
use thurbox::session::SessionState;

fn row(name: &str, repo: &str) -> SessionRow {
    SessionRow {
        id: format!("{name}-0000-0000-0000-000000000000"),
        name: name.into(),
        agent: "claude".into(),
        status: SessionState::Idle,
        cwd: Some(PathBuf::from(format!("/src/{repo}"))),
        repo: Some(repo.into()),
        repos: Vec::new(),
        branch: Some("main".into()),
        base_branch: None,
        backend: "local-tmux".into(),
        backend_id: Some("%1".into()),
        remote_host: None,
        agent_session_id: None,
        parent_id: None,
        display_order: None,
        worktree_count: 0,
        git: None,
        stopped: false,
        hook_state: None,
        reports_as: None,
        detected_agent: None,
        shell_backend_id: None,
        member_dirs: Vec::new(),
    }
}

fn two_sessions() -> Snapshot {
    Snapshot {
        sessions: vec![row("alpha", "thurbox"), row("beta", "website")],
        ..Snapshot::default()
    }
}

/// The bundled interface, published two sessions and rendered once.
fn sessions_host() -> LuaHost {
    let host = LuaHost::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    assert!(host.error.is_none(), "{:?}", host.error);
    publish(&host, &two_sessions());
    host.render(index_of(&host, "sessions"), ctx()).expect("render");
    host
}

#[test]
fn a_right_press_on_a_session_opens_its_menu_at_the_pointer() {
    let host = sessions_host();
    let beta = two_sessions().sessions[1].id.clone();
    assert!(host
        .on_context(index_of(&host, "sessions"), &right_press_at(7, 4, Some(&beta)))
        .expect("context"));
    let float = host
        .render(index_of(&host, "menu"), ctx())
        .expect("render")
        .float
        .expect("the menu floats");
    assert_eq!(float.at, Some((7, 4)));
    let text = menu_text(&host).expect("drawn");
    for label in [
        "Open", "Rename", "Fork", "Open in editor", "Restart", "Sync", "Move up",
        "Move down", "Delete", "Delete + worktree",
    ] {
        assert!(text.contains(label), "{label} missing:\n{text}");
    }
    assert!(!text.contains("Sort"), "sort targets no session:\n{text}");
}

/// The entries run the pane's own actions on the SELECTED session, so the
/// press has to select the row it landed on.
#[test]
fn the_right_press_selects_the_session_it_landed_on() {
    let host = sessions_host();
    let beta = two_sessions().sessions[1].id.clone();
    host.on_context(index_of(&host, "sessions"), &right_press_at(7, 4, Some(&beta)))
        .expect("context");
    // Down once, from Open, is Rename; its action opens the rename float for
    // the selected session once the coordinator runs it.
    let menu = index_of(&host, "menu");
    host.on_key(menu, &key("down")).expect("key");
    host.on_key(menu, &key("enter")).expect("key");
    assert_eq!(actions(&host), ["sessions.rename"]);
    assert!(host
        .on_action(index_of(&host, "sessions"), "sessions.rename")
        .expect("action"));
    let rename = host
        .render(index_of(&host, "rename"), ctx())
        .expect("render");
    assert!(rename.float.is_some(), "the rename float opens");
    assert!(format!("{:?}", rename.node).contains("beta"), "…for beta, the row pressed");
}

#[test]
fn a_right_press_on_no_row_opens_nothing() {
    let host = sessions_host();
    assert!(!host
        .on_context(index_of(&host, "sessions"), &right_press_at(7, 1, None))
        .expect("context"));
    assert!(menu_text(&host).is_none());
}

#[test]
fn a_right_press_on_an_empty_list_opens_nothing() {
    let host = LuaHost::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"));
    publish(&host, &Snapshot::default());
    host.render(index_of(&host, "sessions"), ctx()).expect("render");
    assert!(!host
        .on_context(index_of(&host, "sessions"), &right_press_at(7, 4, Some("gone")))
        .expect("context"));
    assert!(menu_text(&host).is_none());
}
```

(Move the two new `use` lines to the top of the file with the others;
`rustfmt` does not do it for you.)

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --test context_menu sessions right_press`
Expected: FAIL — `the menu floats` panics (the sessions pane declares no `on_context`).

- [ ] **Step 3: Implement**

In `ui/plugins/10_sessions.lua`, above the plugin's `return {` (next to the
other file-level locals such as `CURSOR_OPTS`):

```lua
--- The right-press menu: the actions that target one session, in the order
--- the spec settled. Actions only -- each runs back through `on_action`, so an
--- entry and its chord cannot come to mean different things, and delete still
--- asks first when there is work to lose. Sort, the panel toggle and undo are
--- not here: none of them is about the session that was pressed.
local SESSION_MENU = {
  { label = "Open", action = "sessions.open" },
  { label = "Rename", action = "sessions.rename" },
  { label = "Fork", action = "sessions.fork" },
  { label = "Open in editor", action = "sessions.editor" },
  "sep",
  { label = "Restart", action = "sessions.restart" },
  { label = "Sync", action = "sessions.sync" },
  { label = "Move up", action = "sessions.move_up" },
  { label = "Move down", action = "sessions.move_down" },
  "sep",
  { label = "Delete", action = "sessions.delete" },
  { label = "Delete + worktree", action = "sessions.force_delete" },
}
```

After `on_click`:

```lua
  -- A right press on a row selects it, as a left press would, and opens its
  -- menu where the press was. Selecting is what aims the entries: each runs an
  -- action on the selected session. Focus stays put -- the kernel's rule for a
  -- right press -- and the menu takes every key while it is up anyway. A header
  -- carries no id, so it opens nothing, as it selects nothing on a left press.
  on_context = function(hit)
    if not hit.id then
      return false
    end
    local items = session_model.build(sessions())
    if ui.cursor("sessions", items, CURSOR_OPTS):select_by_id(hit.id) == nil then
      return false
    end
    store.menu = { at = { x = hit.screen_x, y = hit.screen_y }, items = SESSION_MENU }
    return true
  end,
```

- [ ] **Step 4: Run the tests and the Lua gates**

Run:

```bash
cargo nextest run --test context_menu --test mouse --test kernel_mvp
stylua ui && selene ui
lua-language-server --check ui --configpath "$PWD/.luarc.json" --checklevel=Warning
```

Expected: PASS; no lint findings.

- [ ] **Step 5: Commit**

```bash
git add ui/plugins/10_sessions.lua tests/context_menu.rs
git commit -m "feat(ui): open a session's menu on a right press in the sessions column"
```

---

### Task 6: Documentation

**Files:**

- Modify: `docs/PLUGINS.md` (*The right button* ~`:862-889`; *Floating panes and modals* ~`:1010-1035`; hook list ~`:1158`)
- Modify: `ui/README.md:297`, `ui/AGENTS.md:155`, `extensions/ui-skill/SKILL.md:174` (hook lists)
- Modify: `docs/PERFORMANCE.md:1589` (hook list)
- Modify: `docs/FEATURES.md` (the sessions column section)
- Modify: `.agents/skills/thurbox-kernel/SKILL.md:162-166` (bundled floats)
- Modify: `ui/lib/modal.lua:3` (the float count in its header)
- [ ] **Step 1: Find every stale count and hook list**

Run: `grep -rn "four floats\|Four panes float\|on_context\`, \`on_scroll\|on_context\`/\`on_scroll" docs ui extensions .agents`
Every hit is a place to name the menu float or `on_outside`.

- [ ] **Step 2: Edit**
- `docs/PLUGINS.md`, *The right button*: replace the `store.filemenu` example
  with the real one —

  ```lua
  on_context = function(hit)
    if not hit.id then return false end
    store.menu = {
      at = { x = hit.screen_x, y = hit.screen_y },
      items = { { label = "Open", action = "files.open" }, "sep",
                { label = "Delete", action = "files.delete" } },
    }
    return true
  end,
  ```

  and add a paragraph: `hit.screen_x`/`screen_y` are the pressed cell on the
  screen; `64_menu.lua` draws `store.menu` there and runs the chosen entry
  through `command("action")`, closing first.
- `docs/PLUGINS.md`, *Floating panes and modals*: document
  `float = { at = { x, y }, … }` (opens at the point; past an edge it opens the
  other way, then is held on screen) and `on_outside(hit)` (a press of either
  button that missed the float while it held the pointer; `hit.id` is nil; the
  press is swallowed either way; a float without it behaves as before).
- Hook lists (`docs/PLUGINS.md:1158`, `ui/README.md:297`, `ui/AGENTS.md:155`,
  `extensions/ui-skill/SKILL.md:174`, `docs/PERFORMANCE.md:1589`): add
  `on_outside` after `on_context`.
- `ui/README.md`: in the floats list, add `64_menu` and the `store.menu` shape.
- `.agents/skills/thurbox-kernel/SKILL.md` and `ui/lib/modal.lua`: the float
  count becomes five and names the menu (`64_menu`, right press). `64_menu`
  does not use `modal.frame` (it has no title or footer), so the modal.lua
  header says "Four of the five floats".
- `docs/FEATURES.md`: under the sessions column, one paragraph — right-click a
  session for its menu; entries and chords; a terminal that keeps the right
  button for itself never sends the press (link to *The right button*).
- [ ] **Step 3: Lint**

Run: `rumdl check . && stylua --check ui && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add docs ui extensions .agents
git commit -m "docs(ui): document the context menu, float.at and on_outside"
```

---

### Task 7: Full gate

- [ ] **Step 1:** `just lint` — expected clean.
- [ ] **Step 2:** `just test` — expected: the new tests pass; compare any
  failure against `main` before blaming this branch (some integration tests
  are known to be flaky locally).
- [ ] **Step 3:** Manual check in the sandbox: `scripts/dev/sandbox.sh --fresh`,
  create two sessions, right-click each row near the bottom-right of the
  column, walk the menu with `j/k`, open Rename, cancel, try Delete (confirm
  appears when soft delete is off), click outside to close.
