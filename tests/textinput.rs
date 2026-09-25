//! `lib.textinput`'s line editing, key by key.
//!
//! Every text field in the interface — search, rename, the new-session flow —
//! edits through this one module, so a chord it drops is dropped everywhere.
//! These hold each chord to the value and caret it leaves, written as the
//! value with a `|` where the caret is.
//!
//! The chords are the ones a terminal actually delivers with a modifier flag:
//! `alt+backspace` (`ESC DEL`), `alt`/`ctrl` + `delete`/`left`/`right`, and
//! the `ESC b`/`ESC f`/`ESC d` a macOS terminal sends for option+arrow. Legacy
//! `ctrl+backspace` is `^H`, which the kernel keeps for moving focus, so it
//! reaches a field only under the kitty keyboard protocol — tested here as it
//! arrives then.

use thurbox::kernel::host::{KeyPress, LuaHost, Published, RenderContext};
use thurbox::kernel::registry::Registry;
use thurbox::kernel::snapshot::Snapshot;
use thurbox::kernel::theme::Themes;

/// A field holding `start` (caret at its `|`) inside a probe pane in a copy of
/// the bundled interface, so the module under test is the one that ships.
struct Field {
    host: LuaHost,
    _dir: tempfile::TempDir,
}

impl Field {
    fn new(start: &str) -> Self {
        let (value, cursor) = parse(start);
        let dir = tempfile::tempdir().expect("tempdir");
        let report = thurbox::kernel::bundled::materialize(dir.path());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        std::fs::write(
            dir.path().join("plugins").join("95_probe.lua"),
            format!(
                r#"local textinput = require("lib.textinput")
local field = {{ value = {value:?}, cursor = {cursor} }}
local function show()
  local at = utf8.offset(field.value, field.cursor + 1) or (#field.value + 1)
  store["probe.field"] = string.sub(field.value, 1, at - 1) .. "|" .. string.sub(field.value, at)
end
show()
return {{
  name = "probe",
  slot = "center",
  focusable = true,
  render = function() return {{ type = "text", text = "" }} end,
  on_key = function(key)
    local consumed = textinput.key(field, key)
    show()
    return consumed
  end,
}}
"#
            ),
        )
        .expect("write the probe");
        let host = LuaHost::new(dir.path());
        assert!(host.error.is_none(), "{:?}", host.error);
        publish(&host);
        let index = host.index_of("probe").expect("no probe");
        host.render(
            index,
            RenderContext {
                width: 20,
                height: 3,
                focused: true,
                elapsed: 0.0,
                frame: 0,
            },
        )
        .expect("render");
        Field { host, _dir: dir }
    }

    /// Press `chord` (`alt+left`, `ctrl+delete`, `x`); whether it was consumed.
    fn press(&self, chord: &str) -> bool {
        let index = self.host.index_of("probe").expect("no probe");
        self.host.on_key(index, &key(chord)).expect("on_key")
    }

    fn shown(&self) -> String {
        self.host
            .shared_string("probe.field")
            .expect("the probe publishes its field")
    }
}

fn parse(start: &str) -> (String, usize) {
    let caret = start.find('|').expect("a caret in the start value");
    let value = start.replacen('|', "", 1);
    (value, start[..caret].chars().count())
}

fn key(chord: &str) -> KeyPress {
    let mut key = KeyPress::default();
    let mut rest = chord;
    loop {
        if let Some(tail) = rest.strip_prefix("ctrl+") {
            key.ctrl = true;
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("alt+") {
            key.alt = true;
            rest = tail;
        } else {
            break;
        }
    }
    key.name = rest.to_string();
    if rest.chars().count() == 1 {
        key.ch = rest.chars().next();
    }
    key
}

fn publish(host: &LuaHost) {
    let themes = Themes::load(None);
    let mut registry = Registry::default();
    let (bindings, settings) = host.declarations();
    registry.declare(bindings, settings);
    let diffs = thurbox::kernel::diff::DiffStore::new();
    let repos = thurbox::kernel::repos::RepoStore::with_hosts(Default::default());
    host.publish(&Published {
        epoch: thurbox::kernel::host::Epoch::always_fresh(),
        snapshot: &Snapshot::default(),
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

/// `start`, then each chord in turn, must read `expected` — and every chord
/// must be consumed, so none of them falls through to the pane as well.
fn edits(start: &str, chords: &[&str], expected: &str) {
    let field = Field::new(start);
    for chord in chords {
        assert!(
            field.press(chord),
            "{chord} was not consumed from {start:?}"
        );
    }
    assert_eq!(field.shown(), expected, "{start:?} after {chords:?}");
}

const WORD_BACK: [&str; 3] = ["alt+backspace", "ctrl+backspace", "ctrl+w"];
const WORD_FORWARD: [&str; 3] = ["alt+delete", "ctrl+delete", "alt+d"];
const WORD_LEFT: [&str; 3] = ["alt+left", "ctrl+left", "alt+b"];
const WORD_RIGHT: [&str; 3] = ["alt+right", "ctrl+right", "alt+f"];

#[test]
fn deleting_a_word_back_takes_the_word_before_the_caret() {
    for chord in WORD_BACK {
        edits("alpha beta|", &[chord], "alpha |");
        edits("alpha beta| gamma", &[chord], "alpha | gamma");
        edits("alpha be|ta", &[chord], "alpha |ta");
        edits("alpha beta|", &[chord, chord], "|");
    }
}

#[test]
fn deleting_a_word_back_at_the_start_changes_nothing() {
    for chord in WORD_BACK {
        edits("|alpha", &[chord], "|alpha");
        edits("|", &[chord], "|");
    }
}

#[test]
fn deleting_a_word_back_skips_repeated_separators_first() {
    for chord in WORD_BACK {
        edits("~/src//thing//|", &[chord], "~/src//|");
        edits("alpha   |", &[chord], "|");
    }
}

#[test]
fn deleting_a_word_back_counts_characters_not_bytes() {
    for chord in WORD_BACK {
        edits("café naïve|", &[chord], "café |");
        edits("日本 語|x", &[chord], "日本 |x");
    }
}

#[test]
fn deleting_a_word_forward_takes_the_word_after_the_caret() {
    for chord in WORD_FORWARD {
        edits("|alpha beta", &[chord], "| beta");
        edits("alpha| beta gamma", &[chord], "alpha| gamma");
        edits("al|pha beta", &[chord], "al| beta");
        edits("|alpha beta", &[chord, chord], "|");
    }
}

#[test]
fn deleting_a_word_forward_at_the_end_changes_nothing() {
    for chord in WORD_FORWARD {
        edits("alpha|", &[chord], "alpha|");
        edits("|", &[chord], "|");
    }
}

#[test]
fn deleting_a_word_forward_skips_repeated_separators_first() {
    for chord in WORD_FORWARD {
        edits("a|  //  b c", &[chord], "a| c");
        edits("|//src//thing", &[chord], "|//thing");
    }
}

#[test]
fn deleting_a_word_forward_counts_characters_not_bytes() {
    for chord in WORD_FORWARD {
        edits("|café naïve", &[chord], "| naïve");
        edits("x|日本 語", &[chord], "x| 語");
    }
}

#[test]
fn moving_a_word_left_lands_on_the_start_of_each_word() {
    for chord in WORD_LEFT {
        edits("alpha beta|", &[chord], "alpha |beta");
        edits("alpha beta|", &[chord, chord], "|alpha beta");
        edits("alpha beta|", &[chord, chord, chord], "|alpha beta");
        edits("~/src//thing//|", &[chord], "~/src//|thing//");
        edits("café naïve|", &[chord], "café |naïve");
    }
}

#[test]
fn moving_a_word_right_lands_on_the_end_of_each_word() {
    for chord in WORD_RIGHT {
        edits("|alpha beta", &[chord], "alpha| beta");
        edits("|alpha beta", &[chord, chord], "alpha beta|");
        edits("|alpha beta", &[chord, chord, chord], "alpha beta|");
        edits("|//src//thing", &[chord], "//src|//thing");
        edits("|naïve café", &[chord], "naïve| café");
    }
}

#[test]
fn the_character_keys_are_unchanged() {
    edits("alpha|", &["backspace"], "alph|");
    edits("|alpha", &["delete"], "|lpha");
    edits("al|pha", &["left", "left"], "|alpha");
    edits("al|pha", &["right"], "alp|ha");
    edits("al|pha", &["ctrl+a"], "|alpha");
    edits("al|pha", &["ctrl+e"], "alpha|");
    edits("al|pha", &["ctrl+u"], "|pha");
    edits("al|pha", &["ctrl+k"], "al|");
}

#[test]
fn an_alt_chord_that_is_not_a_line_edit_reaches_the_pane_untyped() {
    // `alt+p` is new-session's "import parent": a field must neither type the
    // `p` nor claim the chord, or the pane holding the field never sees it.
    let field = Field::new("al|pha");
    assert!(!field.press("alt+p"));
    assert!(!field.press("alt+up"));
    assert_eq!(field.shown(), "al|pha");
}

#[test]
fn an_unmapped_ctrl_letter_is_swallowed_rather_than_typed() {
    let field = Field::new("al|pha");
    assert!(field.press("ctrl+z"));
    assert_eq!(field.shown(), "al|pha");
}
