//! The built-in **hooks** extension: wires each coding agent's lifecycle hooks
//! to `thurbox-cli session signal` so sessions report `working`/`blocked`/`done`
//! back to thurbox (see the hooks-driven `SessionState`). For **remote**
//! sessions the same hook file is shipped with its commands rewritten to a tmux
//! pane user option (`rewrite_hook_signals_for_remote`) — the local TUI
//! receives those over its control-mode subscription.
//!
//! Unlike user extensions (which are fetched from a source on demand, ADR-20),
//! this one ships **embedded** in the binary and is **auto-activated by default**
//! so the default agent has its hook pre-configured with zero setup. That half
//! is generic and lives in [`super::builtin`]; what is here is the `HOOKS`
//! spec plus the hook rewriting no other built-in needs.
//!
//! Opt out with `thurbox-cli extension deactivate hooks`, which records an
//! opt-out flag so startup self-heal won't resurrect it.

use super::builtin::Builtin;
use super::InstallReport;

/// The extension name (matches `extensions/hooks/extension.toml`).
pub const HOOKS_EXTENSION_NAME: &str = "hooks";

pub(crate) const MANIFEST: &str = include_str!("../../extensions/hooks/extension.toml");
pub(crate) const CLAUDE_SETTINGS: &str = include_str!("../../extensions/hooks/claude.json");
pub(crate) const OPENCODE_PLUGIN: &str = include_str!("../../extensions/hooks/opencode-status.js");
pub(crate) const ANTIGRAVITY_HOOKS: &str =
    include_str!("../../extensions/hooks/antigravity-hooks.json");
pub(crate) const CODEX_HOOKS: &str = include_str!("../../extensions/hooks/codex-hooks.json");
pub(crate) const VIBE_HOOKS: &str = include_str!("../../extensions/hooks/vibe-hooks.toml");
pub(crate) const COPILOT_HOOKS: &str = include_str!("../../extensions/hooks/copilot-hooks.json");
pub(crate) const PI_STATUS: &str = include_str!("../../extensions/hooks/pi-status.ts");
pub(crate) const OMP_STATUS: &str = include_str!("../../extensions/hooks/omp-status.ts");
pub(crate) const GROK_HOOKS: &str = include_str!("../../extensions/hooks/grok-hooks.json");
pub(crate) const KIMI_HOOKS: &str = include_str!("../../extensions/hooks/kimi-hooks.toml");

/// Marker prefix of every thurbox-managed hook command; the state word
/// (`working`/`blocked`/`done`/`idle`) follows it directly.
///
/// Also what a diagnostic looks for to decide whether a payload on disk really
/// is thurbox's wiring rather than a file that merely lives at that path.
pub const SIGNAL_MARKER: &str = "thurbox-cli session signal --state ";

/// How a remote host's rewritten hook commands report state — which
/// multiplexer binary sets the pane user option.
pub(crate) enum RemoteSignalTarget {
    /// POSIX host with real tmux: `tmux set-option -p @thurbox_state <s>`.
    /// Inside a pane tmux resolves its own socket/pane from `$TMUX`/`$TMUX_PANE`.
    Tmux,
    /// Native-Windows host with psmux: `psmux -L <socket> set-option -p …`.
    /// psmux has no `TMUX_TMPDIR`-style socket dir — every `-L <name>` resolves
    /// machine-wide — so the socket is baked into the command at rewrite time.
    Psmux { socket: String },
}

impl RemoteSignalTarget {
    /// The replacement for [`SIGNAL_MARKER`]. Must stay free of `"` and `\` so
    /// the byte-level replace on JSON text stays safe (guarded by a test). The
    /// only variable part is the socket name, sanitized to a conservative
    /// charset at construction ([`remote_signal_target`]).
    fn replacement(&self) -> String {
        let option = crate::session::REMOTE_HOOK_STATE_OPTION;
        match self {
            RemoteSignalTarget::Tmux => format!("tmux set-option -p {option} "),
            RemoteSignalTarget::Psmux { socket } => {
                format!("psmux -L {socket} set-option -p {option} ")
            }
        }
    }
}

/// Rewrite thurbox-managed hook commands for a **remote host**:
/// `thurbox-cli session signal --state <s>` →
/// `<mux> set-option -p @thurbox_state <s>`.
///
/// `thurbox-cli` can't signal from a remote host (it isn't installed there,
/// and it would write the host's own DB — never the one the local TUI reads).
/// A tmux **pane user option** can: inside a pane `set-option -p` needs no
/// socket, pane id, or identity (`$TMUX`/`$TMUX_PANE` are in the pane env),
/// and the local TUI's control-mode connection receives changes through its
/// [`crate::session::REMOTE_HOOK_SUBSCRIPTION`] format subscription (tmux) or
/// pane-option polling (psmux). Applied by the spawn-time materialization
/// (`adapt_agent_args_for_remote`) to every launch arg and every config file
/// it ships, and by `remote_hooks` to the per-agent config-dir payloads.
/// Prefix-replace keeps the state word and whatever trails it (`|| true`,
/// `;; esac; true`) intact; the replacement contains no `"`/`\`, so a
/// byte-level replace on JSON text is safe. Idempotent, and a no-op for
/// marker-free content.
pub(crate) fn rewrite_hook_signals_for_target(
    contents: &str,
    target: &RemoteSignalTarget,
) -> String {
    contents.replace(SIGNAL_MARKER, &target.replacement())
}

/// [`rewrite_hook_signals_for_target`] for a real-tmux POSIX host — the
/// original remote rewrite, kept as the common-case shorthand.
pub(crate) fn rewrite_hook_signals_for_remote(contents: &str) -> String {
    rewrite_hook_signals_for_target(contents, &RemoteSignalTarget::Tmux)
}

/// The signal target for `host`, derived from its multiplexer: a psmux host
/// gets the socket-explicit `psmux` form, everything else (tmux over SSH, tmux
/// inside a WSL distro) the plain `tmux` form.
///
/// The socket name is user-authored (`hosts.toml`) but gets spliced into
/// JSON/JS/TOML hook text by a byte-level replace and tokenized by psmux
/// without quoting, so it is **sanitized** here to a filename-safe charset
/// (`[A-Za-z0-9._-]`). A violating name keeps only its safe characters (warn
/// logged): the signal lands dark on a wrong socket instead of corrupting the
/// shipped config file — and such a name would break every other
/// `-L <socket>` invocation anyway.
pub(crate) fn remote_signal_target(host: &crate::session::HostDef) -> RemoteSignalTarget {
    if host.is_windows() {
        let socket = crate::agent::tmux::host_socket(host);
        let safe: String = socket
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if safe != socket {
            tracing::warn!(
                "host '{}' socket {socket:?} has characters unsafe for the hook \
                 rewrite; using {safe:?}",
                host.name
            );
        }
        RemoteSignalTarget::Psmux {
            socket: if safe.is_empty() {
                crate::agent::tmux::TMUX_SOCKET.to_string()
            } else {
                safe
            },
        }
    } else {
        RemoteSignalTarget::Tmux
    }
}

/// Whether agent status hooks are wired — i.e. the user has not opted out of
/// the built-in extension that installs them. The launch paths consult this
/// before injecting an agent's hook args, so an opted-out profile spawns an
/// agent with no `--settings` rather than one pointing at a file thurbox no
/// longer maintains.
pub fn hooks_enabled(db: &crate::storage::Database) -> bool {
    !db.builtin_extension_opted_out(HOOKS_EXTENSION_NAME)
        .unwrap_or(false)
}

/// The built-in hooks extension: every embedded asset, and the home under this
/// build's config dir (`~/.config/thurbox/hooks`, or `~/.config/thurbox-dev/hooks`
/// for a dev build) that the injected `--settings` path has to point inside.
pub(crate) static HOOKS: Builtin = Builtin {
    name: HOOKS_EXTENSION_NAME,
    blurb: "agent status hooks",
    assets: &[
        ("extension.toml", MANIFEST),
        ("claude.json", CLAUDE_SETTINGS),
        ("opencode-status.js", OPENCODE_PLUGIN),
        ("antigravity-hooks.json", ANTIGRAVITY_HOOKS),
        ("codex-hooks.json", CODEX_HOOKS),
        ("vibe-hooks.toml", VIBE_HOOKS),
        ("copilot-hooks.json", COPILOT_HOOKS),
        ("pi-status.ts", PI_STATUS),
        ("omp-status.ts", OMP_STATUS),
        ("grok-hooks.json", GROK_HOOKS),
        ("kimi-hooks.toml", KIMI_HOOKS),
    ],
    home_dir: "hooks",
    notices: hooks_notices,
};

/// What an ensure of the hooks extension is worth saying: which agents got their
/// launch args patched, and how many wrote a file into their own config dir.
fn hooks_notices(report: &InstallReport) -> Vec<String> {
    let mut msgs = Vec::new();
    if !report.agents_patched.is_empty() {
        msgs.push(format!(
            "hooks: wired agent hooks for {}",
            report.agents_patched.join(", ")
        ));
    }
    if !report.external_files_written.is_empty() {
        // One per agent whose own config dir we drop a file into (opencode's
        // plugin, vibe's hooks.toml) — only those present.
        msgs.push(format!(
            "hooks: installed {} agent hook file(s)",
            report.external_files_written.len()
        ));
    }
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- rewrite_hook_signals_for_remote tests ---

    #[test]
    fn remote_rewrite_replaces_every_signal_command() {
        let rewritten = rewrite_hook_signals_for_remote(CLAUDE_SETTINGS);
        // No local CLI reference survives, every state maps to the pane option.
        assert!(!rewritten.contains("thurbox-cli"));
        for state in ["idle", "working", "blocked", "done"] {
            assert!(
                rewritten.contains(&format!("tmux set-option -p @thurbox_state {state}")),
                "missing rewritten {state} command"
            );
        }
        // The surrounding hook shape (`|| true`, the blocked `case`) survives
        // the prefix replace, and the result is still valid JSON with all five
        // hook events.
        assert!(rewritten.contains("tmux set-option -p @thurbox_state idle || true"));
        assert!(rewritten.contains("tmux set-option -p @thurbox_state blocked ;;"));
        let json: serde_json::Value = serde_json::from_str(&rewritten).expect("still valid JSON");
        let hooks = json.get("hooks").and_then(|h| h.as_object()).unwrap();
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "Notification",
            "Stop",
        ] {
            assert!(hooks.contains_key(event), "missing hook event {event}");
        }
    }

    #[test]
    fn remote_rewrite_covers_the_config_dir_payloads_and_keeps_them_parseable() {
        // A remote grok/kimi session reports through the tmux pane option, so
        // the rewrite has to survive each payload's own syntax — JSON string
        // escaping for grok, TOML for kimi.
        // Checked on the *commands*, not the raw text: a payload may mention
        // `thurbox-cli` in a comment (kimi's does, explaining the ownership
        // marker), and a comment is not something the agent runs.
        let grok = rewrite_hook_signals_for_remote(GROK_HOOKS);
        let grok_doc: serde_json::Value =
            serde_json::from_str(&grok).expect("grok stays valid JSON");
        for command in json_hook_commands(&grok_doc) {
            assert!(
                !command.contains("thurbox-cli"),
                "grok command still calls the local CLI: {command}"
            );
        }

        let kimi = rewrite_hook_signals_for_remote(KIMI_HOOKS);
        let kimi_doc: toml::Value = toml::from_str(&kimi).expect("kimi stays valid TOML");
        for hook in kimi_doc["hooks"].as_array().expect("[[hooks]]") {
            let command = hook["command"].as_str().expect("hook has a command");
            assert!(
                !command.contains("thurbox-cli"),
                "kimi command still calls the local CLI: {command}"
            );
        }

        for state in ["idle", "working", "blocked", "done"] {
            let option = format!("tmux set-option -p @thurbox_state {state}");
            assert!(grok.contains(&option), "grok is missing rewritten {state}");
            assert!(kimi.contains(&option), "kimi is missing rewritten {state}");
        }
    }

    #[test]
    fn remote_rewrite_is_idempotent_and_passes_through() {
        let once = rewrite_hook_signals_for_remote(CLAUDE_SETTINGS);
        assert_eq!(rewrite_hook_signals_for_remote(&once), once);
        let unrelated = "default = \"claude\"\n[[agents]]\nname = \"claude\"\n";
        assert_eq!(rewrite_hook_signals_for_remote(unrelated), unrelated);
    }

    #[test]
    fn signal_marker_matches_shipped_hook_commands() {
        // Guard: a future edit to a hook asset that drifts from the marker
        // (e.g. reordering flags) would silently break the remote rewrite.
        // Every `session signal` occurrence in EVERY shipped asset must carry
        // the exact marker prefix — all of them are candidates for the remote
        // rewrite (claude via `--settings`, the rest via `remote_hooks`).
        for (name, asset) in [
            ("claude.json", CLAUDE_SETTINGS),
            ("opencode-status.js", OPENCODE_PLUGIN),
            ("antigravity-hooks.json", ANTIGRAVITY_HOOKS),
            ("codex-hooks.json", CODEX_HOOKS),
            ("vibe-hooks.toml", VIBE_HOOKS),
            ("copilot-hooks.json", COPILOT_HOOKS),
            ("pi-status.ts", PI_STATUS),
            ("omp-status.ts", OMP_STATUS),
            ("grok-hooks.json", GROK_HOOKS),
            ("kimi-hooks.toml", KIMI_HOOKS),
            ("extension.toml", MANIFEST), // aider's literal --notifications-command arg
        ] {
            // Key on the invocation-with-flags form (`thurbox-cli session
            // signal --…`): a bare mention in a comment (ends at a backtick/
            // newline) is fine, but ANY flagged invocation — including one
            // whose flags drifted, e.g. `--quiet --state` — must carry the
            // exact marker prefix, or the remote rewrite silently misses it.
            let occurrences = asset.matches("thurbox-cli session signal --").count();
            assert!(occurrences > 0, "{name} carries no session-signal command");
            assert_eq!(
                asset.matches(SIGNAL_MARKER).count(),
                occurrences,
                "a `thurbox-cli session signal --…` command in {name} doesn't match SIGNAL_MARKER"
            );
        }
        assert_eq!(
            CLAUDE_SETTINGS.matches(SIGNAL_MARKER).count(),
            6,
            "claude.json hook count changed — review the rewrite"
        );
    }

    #[test]
    fn psmux_target_rewrite_embeds_socket_and_stays_json_safe() {
        let target = RemoteSignalTarget::Psmux {
            socket: "thurbox".into(),
        };
        // The replacement must never contain `"`/`\` — it is spliced into JSON
        // strings by a byte-level replace.
        assert!(!target.replacement().contains(['"', '\\']));
        let rewritten = rewrite_hook_signals_for_target(CLAUDE_SETTINGS, &target);
        assert!(!rewritten.contains("thurbox-cli"));
        for state in ["idle", "working", "blocked", "done"] {
            assert!(
                rewritten.contains(&format!(
                    "psmux -L thurbox set-option -p @thurbox_state {state}"
                )),
                "missing rewritten {state} command"
            );
        }
        // Still valid JSON, and idempotent.
        serde_json::from_str::<serde_json::Value>(&rewritten).expect("still valid JSON");
        assert_eq!(
            rewrite_hook_signals_for_target(&rewritten, &target),
            rewritten
        );
    }

    #[test]
    fn remote_signal_target_sanitizes_a_hostile_socket_name() {
        // The socket is user-authored hosts.toml text spliced into JSON by a
        // byte-level replace — unsafe characters must never survive into the
        // replacement.
        let host = crate::session::HostDef {
            name: "winbox".into(),
            destination: "user@winbox".into(),
            multiplexer: Some("psmux".into()),
            socket: Some("we\"ird sock\\et".into()),
            ..Default::default()
        };
        match remote_signal_target(&host) {
            RemoteSignalTarget::Psmux { socket } => assert_eq!(socket, "weirdsocket"),
            RemoteSignalTarget::Tmux => panic!("psmux host must get the psmux target"),
        }

        // An all-invalid socket falls back to the compile-time default rather
        // than emitting `psmux -L  set-option …`.
        let host = crate::session::HostDef {
            socket: Some("\"\\ ".into()),
            ..host
        };
        match remote_signal_target(&host) {
            RemoteSignalTarget::Psmux { socket } => {
                assert_eq!(socket, crate::agent::tmux::TMUX_SOCKET)
            }
            RemoteSignalTarget::Tmux => panic!("psmux host must get the psmux target"),
        }
    }

    #[test]
    fn embedded_assets_are_present() {
        assert!(MANIFEST.contains("name = \"hooks\""));
        assert!(CLAUDE_SETTINGS.contains("session signal --state working"));
        // The opencode plugin must carry the managed marker so uninstall can
        // safely remove it (see `is_user_modified`).
        assert!(OPENCODE_PLUGIN.contains("thurbox `extension install`"));
        // codex's hooks.json reports the full idle/working/done range.
        assert!(CODEX_HOOKS.contains("session signal --state idle"));
        // The vibe payload carries the signal marker (prune) and the managed
        // marker (external-file uninstall, see `is_user_modified`).
        assert!(VIBE_HOOKS.contains("thurbox-cli session signal"));
        assert!(VIBE_HOOKS.contains("thurbox `extension install`"));
        // The copilot payload carries the signal command and the managed marker
        // (external-file uninstall, see `is_user_modified`).
        assert!(COPILOT_HOOKS.contains("thurbox-cli session signal"));
        assert!(COPILOT_HOOKS.contains("thurbox `extension install`"));
        // The pi payload is a TypeScript extension dropped into pi's extensions
        // dir; it carries the signal command and the managed marker (external-
        // file uninstall, see `is_user_modified`).
        assert!(PI_STATUS.contains("thurbox-cli session signal"));
        assert!(PI_STATUS.contains("thurbox `extension install`"));
        // The omp payload mirrors pi's shape but recognizes OMP's `ask` tool (and
        // upstream pi's `ask_user_question`) as the blocking edge.
        assert!(OMP_STATUS.contains("thurbox-cli session signal"));
        assert!(OMP_STATUS.contains("thurbox `extension install`"));
        assert!(OMP_STATUS.contains("\"ask\""));
        // grok's standalone file carries the signal command and the managed
        // marker (external-file uninstall, see `is_user_modified`).
        assert!(GROK_HOOKS.contains("thurbox-cli session signal"));
        assert!(GROK_HOOKS.contains("thurbox `extension install`"));
        // kimi's entries are merged into the user's own config.toml, so they
        // are pruned by the signal marker rather than by a managed marker.
        assert!(KIMI_HOOKS.contains("thurbox-cli session signal"));
    }

    #[test]
    fn embedded_manifest_parses_with_grok_and_kimi_wiring() {
        let def: crate::session::ExtensionDef =
            toml::from_str(MANIFEST).expect("embedded manifest parses");

        // grok drops a managed standalone file into its ALWAYS-TRUSTED global
        // hooks dir. A project `<repo>/.grok/hooks` would additionally need the
        // folder granted trust in grok's own trusted_folders.toml, so a drop
        // there would load for nobody who had not already opted in by hand.
        let grok = def
            .external_files
            .iter()
            .find(|f| f.path.contains(".grok"))
            .expect("grok external file present");
        assert_eq!(grok.source_path(), "grok-hooks.json");
        assert_eq!(grok.requires_dir.as_deref(), Some("~/.grok"));
        assert_eq!(grok.path, "~/.grok/hooks/thurbox-status.json");

        // The payload is valid JSON in grok's own (claude-shaped) schema, so a
        // typo can't ship a file grok would reject.
        let grok_payload: serde_json::Value =
            serde_json::from_str(GROK_HOOKS).expect("grok payload is valid JSON");
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Notification",
            "Stop",
        ] {
            assert!(
                grok_payload["hooks"][event].is_array(),
                "grok hook event {event} missing"
            );
        }
        // grok refuses to load a hook file whose command references `$VAR`
        // without an inline `:-default` — silently, taking the whole file with
        // it. Every command here must therefore stay `$`-free, which is why the
        // blocked edge pipes stdin through `grep` rather than reusing claude's
        // `case "$(cat)"`.
        for command in json_hook_commands(&grok_payload) {
            assert!(
                !command.contains('$'),
                "grok hook command references a variable: {command}"
            );
        }

        // kimi has no drop-in hooks dir — its hooks live in the `[[hooks]]`
        // array of the user's own ~/.kimi-code/config.toml — so it is a
        // reversible TOML merge, never a managed file that would clobber the
        // user's whole configuration.
        let kimi = def
            .config_merges
            .iter()
            .find(|m| m.path.contains(".kimi-code"))
            .expect("kimi config merge present");
        assert_eq!(kimi.source_path(), "kimi-hooks.toml");
        assert_eq!(kimi.requires_dir.as_deref(), Some("~/.kimi-code"));
        assert_eq!(kimi.format, crate::session::ConfigMergeFormat::Toml);
        assert!(
            def.external_files.iter().all(|f| !f.path.contains(".kimi")),
            "kimi's config.toml must never be written as a whole file"
        );

        // The payload is valid TOML, and every entry carries ONLY the four keys
        // kimi accepts: a fifth makes kimi refuse to load the entire config
        // file, which would break the user's agent rather than merely leave it
        // unreported.
        let kimi_payload: toml::Value =
            toml::from_str(KIMI_HOOKS).expect("kimi payload is valid TOML");
        let kimi_hooks = kimi_payload["hooks"]
            .as_array()
            .expect("kimi payload declares a [[hooks]] table array");
        assert!(!kimi_hooks.is_empty());
        let allowed = ["event", "command", "matcher", "timeout"];
        let mut events = Vec::new();
        for hook in kimi_hooks {
            let table = hook.as_table().expect("hook entry is a table");
            for key in table.keys() {
                assert!(
                    allowed.contains(&key.as_str()),
                    "kimi hook entry carries `{key}`, which is not one of {allowed:?}"
                );
            }
            let event = table["event"].as_str().expect("hook has an event");
            assert!(
                table["command"].as_str().is_some_and(|c| !c.is_empty()),
                "kimi hook `{event}` has a non-empty command"
            );
            events.push((event.to_string(), table["command"].as_str().unwrap()));
        }
        // The block edge is structured (a real permission event) and has a real
        // clearing event, unlike claude's text-matched Notification.
        let mapped = |event: &str| -> String {
            events
                .iter()
                .find(|(e, _)| e == event)
                .unwrap_or_else(|| panic!("kimi hook for {event} missing"))
                .1
                .to_string()
        };
        assert!(mapped("PermissionRequest").contains("--state blocked"));
        assert!(mapped("PermissionResult").contains("--state working"));
        assert!(mapped("SessionStart").contains("--state idle"));
        assert!(mapped("Stop").contains("--state done"));
    }

    /// kimi's entries are merged into a file the user also writes hooks in, so
    /// uninstall has to tell ours from theirs. It does that by the ownership
    /// comment on each entry (`agent::toml_merge`) — an entry shipped without
    /// one would be installed and then never pruned, and an update would stack a
    /// second copy beside it.
    #[test]
    fn every_kimi_entry_carries_the_ownership_marker() {
        let doc: toml_edit::DocumentMut = KIMI_HOOKS.parse().expect("kimi payload is valid TOML");
        let entries = doc["hooks"]
            .as_array_of_tables()
            .expect("[[hooks]] table array");
        assert!(!entries.is_empty());
        for entry in entries.iter() {
            let event = entry["event"].as_str().unwrap_or("<no event>");
            let prefix = entry
                .decor()
                .prefix()
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            assert!(
                prefix.contains(super::super::extensions::MANAGED_MARKER),
                "kimi hook `{event}` ships without the ownership comment, so \
                 uninstall would leave it behind"
            );
        }

        // And the marker really does round-trip through a render + re-parse,
        // which is what makes it usable as ownership at uninstall time.
        let reparsed: toml_edit::DocumentMut = doc.to_string().parse().expect("re-parses");
        assert_eq!(
            reparsed["hooks"].as_array_of_tables().unwrap().len(),
            entries.len()
        );
        for entry in reparsed["hooks"].as_array_of_tables().unwrap().iter() {
            assert!(entry
                .decor()
                .prefix()
                .and_then(|d| d.as_str())
                .is_some_and(|p| p.contains(super::super::extensions::MANAGED_MARKER)));
        }
    }

    #[test]
    fn embedded_manifest_parses_with_codex_vibe_and_antigravity_wiring() {
        // Parse the embedded manifest exactly as the installer does — this guards
        // the codex + antigravity config_merges (and the vibe external file) from
        // silently breaking the build.
        let def: crate::session::ExtensionDef =
            toml::from_str(MANIFEST).expect("embedded manifest parses");

        // codex now JSON-merges a claude-shaped hooks.json into ~/.codex/hooks.json
        // (idle/working/done) rather than the old `-c notify=…` agent patch.
        let codex = def
            .config_merges
            .iter()
            .find(|m| m.path.contains(".codex"))
            .expect("codex config merge present");
        assert_eq!(codex.source_path(), "codex-hooks.json");
        assert_eq!(codex.requires_dir.as_deref(), Some("~/.codex"));
        assert!(
            def.agent_patches.iter().all(|p| p.name != "codex"),
            "codex should no longer be wired via an agent patch"
        );

        // The codex payload is valid JSON, claude-shaped, and carries the marker.
        let codex_payload: serde_json::Value =
            serde_json::from_str(CODEX_HOOKS).expect("codex payload is valid JSON");
        assert!(codex_payload["hooks"]["SessionStart"].is_array());
        assert!(codex_payload["hooks"]["Stop"].is_array());
        assert!(CODEX_HOOKS.contains("thurbox-cli session signal"));

        // vibe drops a managed hooks.toml into ~/.vibe/ (guarded by requires_dir).
        let vibe = def
            .external_files
            .iter()
            .find(|f| f.path.contains(".vibe"))
            .expect("vibe external file present");
        assert_eq!(vibe.source_path(), "vibe-hooks.toml");
        assert_eq!(vibe.requires_dir.as_deref(), Some("~/.vibe"));

        // The vibe payload is valid TOML with at least one hook entry, so a
        // typo can't ship a file vibe would reject.
        let vibe_payload: toml::Value =
            toml::from_str(VIBE_HOOKS).expect("vibe payload is valid TOML");
        let vibe_hooks = vibe_payload["hooks"]
            .as_array()
            .expect("vibe payload declares a [[hooks]] table array");
        assert!(!vibe_hooks.is_empty(), "vibe payload has >=1 hook");
        // Vibe 2.21.0's `HookConfig` requires `name` + `type` and accepts only
        // `pre_tool` / `post_tool` / `post_agent` — the old shipped schema used
        // invented `event = "before_tool"/"after_turn"/"notification"` names
        // that vibe silently rejected (every entry failed validation), so
        // status never reported. Guard the real schema: every entry has a
        // `name`, a valid `type`, a `command`, and maps to working/done only
        // (vibe has no permission/notification hook, so no `blocked`).
        let valid_types = ["pre_tool", "post_tool", "post_agent"];
        for hook in vibe_hooks {
            let name = hook["name"].as_str().expect("vibe hook has a name");
            let htype = hook["type"].as_str().expect("vibe hook has a type");
            assert!(
                valid_types.contains(&htype),
                "vibe hook `{name}` has type `{htype}`, expected one of {valid_types:?}"
            );
            assert!(
                hook["command"].as_str().is_some_and(|c| !c.is_empty()),
                "vibe hook `{name}` has a non-empty command"
            );
            assert!(
                hook.get("event").is_none(),
                "vibe hook `{name}` uses the old (nonexistent) `event` field"
            );
        }
        let vibe_cmds = vibe_hooks
            .iter()
            .filter_map(|h| h["command"].as_str())
            .collect::<String>();
        assert!(
            vibe_cmds.contains("--state working") && vibe_cmds.contains("--state done"),
            "vibe hooks map pre_tool/post_agent to working/done"
        );
        assert!(
            !vibe_cmds.contains("--state blocked"),
            "vibe has no permission/notification hook, so blocked is not reported"
        );

        // antigravity (agy) shares gemini's ~/.gemini/settings.json for hooks.
        let antigravity = def
            .config_merges
            .iter()
            .find(|m| m.path.contains(".gemini"))
            .expect("antigravity config merge present");
        assert_eq!(antigravity.source_path(), "antigravity-hooks.json");
        assert_eq!(antigravity.requires_dir.as_deref(), Some("~/.gemini"));

        // The antigravity payload is valid JSON and carries the prune marker.
        let payload: serde_json::Value =
            serde_json::from_str(ANTIGRAVITY_HOOKS).expect("antigravity payload is valid JSON");
        // agy 1.0.9 adopted claude's hook schema; guard against a regression back
        // to the gemini-era `BeforeTool`/`AfterAgent` names (which agy never fires,
        // so working/done would silently stop reporting).
        for event in ["SessionStart", "PreToolUse", "Notification", "Stop"] {
            assert!(
                payload["hooks"][event].is_array(),
                "antigravity hook event {event} missing"
            );
        }
        assert!(payload["hooks"]["BeforeTool"].is_null());
        assert!(payload["hooks"]["AfterAgent"].is_null());
        assert!(ANTIGRAVITY_HOOKS.contains("thurbox-cli session signal"));

        // copilot drops a managed standalone file into ~/.copilot/hooks/ (guarded
        // by requires_dir; the hooks/ subdir is created on write).
        let copilot = def
            .external_files
            .iter()
            .find(|f| f.path.contains(".copilot"))
            .expect("copilot external file present");
        assert_eq!(copilot.source_path(), "copilot-hooks.json");
        assert_eq!(copilot.requires_dir.as_deref(), Some("~/.copilot"));

        // The copilot payload is valid JSON using copilot's own event schema, so a
        // typo can't ship a file copilot would reject.
        let copilot_payload: serde_json::Value =
            serde_json::from_str(COPILOT_HOOKS).expect("copilot payload is valid JSON");
        for event in [
            "sessionStart",
            "userPromptSubmitted",
            "preToolUse",
            "notification",
            "agentStop",
        ] {
            assert!(
                copilot_payload["hooks"][event].is_array(),
                "copilot hook event {event} missing"
            );
        }
    }

    #[test]
    fn hooks_home_derives_from_build_config_dir() {
        // Home must track the resolved config dir (so a dev build lands under
        // `thurbox-dev`, not the release tree) — never a hardcoded path.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let home = HOOKS.home().expect("home resolves");
        let expected = crate::paths::config_file()
            .unwrap()
            .parent()
            .unwrap()
            .join("hooks");
        assert_eq!(std::path::Path::new(&home), expected);
    }

    #[test]
    fn opt_out_skips_install() {
        let db = crate::storage::Database::open_in_memory().unwrap();
        db.set_builtin_extension_optout(HOOKS_EXTENSION_NAME, true)
            .unwrap();
        // With opt-out set, ensure is a no-op (no install attempted).
        assert!(HOOKS.ensure(&db).is_empty());
    }
    /// How a payload has to be read to find out what it signals.
    #[derive(Clone, Copy)]
    enum PayloadKind {
        /// An agent's hook JSON: every hook entry carries a shell command.
        Json,
        /// Vibe's `[[hooks]]` TOML: same, one `command` per entry.
        Toml,
        /// A JS/TS extension: the states are the literals handed to the one
        /// helper that builds the signal command.
        Script,
    }

    /// The distinct states a payload's *hook commands* actually signal, read
    /// out of the payload's own structure rather than scanned for anywhere in
    /// its text.
    ///
    /// A raw substring scan cannot tell a signalled state from a state word in
    /// a comment, an identifier, or a constant that is declared and never used
    /// — it flips the assertion in either direction on an edit that changes no
    /// behaviour. So each shape is normalised to the same thing: the set of
    /// shell commands the agent will run, and the state each one signals.
    fn states_signalled(payload: &str, kind: PayloadKind) -> Vec<String> {
        let commands = match kind {
            PayloadKind::Json => {
                let doc: serde_json::Value =
                    serde_json::from_str(payload).expect("payload is valid JSON");
                json_hook_commands(&doc)
            }
            PayloadKind::Toml => {
                let doc: toml::Value = toml::from_str(payload).expect("payload is valid TOML");
                doc["hooks"]
                    .as_array()
                    .expect("[[hooks]] table array")
                    .iter()
                    .filter_map(|h| h.get("command")?.as_str().map(str::to_string))
                    .collect()
            }
            // The script builds its command from a fixed prefix plus the state
            // it was handed, so reconstructing it from the reporter's arguments
            // is the same command the agent runs.
            PayloadKind::Script => script_reported_states(payload)
                .into_iter()
                .map(|state| format!("{SIGNAL_MARKER}{state}"))
                .collect(),
        };
        let mut states: Vec<String> = commands
            .iter()
            .filter_map(|command| {
                let rest = command.split(SIGNAL_MARKER).nth(1)?;
                Some(
                    rest.split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .filter(|state| !state.is_empty())
            .collect();
        states.sort();
        states.dedup();
        states
    }

    /// Every shell command a hook JSON payload carries, wherever the agent's
    /// own schema puts it: `command` (claude/codex/antigravity) and the
    /// `bash`/`powershell` pair copilot splits its command into.
    fn json_hook_commands(value: &serde_json::Value) -> Vec<String> {
        match value {
            serde_json::Value::Object(map) => map
                .iter()
                .flat_map(|(key, v)| match (key.as_str(), v.as_str()) {
                    ("command" | "bash" | "powershell", Some(command)) => {
                        vec![command.to_string()]
                    }
                    _ => json_hook_commands(v),
                })
                .collect(),
            serde_json::Value::Array(items) => items.iter().flat_map(json_hook_commands).collect(),
            _ => Vec::new(),
        }
    }

    /// The state literals a JS/TS payload hands to its signal reporter.
    ///
    /// Follows the data flow rather than the vocabulary: it finds the one
    /// helper whose body builds the signal command, then collects the string
    /// literals passed to calls of *that* name. A state word in a comment or a
    /// tool-name constant is not an argument to it and is not counted.
    fn script_reported_states(payload: &str) -> Vec<String> {
        let reporter = ["report", "signal"]
            .into_iter()
            .find(|name| {
                payload
                    .split(&format!("const {name} ="))
                    .nth(1)
                    .is_some_and(|body| {
                        let end = body.find("\n};").unwrap_or(body.len());
                        body[..end].contains(SIGNAL_MARKER.trim_end())
                            || body[..end].contains("SIGNAL +")
                    })
            })
            .expect("payload declares a reporter that builds the signal command");
        let call = format!("{reporter}(");
        payload
            .match_indices(&call)
            .flat_map(|(at, _)| literal_results(balanced_args(&payload[at + call.len()..])))
            .collect()
    }

    /// The text between a call's parentheses, respecting nesting — a call in
    /// the argument list (`BLOCKING_TOOLS.has(…)`) must not end it early.
    fn balanced_args(rest: &str) -> &str {
        let mut depth = 0usize;
        for (at, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => return &rest[..at],
                ')' => depth -= 1,
                _ => {}
            }
        }
        rest
    }

    /// The string literals an argument expression can evaluate to: itself when
    /// it is one, or a ternary's two branches. A literal in the *condition* (a
    /// tool name being compared against) is never the value handed on, so it is
    /// not a state the payload signals.
    fn literal_results(args: &str) -> Vec<String> {
        // The first `?` that opens a ternary rather than continuing `?.` or `??`.
        let bytes = args.as_bytes();
        let ternary = args
            .char_indices()
            .find(|(at, c)| {
                *c == '?'
                    && !matches!(bytes.get(at + 1), Some(b'.' | b'?'))
                    && !matches!(at.checked_sub(1).and_then(|p| bytes.get(p)), Some(b'?'))
            })
            .map(|(at, _)| at);
        let branches: Vec<&str> = match ternary {
            Some(at) => args[at + 1..].split(':').collect(),
            None => vec![args],
        };
        branches
            .iter()
            .filter_map(|branch| {
                branch
                    .trim()
                    .strip_prefix('"')?
                    .strip_suffix('"')
                    .map(str::to_string)
            })
            .collect()
    }

    // --- what the block matcher actually matches -----------------------------

    /// A real `Notification` payload from Claude Code, verbatim, for the
    /// notification that fires ~60s after a turn ends and nobody typed.
    ///
    /// It is the last hook a finished session ever fires, which is what makes a
    /// false `blocked` here permanent: nothing comes after it to overwrite one.
    #[cfg(unix)]
    const IDLE_NOTIFICATION: &str = concat!(
        r#"{"session_id":"d626fdf7","transcript_path":"/home/dev/.claude/projects/-srv-app/x.jsonl","#,
        r#""cwd":"/srv/app","prompt_id":"7da65910","hook_event_name":"Notification","#,
        r#""message":"Claude is waiting for your input","notification_type":"idle_prompt"}"#
    );

    /// The same, from a checkout whose own path carries one of the words the
    /// matcher looks for. Nothing about the notification changed.
    #[cfg(unix)]
    const IDLE_NOTIFICATION_IN_A_PERMISSIONS_REPO: &str = concat!(
        r#"{"session_id":"d626fdf7","transcript_path":"/home/dev/.claude/projects/-srv-permissions-service/x.jsonl","#,
        r#""cwd":"/srv/permissions-service","prompt_id":"7da65910","hook_event_name":"Notification","#,
        r#""message":"Claude is waiting for your input","notification_type":"idle_prompt"}"#
    );

    /// The notification that *is* a block: a tool waiting on approval.
    #[cfg(unix)]
    const PERMISSION_NOTIFICATION: &str = concat!(
        r#"{"session_id":"5e3579cc","transcript_path":"/home/dev/.claude/projects/-srv-app/x.jsonl","#,
        r#""cwd":"/srv/app","prompt_id":"bcf28ba6","hook_event_name":"Notification","#,
        r#""message":"Claude needs your permission","notification_type":"permission_prompt"}"#
    );

    /// Run a payload's own `Notification` command against `stdin`, with the
    /// signal call replaced by `echo` so the test observes the decision instead
    /// of writing to a database.
    ///
    /// The shipped command, not a re-implementation of it: what is under test
    /// is a shell glob against JSON, and the only honest way to check one is to
    /// let a shell run it.
    #[cfg(unix)]
    fn signalled_state(payload: &str, kind: PayloadKind, stdin: &str) -> Option<String> {
        use std::io::Write;

        let doc: serde_json::Value = match kind {
            PayloadKind::Json => serde_json::from_str(payload).expect("valid JSON"),
            _ => unreachable!("only the JSON payloads carry a Notification hook"),
        };
        let command = json_hook_commands(&doc)
            .into_iter()
            .find(|c| c.contains("--state blocked"))
            .expect("payload has a blocked command")
            .replace(SIGNAL_MARKER, "echo ");

        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn sh");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write payload");
        let out = child.wait_with_output().expect("run the hook command");
        let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!printed.is_empty()).then_some(printed)
    }

    /// The regression: the matcher globbed the **whole payload**, so anything
    /// in it carrying one of the words signalled `blocked` — including the
    /// `cwd` and `transcript_path`, which are the operator's own directory
    /// names. A session checked out under a path with "permission" in it went
    /// `blocked` on the idle notification that arrives a minute after every
    /// turn ends, and stayed there, because that notification is the last hook
    /// such a session ever fires.
    ///
    /// The words are matched against the notification's `message` — the
    /// "notification body" the coverage table has always claimed they were.
    #[cfg(unix)]
    #[test]
    fn only_a_permission_notification_signals_blocked() {
        for (agent, payload) in [
            ("claude", CLAUDE_SETTINGS),
            ("antigravity", ANTIGRAVITY_HOOKS),
            ("grok", GROK_HOOKS),
        ] {
            assert_eq!(
                signalled_state(payload, PayloadKind::Json, PERMISSION_NOTIFICATION).as_deref(),
                Some("blocked"),
                "{agent} stopped reporting a real permission prompt"
            );
            assert_eq!(
                signalled_state(payload, PayloadKind::Json, IDLE_NOTIFICATION),
                None,
                "{agent} reports the idle notification as a block"
            );
            assert_eq!(
                signalled_state(
                    payload,
                    PayloadKind::Json,
                    IDLE_NOTIFICATION_IN_A_PERMISSIONS_REPO
                ),
                None,
                "{agent} blocks on a directory name rather than on the notification"
            );
        }
    }

    /// The one thing `session::hook_status`'s coverage table asserts about the
    /// world: that each agent's shipped payload really does signal exactly the
    /// states the table promises. A reader trusting `hook_states_reportable` is
    /// trusting this — so it is checked against the payloads, not maintained by
    /// hand beside them.
    #[test]
    fn the_coverage_table_matches_what_each_payload_actually_signals() {
        use crate::session::hook_status::{HookDelivery, AGENT_HOOK_COVERAGE};

        let payloads: &[(&str, &str, PayloadKind)] = &[
            ("claude", CLAUDE_SETTINGS, PayloadKind::Json),
            ("codex", CODEX_HOOKS, PayloadKind::Json),
            ("antigravity", ANTIGRAVITY_HOOKS, PayloadKind::Json),
            ("opencode", OPENCODE_PLUGIN, PayloadKind::Script),
            ("copilot", COPILOT_HOOKS, PayloadKind::Json),
            ("vibe", VIBE_HOOKS, PayloadKind::Toml),
            ("pi", PI_STATUS, PayloadKind::Script),
            ("omp", OMP_STATUS, PayloadKind::Script),
            ("grok", GROK_HOOKS, PayloadKind::Json),
            ("kimi", KIMI_HOOKS, PayloadKind::Toml),
        ];
        for (agent, payload, kind) in payloads {
            let claimed = AGENT_HOOK_COVERAGE
                .iter()
                .find(|c| c.agent == *agent)
                .unwrap_or_else(|| panic!("{agent} is shipped a payload but claims no coverage"));
            let actual = states_signalled(payload, *kind);
            for state in &actual {
                assert!(
                    crate::session::HOOK_STATES.contains(&state.as_str()),
                    "{agent}: signals '{state}', which is not a thurbox state"
                );
            }
            let mut promised: Vec<String> =
                claimed.states.iter().map(|s| (*s).to_string()).collect();
            promised.sort();
            assert_eq!(
                actual, promised,
                "{agent}: the payload signals {actual:?} but the coverage table promises \
                 {promised:?}"
            );
        }

        // aider ships no payload: its whole wiring is the `--notifications-command`
        // arg patch in the manifest, and `blocked` is all a callback can express.
        let manifest: crate::session::ExtensionDef =
            toml::from_str(MANIFEST).expect("manifest parses");
        let aider = manifest
            .agent_patches
            .iter()
            .find(|p| p.name == "aider")
            .expect("aider is arg-patched");
        assert!(aider
            .append_args
            .iter()
            .any(|a| a.contains("--state blocked")));
        let claimed = AGENT_HOOK_COVERAGE
            .iter()
            .find(|c| c.agent == "aider")
            .expect("aider coverage");
        assert_eq!(claimed.states, &["blocked"]);
        assert_eq!(claimed.delivery, HookDelivery::Args);

        // No orphan rows: everything the table names is wired by the manifest,
        // and every config-dir path it publishes is a path the manifest writes
        // (a diagnostic that stats the wrong file would report hooks missing).
        let manifest_paths: Vec<&str> = manifest
            .config_merges
            .iter()
            .map(|m| m.path.as_str())
            .chain(manifest.external_files.iter().map(|f| f.path.as_str()))
            .collect();
        for c in AGENT_HOOK_COVERAGE {
            let wired = payloads.iter().any(|(a, _, _)| *a == c.agent)
                || manifest.agent_patches.iter().any(|p| p.name == c.agent);
            assert!(wired, "{} claims coverage but nothing wires it", c.agent);
            if c.hook_file_is_in_hooks_home() {
                continue;
            }
            let path = c
                .hook_file
                .unwrap_or_else(|| panic!("{} has no hook file", c.agent));
            assert!(
                manifest_paths.contains(&path),
                "{} publishes {path}, which the manifest does not write",
                c.agent
            );
        }
        // claude's file is the one the `--settings` patch points at, inside the
        // extension's own home rather than the agent's config dir.
        let claude = manifest
            .agent_patches
            .iter()
            .find(|p| p.name == "claude")
            .expect("claude is arg-patched");
        assert!(claude
            .append_args
            .iter()
            .any(|a| a.ends_with("/claude.json")));
    }
}
