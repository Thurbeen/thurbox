use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::host::KeyPress;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Global,
    Mod,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Mod => "plugin",
        }
    }

    fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("global") => Scope::Global,
            _ => Scope::Mod,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub owner: String,
    pub action: String,
    pub default_chord: String,
    pub chord: String,
    pub overridden: bool,
    pub description: String,
    pub scope: Scope,
    pub passthrough: bool,
    pub release: bool,
    pub group: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub owner: String,
    pub id: String,
    pub description: String,
    pub default: Value,
    pub value: Value,
    pub list: bool,
    pub choices: Vec<String>,
}

impl Setting {
    pub fn admits(&self, value: &Value) -> bool {
        if value.type_name() != self.default.type_name() {
            return false;
        }
        match value {
            Value::Text(text) if !self.choices.is_empty() => self.choices.contains(text),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pill {
    pub owner: String,
    pub action: String,
    pub label: String,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDecl {
    pub owner: String,
    pub action: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteRow {
    pub owner: String,
    pub action: String,
    pub description: String,
    pub chords: Option<String>,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub chord: String,
    pub kept: String,
    pub shadowed: String,
    pub kept_mod: String,
    pub shadowed_mod: String,
}

impl Conflict {
    pub fn message(&self) -> String {
        format!(
            "{} is claimed by both {} and {}; {} wins",
            self.chord, self.kept, self.shadowed, self.kept
        )
    }
}

pub const QUIT_CHORD: &str = "ctrl+q";

pub const RESERVED: [&str; 5] = [QUIT_CHORD, "f10", "ctrl+h", "ctrl+l", "f12"];

pub const RESERVED_ACTIONS: [(&str, &str, &str); 5] = [
    (QUIT_CHORD, "core.quit", "quit the interface"),
    ("f10", "kernel.reload", "reload the interface"),
    ("ctrl+h", "kernel.focus_previous", "focus the previous pane"),
    ("ctrl+l", "kernel.focus_next", "focus the next pane"),
    ("f12", "kernel.perf_hud", "toggle the performance HUD"),
];

pub fn protected_action(name: &str) -> bool {
    name.starts_with("kernel.")
        || matches!(
            name,
            "core.quit"
                | "session.focus"
                | "help.open"
                | "settings.open"
                | "themes.open"
                | "palette.open"
        )
        || name == super::clipboard::COPY_ACTION
        || name == super::clipboard::PASTE_ACTION
}

/// The session actions a person confirms before they run, on screen or with
/// `thurbox-cli ui confirm`. Each takes a required `session_id` from outside.
pub const GUARDED_SESSION_ACTIONS: [&str; 5] = [
    "sessions.delete",
    "sessions.force_delete",
    "sessions.restart",
    "sessions.restart_fresh",
    "sessions.sync",
];

/// Whether `name` is one of [`GUARDED_SESSION_ACTIONS`].
pub fn is_guarded_session_action(name: &str) -> bool {
    GUARDED_SESSION_ACTIONS.contains(&name)
}

fn default_effect(name: &str) -> &'static str {
    if is_guarded_session_action(name) {
        return "kernel-write";
    }
    match name {
        "sessions.move_up" | "sessions.move_down" | "sessions.undo" | "restore.restore" => {
            "kernel-write"
        }
        _ => "ui-write",
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ActionArgument {
    pub name: String,
    pub kind: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ActionDescriptor {
    pub name: String,
    pub owner: String,
    pub description: String,
    pub scope: String,
    pub arguments: Vec<ActionArgument>,
    pub effect: String,
    pub destructive: bool,
    pub available: bool,
    pub chords: Vec<String>,
}

impl ActionDescriptor {
    pub fn argument(&self, name: &str) -> Option<&ActionArgument> {
        self.arguments.iter().find(|argument| argument.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDecl {
    pub owner: String,
    pub name: String,
    pub description: String,
    pub scope: Scope,
    pub arguments: Vec<ActionArgument>,
    pub effect: String,
    pub destructive: bool,
}

#[derive(Default)]
pub struct Registry {
    bindings: Vec<Binding>,
    settings: Vec<Setting>,
    pills: Vec<Pill>,
    commands: Vec<CommandDecl>,
    action_declarations: Vec<ActionDecl>,
    conflicts: Vec<Conflict>,
    binding_overrides: BTreeMap<String, String>,
    setting_overrides: BTreeMap<String, Value>,
    pill_overrides: BTreeMap<String, serde_json::Value>,
    disabled: BTreeSet<String>,
    trusted: BTreeMap<String, Granted>,
    load_warnings: Vec<String>,
    origin: Origin,
    version: u64,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
    #[default]
    Detached,
    File,
}

impl Registry {
    pub fn load() -> Self {
        let mut registry = Self {
            origin: Origin::File,
            ..Self::default()
        };
        let (bindings, settings, pills, mut warnings) = read_overrides();
        registry.binding_overrides = bindings;
        registry.setting_overrides = settings;
        registry.pill_overrides = pills;
        match read_decisions() {
            Decisions::Absent => {}
            Decisions::Read(trusted, disabled, notes) => {
                registry.trusted = trusted;
                registry.disabled = disabled;
                warnings.extend(notes);
            }
            // `config validate` names the file.
            Decisions::Unreadable(note) => warnings.push(note),
        }

        registry.load_warnings = warnings;
        registry
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn mark_changed(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn declare(&mut self, bindings: Vec<Binding>, settings: Vec<Setting>) {
        self.mark_changed();
        self.declare_all(bindings, settings, Vec::new());
    }

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

    pub fn declare_commands(&mut self, commands: Vec<CommandDecl>) {
        self.mark_changed();
        self.commands = commands;
        self.apply_overrides();
        self.conflicts.clear();
        self.detect_conflicts();
    }

    pub fn declare_action_metadata(&mut self, declarations: Vec<ActionDecl>) {
        self.mark_changed();
        self.action_declarations = declarations
            .into_iter()
            .filter(|entry| !protected_action(&entry.name))
            .collect();
    }

    fn apply_overrides(&mut self) {
        self.bind_overridden_commands();
        for binding in &mut self.bindings {
            // A synthesised binding has no default to fall back to; it exists
            // only while its override does.
            if binding.default_chord.is_empty() {
                continue;
            }
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
            let key = format!("{}.{}", setting.owner, setting.id);
            if let Some(value) = self.setting_overrides.get(&key) {
                if setting.admits(value) {
                    setting.value = value.clone();
                }
            }
        }
        for pill in &mut self.pills {
            let overridden = self
                .pill_overrides
                .get(&pill.action)
                .and_then(serde_json::Value::as_i64);
            if let Some(priority) = overridden {
                pill.priority = priority;
            }
        }
    }

    fn bind_overridden_commands(&mut self) {
        self.bindings
            .retain(|binding| !binding.default_chord.is_empty());
        for command in &self.commands {
            let already_bound = self
                .bindings
                .iter()
                .any(|binding| binding.action == command.action);
            if already_bound {
                continue;
            }
            if let Some(chord) = self.binding_overrides.get(&command.action) {
                self.bindings.push(Binding {
                    owner: command.owner.clone(),
                    action: command.action.clone(),
                    default_chord: String::new(),
                    chord: chord.clone(),
                    overridden: true,
                    description: command.description.clone(),
                    scope: Scope::Mod,
                    passthrough: false,
                    release: false,
                    group: command.owner.clone(),
                });
            }
        }
    }

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
                    kept_mod: kept.owner.clone(),
                    shadowed_mod: shadowed.owner.clone(),
                });
                continue;
            }
            seen.push((binding.chord.clone(), index));
        }
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut all = self.load_warnings.clone();
        all.extend(self.conflicts.iter().map(Conflict::message));
        all
    }

    pub fn resolve(&self, press: &KeyPress, focused_mod: Option<&str>) -> Option<&Binding> {
        let chord = canonical_chord(press);
        let active = |binding: &&Binding| {
            binding.chord == chord
                && match binding.scope {
                    Scope::Global => true,
                    Scope::Mod => focused_mod == Some(binding.owner.as_str()),
                }
        };
        self.bindings
            .iter()
            .find(|binding| active(binding) && binding.overridden)
            .or_else(|| self.bindings.iter().find(active))
    }

    pub fn is_disabled(&self, path: &str) -> bool {
        self.disabled.contains(path)
    }

    pub fn disabled(&self) -> impl Iterator<Item = &str> {
        self.disabled.iter().map(String::as_str)
    }

    pub fn set_disabled(&mut self, path: &str, off: bool) -> Result<bool, String> {
        self.mark_changed();
        if off {
            self.disabled.insert(path.to_string());
        } else {
            self.disabled.remove(path);
        }
        self.persist_decisions()?;
        Ok(off)
    }

    pub fn trusted_digest(&self, path: &str) -> Option<&str> {
        self.trusted.get(path).map(Granted::digest)
    }

    pub fn trusted_pin(&self, path: &str) -> Option<&str> {
        match self.trusted.get(path) {
            Some(Granted::Managed { pin, .. }) => Some(pin.as_str()),
            _ => None,
        }
    }

    pub fn trust(&mut self, path: &str, contents: &str) -> Result<(), String> {
        self.mark_changed();
        self.trusted.insert(
            path.to_string(),
            Granted::Contents(super::bundled::digest(contents)),
        );
        self.persist_decisions()
    }

    pub fn trust_installed(&mut self, path: &str, pin: &str, contents: &str) -> Result<(), String> {
        self.mark_changed();
        self.trusted.insert(
            path.to_string(),
            Granted::Managed {
                pin: pin.to_string(),
                digest: super::bundled::digest(contents),
            },
        );
        self.persist_decisions()
    }

    pub fn revoke(&mut self, path: &str) -> Result<(), String> {
        self.mark_changed();
        self.trusted.remove(path);
        self.persist_decisions()
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn pills(&self) -> &[Pill] {
        &self.pills
    }

    pub fn settings(&self) -> &[Setting] {
        &self.settings
    }

    pub fn commands(&self) -> &[CommandDecl] {
        &self.commands
    }

    pub fn action_catalog(&self) -> Vec<ActionDescriptor> {
        let mut actions = Vec::<ActionDescriptor>::new();
        for (chord, name, description) in RESERVED_ACTIONS {
            actions.push(ActionDescriptor {
                name: name.into(),
                owner: "kernel".into(),
                description: description.into(),
                scope: "global".into(),
                arguments: Vec::new(),
                effect: "ui-write".into(),
                destructive: false,
                available: true,
                chords: vec![chord.into()],
            });
        }
        actions.push(ActionDescriptor {
            name: "kernel.quit".into(),
            owner: "kernel".into(),
            description: "quit (sessions keep running)".into(),
            scope: "global".into(),
            arguments: Vec::new(),
            effect: "ui-write".into(),
            destructive: false,
            available: true,
            chords: vec![QUIT_CHORD.into()],
        });
        for binding in &self.bindings {
            if binding.owner != "kernel" && binding.action.starts_with("kernel.") {
                continue;
            }
            if let Some(existing) = actions
                .iter_mut()
                .find(|entry| entry.name == binding.action)
            {
                if existing.owner == binding.owner {
                    existing.chords.push(binding.chord.clone());
                }
                continue;
            }
            actions.push(ActionDescriptor {
                name: binding.action.clone(),
                owner: binding.owner.clone(),
                description: binding.description.clone(),
                scope: binding.scope.as_str().into(),
                arguments: Vec::new(),
                effect: default_effect(&binding.action).into(),
                destructive: is_guarded_session_action(&binding.action),
                available: true,
                chords: vec![binding.chord.clone()],
            });
        }
        for command in &self.commands {
            if command.owner != "kernel" && command.action.starts_with("kernel.") {
                continue;
            }
            if let Some(existing) = actions
                .iter_mut()
                .find(|entry| entry.name == command.action)
            {
                if existing.owner == command.owner && existing.description.is_empty() {
                    existing.description = command.description.clone();
                }
                continue;
            }
            actions.push(ActionDescriptor {
                name: command.action.clone(),
                owner: command.owner.clone(),
                description: command.description.clone(),
                scope: "global".into(),
                arguments: Vec::new(),
                effect: default_effect(&command.action).into(),
                destructive: is_guarded_session_action(&command.action),
                available: true,
                chords: Vec::new(),
            });
        }
        if let Some(focus) = actions
            .iter_mut()
            .find(|entry| entry.name == "session.focus")
        {
            focus.arguments.push(ActionArgument {
                name: "session_id".into(),
                kind: "string".into(),
                required: true,
            });
        } else {
            actions.push(ActionDescriptor {
                name: "session.focus".into(),
                owner: "kernel".into(),
                description: "focus a session in this interface".into(),
                scope: "global".into(),
                arguments: vec![ActionArgument {
                    name: "session_id".into(),
                    kind: "string".into(),
                    required: true,
                }],
                effect: "ui-write".into(),
                destructive: false,
                available: true,
                chords: Vec::new(),
            });
        }
        for declaration in &self.action_declarations {
            if let Some(existing) = actions
                .iter_mut()
                .find(|entry| entry.name == declaration.name)
            {
                if existing.owner != declaration.owner {
                    continue;
                }
                existing.arguments = declaration.arguments.clone();
                existing.effect = declaration.effect.clone();
                existing.destructive |= declaration.destructive;
                if !declaration.description.is_empty() {
                    existing.description = declaration.description.clone();
                }
            } else {
                actions.push(ActionDescriptor {
                    name: declaration.name.clone(),
                    owner: declaration.owner.clone(),
                    description: declaration.description.clone(),
                    scope: declaration.scope.as_str().into(),
                    arguments: declaration.arguments.clone(),
                    effect: declaration.effect.clone(),
                    destructive: declaration.destructive,
                    available: true,
                    chords: Vec::new(),
                });
            }
        }
        for action in &mut actions {
            if action.owner == "sessions"
                && matches!(
                    action.name.as_str(),
                    "sessions.open" | "sessions.rename" | "sessions.editor" | "sessions.delete"
                )
                && action.argument("session_id").is_none()
            {
                action.arguments.push(ActionArgument {
                    name: "session_id".into(),
                    kind: "string".into(),
                    required: false,
                });
            }
            if is_guarded_session_action(&action.name) {
                action.destructive = true;
                action.effect = "kernel-write".into();
                action.arguments = vec![ActionArgument {
                    name: "session_id".into(),
                    kind: "string".into(),
                    required: true,
                }];
                if action.owner != "sessions" {
                    action.available = false;
                }
            }
        }
        actions
    }

    pub fn palette_rows(&self) -> Vec<PaletteRow> {
        let mut rows: Vec<PaletteRow> = Vec::new();
        for binding in &self.bindings {
            let existing = rows
                .iter_mut()
                .find(|row| row.owner == binding.owner && row.action == binding.action);
            match existing {
                Some(row) => add_chord(row.chords.get_or_insert_with(String::new), &binding.chord),
                None => rows.push(PaletteRow {
                    owner: binding.owner.clone(),
                    action: binding.action.clone(),
                    description: binding.description.clone(),
                    chords: Some(binding.chord.clone()),
                }),
            }
        }
        for command in &self.commands {
            let bound = rows
                .iter_mut()
                .find(|row| row.owner == command.owner && row.action == command.action);
            match bound {
                // The key's row already exists; a command's description wins
                // when the key declared none.
                Some(row) if row.description.is_empty() => {
                    row.description = command.description.clone();
                }
                Some(_) => {}
                None => rows.push(PaletteRow {
                    owner: command.owner.clone(),
                    action: command.action.clone(),
                    description: command.description.clone(),
                    chords: None,
                }),
            }
        }
        rows
    }

    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    pub fn settings_for(&self, mod_name: &str) -> BTreeMap<&str, &Value> {
        self.settings
            .iter()
            .filter(|setting| setting.owner == mod_name)
            .map(|setting| (setting.id.as_str(), &setting.value))
            .collect()
    }

    pub fn rebind(&mut self, action: &str, chord: Option<&str>) -> Result<(), String> {
        self.mark_changed();
        match chord {
            Some(chord) => {
                let chord = normalise_chord(chord);
                if RESERVED.contains(&chord.as_str()) {
                    return Err(format!("{chord} is reserved and cannot be rebound"));
                }
                // A chord-less command is a legal target: binding one is how it
                // becomes a key, which `apply_overrides` then synthesises.
                if !self.bindings.iter().any(|b| b.action == action)
                    && !self.commands.iter().any(|c| c.action == action)
                {
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
        self.persist_overrides()
    }

    pub fn set_setting(
        &mut self,
        mod_name: &str,
        id: &str,
        value: Option<Value>,
    ) -> Result<(), String> {
        self.mark_changed();
        let key = format!("{mod_name}.{id}");
        let Some(declared) = self
            .settings
            .iter()
            .find(|s| s.owner == mod_name && s.id == id)
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
                if !declared.admits(&value) {
                    return Err(format!(
                        "{key} is one of {}, not {}",
                        declared.choices.join(", "),
                        value_to_json(&value)
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
        self.persist_overrides()
    }

    fn persist_overrides(&self) -> Result<(), String> {
        // Nothing was read, so there is nothing to write back: this registry's
        // empty tables are not the user's decisions (see `Origin`). Reported as
        // success because the caller asked for a state, and in memory it has it.
        if self.origin == Origin::Detached {
            return Ok(());
        }
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
            "pills".to_string(),
            serde_json::to_value(&self.pill_overrides).map_err(|e| e.to_string())?,
        );

        let text = serde_json::to_string_pretty(&serde_json::Value::Object(json))
            .map_err(|e| e.to_string())?;
        crate::paths::write_atomically(&path, &text)
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    fn persist_decisions(&self) -> Result<(), String> {
        if self.origin == Origin::Detached {
            return Ok(());
        }
        let Some(path) = decisions_path() else {
            return Err("could not resolve the config directory".to_string());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
        }
        let json = serde_json::json!({
            "trusted": self.trusted,
            "disabled": self.disabled,
        });
        let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        crate::paths::write_atomically(&path, &text)
            .map_err(|e| format!("write {}: {e}", path.display()))
    }
}

fn add_chord(chords: &mut String, chord: &str) {
    if chords.split(" / ").any(|listed| listed == chord) {
        return;
    }
    if !chords.is_empty() {
        chords.push_str(" / ");
    }
    chords.push_str(chord);
}

fn scopes_overlap(a: &Binding, b: &Binding) -> bool {
    match (a.scope, b.scope) {
        (Scope::Mod, Scope::Mod) => a.owner == b.owner,
        _ => true,
    }
}

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
    // Folding them here means a mod declares one chord and it works
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
        if press.ch.is_none_or(|c| c.is_ascii_alphabetic()) {
            parts.push("shift".to_string());
        } else if let Some(c) = press.ch {
            key = c.to_string();
        }
    }
    if key == "backtab" {
        return "shift+tab".to_string();
    }
    // Last modifier, matching `normalise_chord`'s order — the two spellings have
    // to agree or a declared `cmd+c` never matches the press that made it.
    if press.cmd {
        parts.push("cmd".to_string());
    }
    parts.push(key);
    parts.join("+")
}

const SHIFTED_SYMBOLS: [char; 11] = ['!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '_'];

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

pub fn validate_overrides() -> Vec<String> {
    read_overrides().3
}

pub fn validate_decisions() -> Vec<String> {
    match read_decisions() {
        Decisions::Absent => Vec::new(),
        Decisions::Read(_, _, notes) => notes,
        Decisions::Unreadable(note) => vec![note],
    }
}

pub fn decisions_file() -> Option<PathBuf> {
    decisions_path()
}

fn decisions_path() -> Option<PathBuf> {
    crate::paths::config_file().and_then(|config| {
        config
            .parent()
            .map(|dir| dir.join(super::mod_migration::STATE_FILE))
    })
}

enum Decisions {
    Absent,
    Read(BTreeMap<String, Granted>, BTreeSet<String>, Vec<String>),
    Unreadable(String),
}

fn read_decisions() -> Decisions {
    let Some(path) = decisions_path() else {
        return Decisions::Absent;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Decisions::Absent,
        Err(e) => return Decisions::Unreadable(format!("{}: {e}", path.display())),
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(parsed) => {
            let mut notes = Vec::new();
            let trusted = read_trusted(&parsed, &mut notes);
            Decisions::Read(trusted, read_disabled(&parsed), notes)
        }
        Err(e) => Decisions::Unreadable(format!("{}: {e}", path.display())),
    }
}

pub fn overrides_file() -> Option<PathBuf> {
    overrides_path()
}

fn overrides_path() -> Option<PathBuf> {
    crate::paths::config_file().and_then(|config| config.parent().map(|dir| dir.join("ui.json")))
}

type Overrides = (
    BTreeMap<String, String>,
    BTreeMap<String, Value>,
    BTreeMap<String, serde_json::Value>,
    Vec<String>,
);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum Granted {
    Contents(String),
    Managed { pin: String, digest: String },
}

impl Granted {
    fn digest(&self) -> &str {
        match self {
            Granted::Contents(digest) | Granted::Managed { digest, .. } => digest.as_str(),
        }
    }
}

fn path_display(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn read_overrides() -> Overrides {
    let Some(path) = overrides_path() else {
        return Overrides::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Overrides::default();
    };
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            let mut unreadable = Overrides::default();
            unreadable.3.push(format!("{}: {e}", path.display()));
            return unreadable;
        }
    };

    let mut warnings = Vec::new();
    (
        read_bindings(&parsed),
        read_settings(&parsed),
        read_pills(&parsed, &mut warnings),
        warnings,
    )
}

fn read_bindings(parsed: &serde_json::Value) -> BTreeMap<String, String> {
    let mut bindings = BTreeMap::new();
    let Some(map) = parsed.get("bindings").and_then(|v| v.as_object()) else {
        return bindings;
    };
    for (action, chord) in map {
        if let Some(chord) = chord.as_str() {
            bindings.insert(action.clone(), normalise_chord(chord));
        }
    }
    bindings
}

fn read_settings(parsed: &serde_json::Value) -> BTreeMap<String, Value> {
    let mut settings = BTreeMap::new();
    let Some(map) = parsed.get("settings").and_then(|v| v.as_object()) else {
        return settings;
    };
    for (key, value) in map {
        if let Some(value) = json_to_value(value) {
            settings.insert(key.clone(), value);
        }
    }
    settings
}

fn read_pills(
    parsed: &serde_json::Value,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, serde_json::Value> {
    let mut pills = BTreeMap::new();
    let Some(section) = parsed.get("pills") else {
        return pills;
    };
    let Some(map) = section.as_object() else {
        warnings.push("ui.json: pills is not an object of action → priority; ignoring it".into());
        return pills;
    };
    for (action, priority) in map {
        if priority.as_i64().is_none() {
            warnings.push(format!(
                "ui.json: pills[{action}] is not a whole number; ignoring it"
            ));
        }
        pills.insert(action.clone(), priority.clone());
    }
    pills
}

fn read_trusted(
    parsed: &serde_json::Value,
    warnings: &mut Vec<String>,
) -> BTreeMap<String, Granted> {
    let mut trusted = BTreeMap::new();
    let Some(map) = parsed.get("trusted").and_then(|v| v.as_object()) else {
        return trusted;
    };
    for (path, granted) in map {
        if let Some(grant) = read_grant(granted) {
            trusted.insert(path.clone(), grant);
        } else if granted.is_object() {
            // A half-written grant is not one. Dropped rather than guessed at,
            // since guessing here means granting a capability on the strength of
            // a malformed file.
            warnings.push(format!(
                "{}: trusted[{path}] is not a grant; ignoring it",
                path_display(path)
            ));
        }
    }
    trusted
}

fn read_disabled(parsed: &serde_json::Value) -> BTreeSet<String> {
    let Some(list) = parsed.get("disabled").and_then(|v| v.as_array()) else {
        return BTreeSet::new();
    };
    list.iter()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect()
}

fn read_grant(granted: &serde_json::Value) -> Option<Granted> {
    if let Some(digest) = granted.as_str() {
        return Some(Granted::Contents(digest.to_string()));
    }
    let object = granted.as_object()?;
    let pin = object.get("pin").and_then(|v| v.as_str())?;
    let digest = object.get("digest").and_then(|v| v.as_str())?;
    Some(Granted::Managed {
        pin: pin.to_string(),
        digest: digest.to_string(),
    })
}

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

pub fn binding_from(
    mod_name: &str,
    chord: &str,
    action: &str,
    description: &str,
    scope: Option<&str>,
    passthrough: bool,
    group: Option<&str>,
) -> Binding {
    let chord = normalise_chord(chord);
    Binding {
        owner: mod_name.to_string(),
        action: action.to_string(),
        default_chord: chord.clone(),
        chord,
        overridden: false,
        description: description.to_string(),
        scope: Scope::parse(scope),
        passthrough,
        release: false,
        group: group.unwrap_or(mod_name).to_string(),
    }
}

pub fn is_ctrl_letter_chord(chord: &str) -> bool {
    matches!(
        chord.strip_prefix("ctrl+"),
        Some(key) if key.len() == 1 && key.starts_with(|c: char| c.is_ascii_alphabetic())
    )
}

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
            cmd: false,
        }
    }

    fn binding(mod_name: &str, chord: &str, action: &str, scope: Scope) -> Binding {
        Binding {
            owner: mod_name.into(),
            action: action.into(),
            default_chord: normalise_chord(chord),
            chord: normalise_chord(chord),
            overridden: false,
            description: String::new(),
            scope,
            passthrough: false,
            release: false,
            group: mod_name.into(),
        }
    }

    #[test]
    fn palette_rows_merge_an_actions_chords_and_descriptions() {
        let mut registry = Registry::default();
        let mut next = binding("mine", "j", "mine.next", Scope::Mod);
        next.description = "next item".into();
        let mut next_again = binding("mine", "down", "mine.next", Scope::Mod);
        next_again.description = "a second description".into();
        let repeated = binding("mine", "j", "mine.next", Scope::Mod);
        let export = binding("mine", "x", "mine.export", Scope::Mod);
        let theirs = binding("theirs", "k", "mine.next", Scope::Mod);
        registry.declare(vec![next, next_again, repeated, export, theirs], Vec::new());
        let command = |action: &str, description: &str| CommandDecl {
            owner: "mine".into(),
            action: action.into(),
            description: description.into(),
        };
        registry.declare_commands(vec![
            command("mine.export", "export the list"),
            command("mine.next", "not the key's"),
            command("mine.fresh", "chord-less"),
        ]);
        let row =
            |loaded: &str, action: &str, description: &str, chords: Option<String>| PaletteRow {
                owner: loaded.into(),
                action: action.into(),
                description: description.into(),
                chords,
            };
        assert_eq!(
            registry.palette_rows(),
            vec![
                row(
                    "mine",
                    "mine.next",
                    "next item",
                    Some(format!(
                        "{} / {}",
                        normalise_chord("j"),
                        normalise_chord("down")
                    ))
                ),
                row(
                    "mine",
                    "mine.export",
                    "export the list",
                    Some(normalise_chord("x"))
                ),
                row("theirs", "mine.next", "", Some(normalise_chord("k"))),
                row("mine", "mine.fresh", "chord-less", None),
            ]
        );
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
        // ctrl+_; a kitty-protocol one reports ctrl+/ literally. A mod
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
    fn a_mod_scoped_key_fires_only_for_its_mod() {
        let mut registry = Registry::default();
        registry.declare(
            vec![
                binding("sessions", "j", "sessions.next", Scope::Mod),
                binding("themes", "j", "themes.next", Scope::Mod),
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
    fn a_global_claim_overlaps_a_mod_scoped_one() {
        let mut registry = Registry::default();
        registry.declare(
            vec![
                binding("a", "x", "a.x", Scope::Global),
                binding("b", "x", "b.x", Scope::Mod),
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
                owner: "sessions".into(),
                id: "compact".into(),
                description: String::new(),
                default: Value::Bool(false),
                value: Value::Bool(false),
                list: false,
                choices: Vec::new(),
            }],
        );
        assert_eq!(
            registry.settings_for("sessions").get("compact"),
            Some(&&Value::Bool(false))
        );
    }

    #[test]
    fn an_override_of_the_wrong_type_is_ignored() {
        let mut registry = Registry::default();
        registry
            .setting_overrides
            .insert("sessions.compact".into(), Value::Text("yes".into()));
        registry.declare(
            Vec::new(),
            vec![Setting {
                owner: "sessions".into(),
                id: "compact".into(),
                description: String::new(),
                default: Value::Bool(false),
                value: Value::Bool(false),
                list: false,
                choices: Vec::new(),
            }],
        );
        assert_eq!(
            registry.settings_for("sessions").get("compact"),
            Some(&&Value::Bool(false))
        );
    }

    #[test]
    fn an_override_survives_a_reload_that_drops_its_action() {
        let mut registry = Registry::default();
        registry
            .binding_overrides
            .insert("sessions.delete".into(), "ctrl+x".into());

        registry.declare(Vec::new(), Vec::new());
        assert!(registry.bindings().is_empty());

        registry.declare(
            vec![binding("sessions", "d", "sessions.delete", Scope::Mod)],
            Vec::new(),
        );
        assert_eq!(registry.bindings()[0].chord, "ctrl+x");
        assert_eq!(registry.bindings()[0].default_chord, "d");
    }

    #[test]
    fn reserved_chords_cannot_be_rebound() {
        let mut registry = Registry::default();
        registry.declare(
            vec![binding("sessions", "d", "sessions.delete", Scope::Mod)],
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
    fn a_setting_of_the_wrong_type_is_refused_with_both_types_named() {
        let mut registry = Registry::default();
        registry.declare(
            Vec::new(),
            vec![Setting {
                owner: "sessions".into(),
                id: "width".into(),
                description: String::new(),
                default: Value::Number(30.0),
                value: Value::Number(30.0),
                list: false,
                choices: Vec::new(),
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
        assert!(!registry.trusted_digest("/ui/mods/mine.lua").is_some());

        registry
            .trust("/ui/mods/mine.lua", "return {}")
            .expect("trust");
        assert!(registry.trusted_digest("/ui/mods/mine.lua").is_some());
        let recorded = registry
            .trusted_digest("/ui/mods/mine.lua")
            .expect("a digest is recorded");
        assert_eq!(recorded, super::super::bundled::digest("return {}"));

        // Trusting one file says nothing about another.
        assert!(!registry.trusted_digest("/ui/mods/other.lua").is_some());

        registry.revoke("/ui/mods/mine.lua").expect("revoke");
        assert!(!registry.trusted_digest("/ui/mods/mine.lua").is_some());
        // Revoking what was never trusted is a state, not a transition.
        registry.revoke("/ui/mods/never.lua").expect("idempotent");

        std::env::remove_var("THURBOX_CONFIG_DIR");
    }

    #[test]
    fn trust_survives_being_written_and_read_back() {
        let home = tempfile::TempDir::new().expect("tempdir");
        std::env::set_var("THURBOX_CONFIG_DIR", home.path());

        // Loaded rather than default, because only a registry that read the
        // file writes it back — see `Origin`.
        let mut written = Registry::load();
        written.trust("/ui/mods/mine.lua", "body").expect("trust");

        let read = Registry::load();
        assert!(
            read.trusted_digest("/ui/mods/mine.lua").is_some(),
            "a decision that does not survive a restart is not a decision"
        );

        std::env::remove_var("THURBOX_CONFIG_DIR");
    }

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

    #[test]
    fn a_registry_that_was_never_read_does_not_write_over_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _guard = crate::paths::TestPathGuard::new(dir.path());
        let path = overrides_path().expect("a config directory");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let decisions = r#"{"bindings":{},"settings":{"sessions.group_by_repo":false}}"#;
        std::fs::write(&path, decisions).expect("write");
        let store = decisions_path().expect("a store path");
        let grants = r#"{"disabled":["/ui/mods/40_review.lua"],"trusted":{}}"#;
        std::fs::write(&store, grants).expect("write store");

        let mut registry = Registry::default();
        registry.declare(
            vec![binding("sessions", "d", "sessions.delete", Scope::Mod)],
            Vec::new(),
        );
        // Each of the four writers, all reporting success: the state asked for
        // is held in memory, which is what a caller of a detached registry wants.
        registry
            .rebind("sessions.delete", Some("x"))
            .expect("rebind");
        registry
            .set_disabled("/ui/mods/10_sessions.lua", true)
            .expect("disable");
        registry
            .trust("/ui/mods/10_sessions.lua", "-- mine")
            .expect("trust");
        registry.revoke("/ui/mods/10_sessions.lua").expect("revoke");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            decisions,
            "the user's decisions were overwritten by a registry that never read them"
        );
        assert_eq!(
            std::fs::read_to_string(&store).expect("read store"),
            grants,
            "the user's grants were overwritten by a registry that never read them"
        );
    }

    #[test]
    fn a_registry_that_read_the_file_writes_its_changes_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let _guard = crate::paths::TestPathGuard::new(dir.path());

        let mut registry = Registry::load();
        registry.declare(
            vec![binding("sessions", "d", "sessions.delete", Scope::Mod)],
            Vec::new(),
        );
        registry
            .set_disabled("/ui/mods/65_search.lua", true)
            .expect("disable");

        let path = decisions_path().expect("a config directory");
        let written = std::fs::read_to_string(&path).expect("the store was not written");
        assert!(written.contains("65_search.lua"), "{written}");

        // And it comes back on the next launch, which is the whole point.
        let reloaded = Registry::load();
        assert!(reloaded.is_disabled("/ui/mods/65_search.lua"));
    }

    fn pill(mod_name: &str, action: &str, label: &str, priority: i64) -> Pill {
        Pill {
            owner: mod_name.into(),
            action: action.into(),
            label: label.into(),
            priority,
        }
    }

    fn banded() -> (Vec<Binding>, Vec<Pill>) {
        (
            vec![
                binding("kernel", "f6", "kernel.settings", Scope::Global),
                binding("fleetqueue", "f3", "fleetqueue.toggle", Scope::Global),
            ],
            vec![
                pill("kernel", "kernel.settings", "Settings", 60),
                pill("fleetqueue", "fleetqueue.toggle", "Fleet", 50),
            ],
        )
    }

    fn band_labels(registry: &Registry) -> Vec<String> {
        super::super::bands::entries(registry.pills(), registry)
            .into_iter()
            .map(|entry| entry.label)
            .collect()
    }

    fn write_overrides(dir: &std::path::Path, body: &str) {
        std::fs::write(dir.join("ui.json"), body).expect("write ui.json");
    }

    #[test]
    fn a_pills_priority_is_overridden_from_ui_json() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let _guard = crate::paths::TestPathGuard::new(home.path());
        write_overrides(home.path(), r#"{ "pills": { "fleetqueue.toggle": 75 } }"#);

        let mut registry = Registry::load();
        let (bindings, pills) = banded();
        registry.declare_all(bindings, Vec::new(), pills);

        assert_eq!(band_labels(&registry), vec!["Fleet", "Settings"]);
    }

    #[test]
    fn clearing_a_pill_override_restores_the_declared_priority() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let _guard = crate::paths::TestPathGuard::new(home.path());
        write_overrides(home.path(), r#"{ "pills": { "fleetqueue.toggle": 75 } }"#);

        let mut overridden = Registry::load();
        let (bindings, pills) = banded();
        overridden.declare_all(bindings, Vec::new(), pills);
        assert_eq!(band_labels(&overridden), vec!["Fleet", "Settings"]);

        write_overrides(home.path(), r#"{ "pills": {} }"#);
        let mut cleared = Registry::load();
        let (bindings, pills) = banded();
        cleared.declare_all(bindings, Vec::new(), pills);
        assert_eq!(band_labels(&cleared), vec!["Settings", "Fleet"]);
    }

    #[test]
    fn a_pill_override_for_an_unknown_action_is_ignored() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let _guard = crate::paths::TestPathGuard::new(home.path());
        write_overrides(
            home.path(),
            r#"{ "pills": { "nobody.declares.this": 99 } }"#,
        );

        let mut registry = Registry::load();
        let (bindings, pills) = banded();
        registry.declare_all(bindings, Vec::new(), pills);

        assert_eq!(band_labels(&registry), vec!["Settings", "Fleet"]);
    }

    #[test]
    fn a_pill_priority_of_the_wrong_shape_is_refused_and_kept() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let _guard = crate::paths::TestPathGuard::new(home.path());
        write_overrides(
            home.path(),
            r#"{ "pills": { "fleetqueue.toggle": "75", "kernel.settings": 12.5 } }"#,
        );

        let mut registry = Registry::load();
        let (bindings, pills) = banded();
        registry.declare_all(bindings, Vec::new(), pills);

        assert_eq!(band_labels(&registry), vec!["Settings", "Fleet"]);
        let warnings = registry.warnings();
        assert_eq!(warnings.len(), 2, "one per refused entry: {warnings:?}");
        for action in ["fleetqueue.toggle", "kernel.settings"] {
            assert!(
                warnings.iter().any(|w| w.contains(action)),
                "{action} was refused without saying so: {warnings:?}"
            );
        }

        // The write that used to take the evidence with it.
        registry
            .rebind("fleetqueue.toggle", Some("f4"))
            .expect("rebind");
        let written = std::fs::read_to_string(home.path().join("ui.json")).expect("read back");
        assert!(written.contains("\"75\""), "{written}");
        assert!(written.contains("12.5"), "{written}");
        assert_eq!(
            Registry::load().warnings().len(),
            2,
            "the typo must still be there to be reported on the next run"
        );
    }

    #[test]
    fn a_pills_section_of_the_wrong_shape_is_ignored_and_reported() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let _guard = crate::paths::TestPathGuard::new(home.path());
        write_overrides(home.path(), r#"{ "pills": ["fleetqueue.toggle"] }"#);

        let mut registry = Registry::load();
        let (bindings, pills) = banded();
        registry.declare_all(bindings, Vec::new(), pills);

        assert_eq!(band_labels(&registry), vec!["Settings", "Fleet"]);
        // One warning for the section, not one per entry it might have held:
        // the per-entry message carries a `pills[...]` and this one must not be
        // confusable with it.
        let warnings = registry.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("pills is not an object"),
            "the section was dropped without saying so: {warnings:?}"
        );
    }

    #[test]
    fn a_pill_override_survives_a_persist_triggered_by_something_else() {
        let home = tempfile::TempDir::new().expect("tempdir");
        let _guard = crate::paths::TestPathGuard::new(home.path());
        write_overrides(home.path(), r#"{ "pills": { "fleetqueue.toggle": 75 } }"#);

        let mut registry = Registry::load();
        let (bindings, pills) = banded();
        registry.declare_all(bindings, Vec::new(), pills);
        registry
            .trust("/ui/mods/mine.lua", "return {}")
            .expect("trust");

        let mut reloaded = Registry::load();
        let (bindings, pills) = banded();
        reloaded.declare_all(bindings, Vec::new(), pills);
        assert_eq!(band_labels(&reloaded), vec!["Fleet", "Settings"]);
    }
}
