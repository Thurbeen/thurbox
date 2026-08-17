//! Keys and settings, declared by plugins and collected in one place.
//!
//! Design.md D7: coherence has to be a property of the API, not a request in
//! the docs. A plugin declares its keys and settings as *data*, so the kernel
//! can enumerate them without invoking anything — which is what makes a new
//! plugin's keys show up in help and become rebindable with no other file
//! edited.
//!
//! The kernel collects, detects conflicts, applies persisted overrides and
//! routes. It renders nothing: help and settings are plugins reading what was
//! collected.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::host::KeyPress;

/// Where a declared key is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Fires wherever focus is.
    Global,
    /// Fires only while the declaring plugin holds focus, so two plugins may
    /// declare the same chord without conflicting.
    Plugin,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Plugin => "plugin",
        }
    }

    fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("global") => Scope::Global,
            _ => Scope::Plugin,
        }
    }
}

/// A key a plugin responds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Plugin that declared it.
    pub plugin: String,
    /// Stable identifier the plugin is called back with.
    pub action: String,
    /// Chord as declared, canonicalised.
    pub default_chord: String,
    /// Chord actually in force — the override when one exists.
    pub chord: String,
    /// Whether [`chord`](Self::chord) came from a persisted override.
    ///
    /// Not derivable from `chord != default_chord` (rebinding an action to the
    /// chord it already had is legal), and it decides who wins a clash: see
    /// [`Registry::resolve`].
    pub overridden: bool,
    pub description: String,
    pub scope: Scope,
    /// Whether a focused agent terminal keeps this chord instead.
    ///
    /// v1's `Action::terminal_passthrough` (`src/session/keybindings.rs`): the
    /// global chords share the `Ctrl+<letter>` namespace with readline, so the
    /// ones an agent CLI needs for line editing (`Ctrl+R` reverse-search,
    /// `Ctrl+D` EOF, `Ctrl+W` delete-word, …) are forwarded to the pty while a
    /// terminal has focus. The command stays reachable from every other pane
    /// and from its F-key alternate. Navigation and app control (`Ctrl+H/J/K/L`,
    /// `Ctrl+Q`, `Ctrl+N`) are deliberately not marked: they are the keyboard
    /// escape route out of the terminal.
    pub passthrough: bool,
    /// Section this key belongs to in help — "Navigation", "Sessions", …
    ///
    /// v1 grouped its help by *function* rather than by which module owned the
    /// key, and that is the more useful grouping: you look for "how do I delete
    /// a session", not "what does the session list offer". Defaults to the
    /// plugin's name so an undeclared key still lands somewhere sensible.
    pub group: String,
}

/// A setting a plugin accepts.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub plugin: String,
    pub id: String,
    pub description: String,
    pub default: Value,
    /// Effective value: the override when one exists, else the default.
    pub value: Value,
}

/// An action-band entry a plugin contributes.
///
/// Declared as data, like a key or a setting, so a pane earns its place in the
/// chrome by declaring one table field and knowing nothing about how the band
/// draws. The band resolves the chord itself, from the registry, so an entry can
/// never advertise a chord the keyboard does not honour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pill {
    /// Plugin that declared it.
    pub plugin: String,
    /// The action pressing it runs — the same identifier a key binds to.
    pub action: String,
    pub label: String,
    /// Higher is kept longer when the band runs out of width.
    pub priority: i64,
}

/// A setting's value. Deliberately small — a setting is a knob, not a document.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Number(f64),
    Text(String),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::Text(_) => "text",
        }
    }
}

/// A chord claimed by two declarations whose scopes overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub chord: String,
    pub kept: String,
    pub shadowed: String,
}

/// Chords the kernel keeps for itself.
///
/// The escape route: a plugin that consumes every key it is offered must not be
/// able to trap you inside it. Declaring or overriding one of these is refused.
// Reload is F10 because v1 already spends F5 on the tasks panel; the kernel
// takes one of the two F-keys v1 leaves free rather than shadowing a pane.
//
// Focus movement is here as well as on Tab: v1 binds it to Ctrl+H/Ctrl+L and
// refuses to defer either to the agent, precisely because they are how you get
// *out* of a focused terminal. F12 is v1's perf HUD, which reports on the loop
// and so must not be something a plugin can take over.
// Tab and Shift+Tab are DELIBERATELY absent. v1 forwards both to the agent
// (`agent::input::key_to_bytes` maps Tab to `\t` and BackTab to CSI Z), because
// every coding agent uses Tab for completion — taking it for focus movement
// makes the pane unusable for the thing the pane exists to run. Focus moves on
// Ctrl+H/Ctrl+L, which v1 reserves for exactly this reason.
pub const RESERVED: [&str; 5] = ["ctrl+q", "f10", "ctrl+h", "ctrl+l", "f12"];

/// Everything declared, plus what the user changed.
#[derive(Default)]
pub struct Registry {
    bindings: Vec<Binding>,
    settings: Vec<Setting>,
    pills: Vec<Pill>,
    conflicts: Vec<Conflict>,
    /// Persisted overrides: action → chord.
    binding_overrides: BTreeMap<String, String>,
    /// Persisted overrides: `plugin.setting` → value.
    setting_overrides: BTreeMap<String, Value>,
    /// Plugins the user turned off: absolute paths, present on disk and not
    /// loaded.
    ///
    /// Kept here rather than in the delivery manifest deliberately.
    /// `.bundled.json` records what the *binary* did to the directory — wrote,
    /// updated, preserved, tombstoned — and a disabled bundled file is still one
    /// delivery should keep up to date. Putting a user's preference there would
    /// make "I turned this off" and "we stopped shipping this" the same kind of
    /// fact (design D1).
    disabled: BTreeSet<String>,
    /// Plugins the user trusted with a capability: absolute path → the digest
    /// its contents had when trust was granted.
    ///
    /// Keyed by **absolute** path because a repo's `./ui` and the config
    /// directory's are different sets of files and must not share trust. Keyed
    /// by path rather than by digest because revoking on every edit would make
    /// writing your own plugin intolerable — the digest is kept so the drift can
    /// be *reported* instead (design D6).
    trusted: BTreeMap<String, String>,
    /// What would not load: a config file that failed to parse, a rejected
    /// override. Conflicts are *not* kept here — see [`Registry::warnings`].
    load_warnings: Vec<String>,
}

impl Registry {
    /// Load persisted overrides, migrating v1's bindings on first run.
    pub fn load() -> Self {
        let mut registry = Self::default();
        let (bindings, settings, trusted, disabled, mut warnings) = read_overrides();
        registry.binding_overrides = bindings;
        registry.trusted = trusted;
        registry.disabled = disabled;
        registry.setting_overrides = settings;

        if registry.binding_overrides.is_empty() {
            let (migrated, notes) = migrate_v1_bindings();
            if !migrated.is_empty() {
                warnings.push(format!(
                    "migrated {} keybinding override(s) from v1",
                    migrated.len()
                ));
                registry.binding_overrides = migrated;
            }
            warnings.extend(notes);
        }
        registry.load_warnings = warnings;
        registry
    }

    /// Replace the declarations after a reload, keeping the overrides.
    ///
    /// Overrides survive because they belong to the *user*, not to the plugin
    /// set — an override naming a plugin that is temporarily broken must come
    /// back when the plugin does.
    pub fn declare(&mut self, bindings: Vec<Binding>, settings: Vec<Setting>) {
        self.declare_all(bindings, settings, Vec::new());
    }

    /// [`Self::declare`] including the action-band entries.
    ///
    /// Separate rather than a third parameter on `declare` because most callers
    /// — every test that only cares about keys — have no entries to pass, and a
    /// two-argument call site reads better than one ending in `Vec::new()`.
    pub fn declare_all(
        &mut self,
        bindings: Vec<Binding>,
        settings: Vec<Setting>,
        pills: Vec<Pill>,
    ) {
        self.bindings = bindings;
        self.settings = settings;
        self.pills = pills;
        self.conflicts.clear();
        self.apply_overrides();
        self.detect_conflicts();
    }

    fn apply_overrides(&mut self) {
        for binding in &mut self.bindings {
            // The default is restored when no override remains, so clearing one
            // takes effect on the next keystroke rather than on the next reload
            // — which is what "reset this action" has to mean in the help
            // editor.
            match self.binding_overrides.get(&binding.action) {
                Some(chord) => {
                    binding.chord = chord.clone();
                    binding.overridden = true;
                }
                None => {
                    binding.chord = binding.default_chord.clone();
                    binding.overridden = false;
                }
            }
        }
        for setting in &mut self.settings {
            let key = format!("{}.{}", setting.plugin, setting.id);
            if let Some(value) = self.setting_overrides.get(&key) {
                // A stored value of the wrong shape is ignored rather than
                // coerced: a plugin that declared a number should never be
                // handed a string because an old file said so.
                if value.type_name() == setting.default.type_name() {
                    setting.value = value.clone();
                }
            }
        }
    }

    /// Find chords claimed twice in overlapping scopes.
    ///
    /// Two plugin-scoped declarations of `j` are fine — focus decides. A global
    /// claim overlaps with everything.
    fn detect_conflicts(&mut self) {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for (index, binding) in self.bindings.iter().enumerate() {
            if let Some((_, first)) = seen.iter().find(|(chord, other)| {
                chord == &binding.chord && scopes_overlap(&self.bindings[*other], binding)
            }) {
                // Named the way `resolve` decides it, so the diagnostic and the
                // routing can never disagree about who actually fires.
                let earlier = &self.bindings[*first];
                let (kept, shadowed) = if binding.overridden && !earlier.overridden {
                    (binding, earlier)
                } else {
                    (earlier, binding)
                };
                self.conflicts.push(Conflict {
                    chord: binding.chord.clone(),
                    kept: kept.action.clone(),
                    shadowed: shadowed.action.clone(),
                });
                continue;
            }
            seen.push((binding.chord.clone(), index));
        }
    }

    /// Everything worth telling the user: what would not load, then every chord
    /// two plugins both claimed.
    ///
    /// Conflicts are rendered here on demand rather than appended during
    /// `apply_overrides`. That ran on every `declare` — i.e. on every reload — and
    /// appended without clearing, so each reload left one more copy of every
    /// conflict behind, for the life of the process.
    pub fn warnings(&self) -> Vec<String> {
        let mut all = self.load_warnings.clone();
        all.extend(self.conflicts.iter().map(|conflict| {
            format!(
                "{} is claimed by both {} and {}; {} wins",
                conflict.chord, conflict.kept, conflict.shadowed, conflict.kept
            )
        }));
        all
    }

    /// The action a keypress fires, given who has focus.
    ///
    /// A user's **override** outranks a declaration that merely defaults to the
    /// same chord: rebinding an action onto a chord something else already
    /// declared has to move the chord, or the help editor would accept a
    /// binding that silently never fires. Between two equals, the earlier
    /// declaration wins — load order, so it is stable across runs.
    pub fn resolve(&self, press: &KeyPress, focused_plugin: Option<&str>) -> Option<&Binding> {
        let chord = canonical_chord(press);
        let active = |binding: &&Binding| {
            binding.chord == chord
                && match binding.scope {
                    Scope::Global => true,
                    Scope::Plugin => focused_plugin == Some(binding.plugin.as_str()),
                }
        };
        self.bindings
            .iter()
            .find(|binding| active(binding) && binding.overridden)
            .or_else(|| self.bindings.iter().find(active))
    }

    /// Is this file present but deliberately not loaded?
    pub fn is_disabled(&self, path: &str) -> bool {
        self.disabled.contains(path)
    }

    /// Every file the user turned off.
    pub fn disabled(&self) -> impl Iterator<Item = &str> {
        self.disabled.iter().map(String::as_str)
    }

    /// Turn a plugin off, or back on. Returns whether it is now off.
    ///
    /// Idempotent in both directions: the user asked for a state, not for a
    /// transition, and a toggle they cannot predict is worse than one that does
    /// nothing.
    pub fn set_disabled(&mut self, path: &str, off: bool) -> Result<bool, String> {
        if off {
            self.disabled.insert(path.to_string());
        } else {
            self.disabled.remove(path);
        }
        self.persist()?;
        Ok(off)
    }

    /// Has the user trusted this file with the capabilities it declares?
    pub fn is_trusted(&self, path: &str) -> bool {
        self.trusted.contains_key(path)
    }

    /// The contents trusted, if any — so a caller can notice they have changed.
    pub fn trusted_digest(&self, path: &str) -> Option<&str> {
        self.trusted.get(path).map(String::as_str)
    }

    /// Trust a file as it is right now.
    ///
    /// The digest is taken from the contents at this moment, which is what makes
    /// "trusted, and since modified" a state the interface can report.
    pub fn trust(&mut self, path: &str, contents: &str) -> Result<(), String> {
        self.trusted
            .insert(path.to_string(), super::bundled::digest(contents));
        self.persist()
    }

    /// Withdraw trust. Idempotent: revoking what was never trusted is not an
    /// error, because the user asked for a state, not for a transition.
    pub fn revoke(&mut self, path: &str) -> Result<(), String> {
        self.trusted.remove(path);
        self.persist()
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Every declared action-band entry, in declaration order. The band orders
    /// them by priority itself.
    pub fn pills(&self) -> &[Pill] {
        &self.pills
    }

    pub fn settings(&self) -> &[Setting] {
        &self.settings
    }

    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    /// Settings belonging to one plugin, as `id → value`.
    pub fn settings_for(&self, plugin: &str) -> BTreeMap<&str, &Value> {
        self.settings
            .iter()
            .filter(|setting| setting.plugin == plugin)
            .map(|setting| (setting.id.as_str(), &setting.value))
            .collect()
    }

    /// Rebind an action, or clear the override when `chord` is `None`.
    pub fn rebind(&mut self, action: &str, chord: Option<&str>) -> Result<(), String> {
        match chord {
            Some(chord) => {
                let chord = normalise_chord(chord);
                if RESERVED.contains(&chord.as_str()) {
                    return Err(format!("{chord} is reserved and cannot be rebound"));
                }
                if !self.bindings.iter().any(|b| b.action == action) {
                    return Err(format!("no action named {action:?}"));
                }
                self.binding_overrides.insert(action.to_string(), chord);
            }
            None => {
                self.binding_overrides.remove(action);
            }
        }
        self.apply_overrides();
        self.conflicts.clear();
        self.detect_conflicts();
        self.persist()
    }

    /// Set a setting's value, or clear the override when `value` is `None`.
    pub fn set_setting(
        &mut self,
        plugin: &str,
        id: &str,
        value: Option<Value>,
    ) -> Result<(), String> {
        let key = format!("{plugin}.{id}");
        let Some(declared) = self
            .settings
            .iter()
            .find(|s| s.plugin == plugin && s.id == id)
        else {
            return Err(format!("no setting named {key:?}"));
        };
        match value {
            Some(value) => {
                if value.type_name() != declared.default.type_name() {
                    return Err(format!(
                        "{key} is a {}, not a {}",
                        declared.default.type_name(),
                        value.type_name()
                    ));
                }
                self.setting_overrides.insert(key, value);
            }
            None => {
                self.setting_overrides.remove(&key);
            }
        }
        // Reset to defaults before reapplying, or a cleared override would keep
        // its old value until the next reload.
        for setting in &mut self.settings {
            setting.value = setting.default.clone();
        }
        self.apply_overrides();
        self.persist()
    }

    fn persist(&self) -> Result<(), String> {
        let Some(path) = overrides_path() else {
            return Err("could not resolve the config directory".to_string());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
        }
        let mut json = serde_json::Map::new();
        json.insert(
            "bindings".to_string(),
            serde_json::to_value(&self.binding_overrides).map_err(|e| e.to_string())?,
        );
        let settings: serde_json::Map<String, serde_json::Value> = self
            .setting_overrides
            .iter()
            .map(|(key, value)| (key.clone(), value_to_json(value)))
            .collect();
        json.insert("settings".to_string(), serde_json::Value::Object(settings));
        json.insert(
            "trusted".to_string(),
            serde_json::to_value(&self.trusted).map_err(|e| e.to_string())?,
        );
        json.insert(
            "disabled".to_string(),
            serde_json::to_value(&self.disabled).map_err(|e| e.to_string())?,
        );

        let text = serde_json::to_string_pretty(&serde_json::Value::Object(json))
            .map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

/// Do two declarations compete for the same chord?
fn scopes_overlap(a: &Binding, b: &Binding) -> bool {
    match (a.scope, b.scope) {
        // Two plugin-scoped claims only collide when the same plugin makes both.
        (Scope::Plugin, Scope::Plugin) => a.plugin == b.plugin,
        _ => true,
    }
}

/// Canonical text for a keypress.
///
/// **This is where a real portability trap lives.** A terminal without the
/// kitty protocol sends a bare `J` byte with no SHIFT modifier, while one with
/// it sends `j` + SHIFT. Both must produce `shift+j`, or a capital binding
/// works in one terminal and silently does nothing in the other — which is
/// exactly the bug the session list hit when it matched on `key.shift`.
pub fn canonical_chord(press: &KeyPress) -> String {
    let mut parts = Vec::new();
    if press.ctrl {
        parts.push("ctrl".to_string());
    }
    if press.alt {
        parts.push("alt".to_string());
    }

    let mut key = press.name.to_lowercase();

    // Ctrl+/ reaches a terminal as one of three things: the kitty protocol
    // reports it literally, while a legacy terminal sends the raw 0x1F byte,
    // which crossterm surfaces as ctrl+7 or ctrl+_ depending on the emulator.
    // Folding them here means a plugin declares one chord and it works
    // everywhere — the same argument as the capital-letter case below. v1
    // instead bound all three, in three places.
    if press.ctrl && matches!(key.as_str(), "7" | "_" | "/") {
        return "ctrl+/".to_string();
    }
    let shifted = press.shift
        || press
            .ch
            .is_some_and(|c| c.is_ascii_uppercase() || SHIFTED_SYMBOLS.contains(&c));
    if shifted && key != "tab" {
        // A shifted symbol is its own key ("!"), not "shift+1"; only letters
        // and named keys take the prefix.
        //  is stable only from 1.82; this crate targets 1.75.
        if press.ch.map_or(true, |c| c.is_ascii_alphabetic()) {
            parts.push("shift".to_string());
        } else if let Some(c) = press.ch {
            key = c.to_string();
        }
    }
    if key == "backtab" {
        return "shift+tab".to_string();
    }
    parts.push(key);
    parts.join("+")
}

/// Symbols that only exist as a shifted keystroke.
const SHIFTED_SYMBOLS: [char; 11] = ['!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '_'];

/// Canonicalise a chord written by hand, so `Ctrl+D` and `ctrl+d` agree.
pub fn normalise_chord(raw: &str) -> String {
    let mut modifiers: Vec<String> = Vec::new();
    let mut key = String::new();
    for part in raw.split('+') {
        let part = part.trim().to_lowercase();
        match part.as_str() {
            "ctrl" | "control" => modifiers.push("ctrl".into()),
            "alt" | "meta" | "option" => modifiers.push("alt".into()),
            "shift" => modifiers.push("shift".into()),
            // v1 accepted these spellings for the Command key.
            "cmd" | "command" | "super" | "win" => modifiers.push("cmd".into()),
            other => key = other.to_string(),
        }
    }
    // Fixed order, so the same chord always reads the same way.
    let mut parts: Vec<String> = Vec::new();
    for wanted in ["ctrl", "alt", "shift", "cmd"] {
        if modifiers.iter().any(|m| m == wanted) {
            parts.push(wanted.to_string());
        }
    }
    // A capital letter written on its own means shift — "J" is shift+j. Only
    // when nothing else was written, though: "Ctrl+D" conventionally means
    // ctrl+d, not ctrl+shift+d, and reading it the other way would silently
    // move every hand-written binding.
    if parts.is_empty()
        && key.chars().count() == 1
        && raw
            .trim()
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_uppercase())
    {
        parts.push("shift".to_string());
    }
    parts.push(key);
    parts.join("+")
}

fn overrides_path() -> Option<PathBuf> {
    crate::paths::config_file().and_then(|config| config.parent().map(|dir| dir.join("ui.json")))
}

/// What `ui.json` carries: rebound chords, changed settings, trusted plugins,
/// and anything worth warning about.
type Overrides = (
    BTreeMap<String, String>,
    BTreeMap<String, Value>,
    BTreeMap<String, String>,
    BTreeSet<String>,
    Vec<String>,
);

fn read_overrides() -> Overrides {
    let mut bindings = BTreeMap::new();
    let mut settings = BTreeMap::new();
    let mut trusted = BTreeMap::new();
    let mut disabled = BTreeSet::new();
    let mut warnings = Vec::new();

    let Some(path) = overrides_path() else {
        return (bindings, settings, trusted, disabled, warnings);
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (bindings, settings, trusted, disabled, warnings);
    };
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            warnings.push(format!("{}: {e}", path.display()));
            return (bindings, settings, trusted, disabled, warnings);
        }
    };
    if let Some(map) = parsed.get("bindings").and_then(|v| v.as_object()) {
        for (action, chord) in map {
            if let Some(chord) = chord.as_str() {
                bindings.insert(action.clone(), normalise_chord(chord));
            }
        }
    }
    if let Some(map) = parsed.get("settings").and_then(|v| v.as_object()) {
        for (key, value) in map {
            if let Some(value) = json_to_value(value) {
                settings.insert(key.clone(), value);
            }
        }
    }
    if let Some(map) = parsed.get("trusted").and_then(|v| v.as_object()) {
        for (path, digest) in map {
            if let Some(digest) = digest.as_str() {
                trusted.insert(path.clone(), digest.to_string());
            }
        }
    }
    if let Some(list) = parsed.get("disabled").and_then(|v| v.as_array()) {
        for path in list.iter().filter_map(|v| v.as_str()) {
            disabled.insert(path.to_string());
        }
    }
    (bindings, settings, trusted, disabled, warnings)
}

/// Bring v1's `keybindings.json` overrides forward.
///
/// v1 keyed overrides by `Action` name and stored a *list* of chords; v2 keys by
/// a plugin's action id and stores one. Only actions with an obvious equivalent
/// are carried — the rest are reported rather than guessed at, because silently
/// binding a key to the wrong thing is worse than not binding it.
fn migrate_v1_bindings() -> (BTreeMap<String, String>, Vec<String>) {
    let mut migrated = BTreeMap::new();
    let mut notes = Vec::new();

    let Some(path) = crate::paths::keybindings_file() else {
        return (migrated, notes);
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (migrated, notes);
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
        notes.push(format!(
            "{}: could not be parsed; not migrated",
            path.display()
        ));
        return (migrated, notes);
    };
    let Some(map) = parsed.as_object() else {
        return (migrated, notes);
    };

    let mut unmapped = Vec::new();
    for (action, chords) in map {
        let Some(chord) = chords
            .as_array()
            .and_then(|list| list.first())
            .and_then(|v| v.as_str())
            .or_else(|| chords.as_str())
        else {
            continue;
        };
        match V1_ACTIONS.iter().find(|(v1, _)| v1 == action) {
            Some((_, v2)) => {
                migrated.insert((*v2).to_string(), normalise_chord(chord));
            }
            None => unmapped.push(action.clone()),
        }
    }
    if !unmapped.is_empty() {
        unmapped.sort();
        notes.push(format!(
            "not migrated (no v2 equivalent yet): {}",
            unmapped.join(", ")
        ));
    }
    (migrated, notes)
}

/// v1 `Action` name → v2 plugin action id.
///
/// Only the ones the bare core actually provides. Extended as each pane lands,
/// which is why the unmapped remainder is reported rather than dropped.
const V1_ACTIONS: [(&str, &str); 6] = [
    ("SelectNextSession", "sessions.next"),
    ("SelectPreviousSession", "sessions.previous"),
    ("DeleteSession", "sessions.delete"),
    ("RestartSession", "sessions.restart"),
    ("SessionListMoveDown", "sessions.move_down"),
    ("SessionListMoveUp", "sessions.move_up"),
];

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(t) => serde_json::Value::String(t.clone()),
    }
}

fn json_to_value(value: &serde_json::Value) -> Option<Value> {
    match value {
        serde_json::Value::Bool(b) => Some(Value::Bool(*b)),
        serde_json::Value::Number(n) => n.as_f64().map(Value::Number),
        serde_json::Value::String(s) => Some(Value::Text(s.clone())),
        _ => None,
    }
}

/// Build a binding from a plugin's declaration table.
pub fn binding_from(
    plugin: &str,
    chord: &str,
    action: &str,
    description: &str,
    scope: Option<&str>,
    passthrough: bool,
    group: Option<&str>,
) -> Binding {
    let chord = normalise_chord(chord);
    Binding {
        plugin: plugin.to_string(),
        action: action.to_string(),
        default_chord: chord.clone(),
        chord,
        overridden: false,
        description: description.to_string(),
        scope: Scope::parse(scope),
        passthrough,
        group: group.unwrap_or(plugin).to_string(),
    }
}

/// Is this chord a bare `Ctrl+<letter>` — the namespace readline owns?
///
/// v1's `is_ctrl_letter_chord` (`src/app/key_handlers.rs`), and it gates the
/// deferral for the same reason: rebinding a passthrough action onto a key the
/// agent does not want (an F-key, `Ctrl+,`) must make it work in the terminal
/// again, so the test is on the *bound* chord rather than on the action.
pub fn is_ctrl_letter_chord(chord: &str) -> bool {
    matches!(
        chord.strip_prefix("ctrl+"),
        Some(key) if key.len() == 1 && key.starts_with(|c: char| c.is_ascii_alphabetic())
    )
}

/// Help sections, in v1's order.
///
/// Anything a plugin puts in a group not listed here follows, alphabetically —
/// so a third-party plugin appears without editing this list.
pub const HELP_SECTIONS: [&str; 4] = ["Navigation", "Sessions", "Project", "UI"];

#[cfg(test)]
mod tests {
    use super::*;

    fn press(name: &str, ch: Option<char>, ctrl: bool, shift: bool) -> KeyPress {
        KeyPress {
            name: name.to_string(),
            ch,
            ctrl,
            alt: false,
            shift,
        }
    }

    fn binding(plugin: &str, chord: &str, action: &str, scope: Scope) -> Binding {
        Binding {
            plugin: plugin.into(),
            action: action.into(),
            default_chord: normalise_chord(chord),
            chord: normalise_chord(chord),
            overridden: false,
            description: String::new(),
            scope,
            passthrough: false,
            group: plugin.into(),
        }
    }

    #[test]
    fn both_terminal_encodings_of_a_capital_agree() {
        // The portability trap: a legacy terminal sends a bare "J" with no
        // modifier; a kitty-protocol one sends "j" + SHIFT. Both must be
        // shift+j, or a capital binding works in one and not the other.
        let legacy = press("j", Some('J'), false, false);
        let kitty = press("j", Some('j'), false, true);
        assert_eq!(canonical_chord(&legacy), "shift+j");
        assert_eq!(canonical_chord(&kitty), "shift+j");
    }

    #[test]
    fn every_encoding_of_ctrl_slash_agrees() {
        // A legacy terminal sends the raw 0x1F byte, surfaced as ctrl+7 or
        // ctrl+_; a kitty-protocol one reports ctrl+/ literally. A plugin
        // declares one chord and gets all three.
        for name in ["7", "_", "/"] {
            assert_eq!(
                canonical_chord(&press(name, name.chars().next(), true, false)),
                "ctrl+/"
            );
        }
    }

    #[test]
    fn chords_canonicalise_consistently() {
        assert_eq!(
            canonical_chord(&press("d", Some('d'), true, false)),
            "ctrl+d"
        );
        assert_eq!(canonical_chord(&press("f5", None, false, false)), "f5");
        assert_eq!(
            canonical_chord(&press("enter", None, false, false)),
            "enter"
        );
        assert_eq!(
            canonical_chord(&press("backtab", None, false, true)),
            "shift+tab"
        );
    }

    #[test]
    fn hand_written_chords_normalise_to_the_same_form() {
        assert_eq!(normalise_chord("Ctrl+D"), "ctrl+d");
        assert_eq!(normalise_chord("CONTROL+d"), "ctrl+d");
        assert_eq!(normalise_chord("J"), "shift+j");
        assert_eq!(normalise_chord("shift+j"), "shift+j");
        // Modifier order is fixed, so one chord has one spelling.
        assert_eq!(normalise_chord("shift+ctrl+a"), "ctrl+shift+a");
    }

    #[test]
    fn a_plugin_scoped_key_fires_only_for_its_plugin() {
        let mut registry = Registry::default();
        registry.declare(
            vec![
                binding("sessions", "j", "sessions.next", Scope::Plugin),
                binding("themes", "j", "themes.next", Scope::Plugin),
            ],
            Vec::new(),
        );
        let key = press("j", Some('j'), false, false);
        assert_eq!(
            registry
                .resolve(&key, Some("sessions"))
                .map(|b| b.action.as_str()),
            Some("sessions.next")
        );
        assert_eq!(
            registry
                .resolve(&key, Some("themes"))
                .map(|b| b.action.as_str()),
            Some("themes.next")
        );
        // Two plugins declaring `j` is not a conflict — focus decides.
        assert!(
            registry.conflicts().is_empty(),
            "{:?}",
            registry.conflicts()
        );
    }

    #[test]
    fn a_global_key_fires_wherever_focus_is() {
        let mut registry = Registry::default();
        registry.declare(
            vec![binding("help", "ctrl+g", "help.toggle", Scope::Global)],
            Vec::new(),
        );
        let key = press("g", Some('g'), true, false);
        assert!(registry.resolve(&key, Some("anything")).is_some());
        assert!(registry.resolve(&key, None).is_some());
    }

    #[test]
    fn overlapping_claims_are_reported_and_resolved_deterministically() {
        let mut registry = Registry::default();
        registry.declare(
            vec![
                binding("a", "ctrl+g", "a.go", Scope::Global),
                binding("b", "ctrl+g", "b.go", Scope::Global),
            ],
            Vec::new(),
        );
        assert_eq!(registry.conflicts().len(), 1);
        let conflict = &registry.conflicts()[0];
        assert_eq!(conflict.kept, "a.go");
        assert_eq!(conflict.shadowed, "b.go");
        // The earlier declaration keeps the chord, every time.
        let key = press("g", Some('g'), true, false);
        assert_eq!(
            registry.resolve(&key, None).map(|b| b.action.as_str()),
            Some("a.go")
        );
    }

    #[test]
    fn a_global_claim_overlaps_a_plugin_scoped_one() {
        let mut registry = Registry::default();
        registry.declare(
            vec![
                binding("a", "x", "a.x", Scope::Global),
                binding("b", "x", "b.x", Scope::Plugin),
            ],
            Vec::new(),
        );
        assert_eq!(registry.conflicts().len(), 1);
    }

    #[test]
    fn settings_default_until_overridden() {
        let mut registry = Registry::default();
        registry.declare(
            Vec::new(),
            vec![Setting {
                plugin: "sessions".into(),
                id: "compact".into(),
                description: String::new(),
                default: Value::Bool(false),
                value: Value::Bool(false),
            }],
        );
        assert_eq!(
            registry.settings_for("sessions").get("compact"),
            Some(&&Value::Bool(false))
        );
    }

    #[test]
    fn an_override_of_the_wrong_type_is_ignored() {
        // A stored value that no longer matches the declaration must not be
        // coerced — handing a plugin a string where it declared a number is
        // worse than ignoring the file.
        let mut registry = Registry::default();
        registry
            .setting_overrides
            .insert("sessions.compact".into(), Value::Text("yes".into()));
        registry.declare(
            Vec::new(),
            vec![Setting {
                plugin: "sessions".into(),
                id: "compact".into(),
                description: String::new(),
                default: Value::Bool(false),
                value: Value::Bool(false),
            }],
        );
        assert_eq!(
            registry.settings_for("sessions").get("compact"),
            Some(&&Value::Bool(false))
        );
    }

    #[test]
    fn an_override_survives_a_reload_that_drops_its_action() {
        // The user's override belongs to the user: a plugin that is broken for
        // one reload must get its binding back when it returns.
        let mut registry = Registry::default();
        registry
            .binding_overrides
            .insert("sessions.delete".into(), "ctrl+x".into());

        registry.declare(Vec::new(), Vec::new());
        assert!(registry.bindings().is_empty());

        registry.declare(
            vec![binding("sessions", "d", "sessions.delete", Scope::Plugin)],
            Vec::new(),
        );
        assert_eq!(registry.bindings()[0].chord, "ctrl+x");
        assert_eq!(registry.bindings()[0].default_chord, "d");
    }

    #[test]
    fn reserved_chords_cannot_be_rebound() {
        let mut registry = Registry::default();
        registry.declare(
            vec![binding("sessions", "d", "sessions.delete", Scope::Plugin)],
            Vec::new(),
        );
        for reserved in RESERVED {
            let error = registry
                .rebind("sessions.delete", Some(reserved))
                .unwrap_err();
            assert!(error.contains("reserved"), "{error}");
        }
    }

    #[test]
    fn rebinding_an_unknown_action_is_refused() {
        let mut registry = Registry::default();
        let error = registry.rebind("nope.nothing", Some("ctrl+n")).unwrap_err();
        assert!(error.contains("no action"), "{error}");
    }

    #[test]
    fn v1_action_names_map_onto_v2_actions() {
        // Migration is only as good as this table; every entry must name an
        // action the bare core actually declares.
        for (v1, v2) in V1_ACTIONS {
            assert!(!v1.is_empty() && v2.contains('.'), "{v1} -> {v2}");
        }
    }

    #[test]
    fn a_setting_of_the_wrong_type_is_refused_with_both_types_named() {
        let mut registry = Registry::default();
        registry.declare(
            Vec::new(),
            vec![Setting {
                plugin: "sessions".into(),
                id: "width".into(),
                description: String::new(),
                default: Value::Number(30.0),
                value: Value::Number(30.0),
            }],
        );
        let error = registry
            .set_setting("sessions", "width", Some(Value::Text("wide".into())))
            .unwrap_err();
        assert!(error.contains("number"), "{error}");
        assert!(error.contains("text"), "{error}");
    }

    #[test]
    fn trust_is_recorded_with_the_contents_it_was_granted_for() {
        // Granting, reading back, and the digest that makes drift detectable —
        // the three halves of what the Interface tab shows.
        let home = tempfile::TempDir::new().expect("tempdir");
        std::env::set_var("THURBOX_CONFIG_DIR", home.path());

        let mut registry = Registry::default();
        assert!(!registry.is_trusted("/ui/plugins/mine.lua"));

        registry
            .trust("/ui/plugins/mine.lua", "return {}")
            .expect("trust");
        assert!(registry.is_trusted("/ui/plugins/mine.lua"));
        let recorded = registry
            .trusted_digest("/ui/plugins/mine.lua")
            .expect("a digest is recorded");
        assert_eq!(recorded, super::super::bundled::digest("return {}"));

        // Trusting one file says nothing about another.
        assert!(!registry.is_trusted("/ui/plugins/other.lua"));

        registry.revoke("/ui/plugins/mine.lua").expect("revoke");
        assert!(!registry.is_trusted("/ui/plugins/mine.lua"));
        // Revoking what was never trusted is a state, not a transition.
        registry
            .revoke("/ui/plugins/never.lua")
            .expect("idempotent");

        std::env::remove_var("THURBOX_CONFIG_DIR");
    }

    #[test]
    fn trust_survives_being_written_and_read_back() {
        let home = tempfile::TempDir::new().expect("tempdir");
        std::env::set_var("THURBOX_CONFIG_DIR", home.path());

        let mut written = Registry::default();
        written
            .trust("/ui/plugins/mine.lua", "body")
            .expect("trust");

        let read = Registry::load();
        assert!(
            read.is_trusted("/ui/plugins/mine.lua"),
            "a decision that does not survive a restart is not a decision"
        );

        std::env::remove_var("THURBOX_CONFIG_DIR");
    }

    /// A reload re-declares everything, so a conflict must be reported once
    /// however many times that happens. Appending during `apply_overrides` left
    /// one more copy of every conflict behind per reload, forever.
    #[test]
    fn a_conflict_is_reported_once_however_often_the_interface_reloads() {
        let mut registry = Registry::default();
        let clash = || {
            vec![
                binding("first", "ctrl+g", "first.go", Scope::Global),
                binding("second", "ctrl+g", "second.go", Scope::Global),
            ]
        };

        registry.declare(clash(), Vec::new());
        let first = registry.warnings();
        assert_eq!(first.len(), 1, "one conflict, one warning: {first:?}");

        for _ in 0..5 {
            registry.declare(clash(), Vec::new());
        }
        assert_eq!(
            registry.warnings(),
            first,
            "reloading must not stack another copy of the same conflict"
        );
    }
}
