# OS Notifications — Test Plan

This documents how thurbox's OS desktop-notification delivery is tested,
the findings that motivated the WSL fix, and the manual steps to verify
delivery end-to-end on each platform.

## Background: the bug this addresses

On **WSL2** (e.g. Windows Terminal) notifications never appeared. Root
cause: `DBUS_SESSION_BUS_ADDRESS` points at `/run/user/<uid>/bus`, but no
session bus / `org.freedesktop.Notifications` service exists, so
`notify-rust`'s `show()` errors on connect. The only signal was a `warn!`
to the logfile — the user saw **nothing**: no banner, no error, no
diagnostic. The fix auto-detects this case and delivers a Windows toast
via `powershell.exe` instead, records delivery errors, and adds a
`thurbox-cli notify` diagnostic so the failure is never silent again.

## What changed

- **Backend auto-detection** (`notifications::detect_backend` /
  `resolve_backend`): `auto` → dbus on a normal Linux desktop,
  Windows-toast under WSL with no dbus daemon, macOS native banner.
- **`[notifications] backend`** override: `auto | dbus | windows | off`.
- **Non-silent failures**: `notifications::last_error` records the last
  delivery error.
- **`thurbox-cli notify`** diagnostic + `--test`.
- **Body truncation** to 200 chars so a huge OSC message can't overflow.

## Automated tests

Run with `cargo nextest run --all` (or the subset
`cargo nextest run -E 'test(backend) + test(toast) + test(notif) + test(wsl) + test(body)'`).

| Area | Test(s) | What it asserts |
|------|---------|-----------------|
| Backend selection (pure table) | `notifications::tests::auto_*`, `forced_*`, `off_always_disables` | dbus on desktop Linux; Windows-toast under WSL w/o dbus; `None` when WSL lacks powershell; dbus still preferred under WSL if a daemon answers; macOS banner; forced/off variants |
| WSL detection | `proc_version_wsl_marker` | Microsoft/WSL marker in `/proc/version` is recognised; arch kernel is not |
| PowerShell escaping | `powershell_escape_doubles_single_quotes_and_strips_controls` | single quotes doubled, control chars stripped, newlines → spaces |
| Toast script | `toast_script_embeds_escaped_title_and_body`, `toast_script_suppresses_sound_when_off` | title/body embedded + escaped; silent `<audio>` node only when sound off |
| Last-error slot | `last_error_round_trips` | recorded error is retrievable |
| Capability helpers | `backend_capability_helpers` | `is_deliverable` / `supports_click_to_focus` per backend |
| Body truncation | `notify_state::tests::body_is_truncated_when_too_long`, `body_at_limit_is_not_truncated`, `truncate_respects_char_boundaries` | caps at 200 chars + ellipsis; never splits a UTF-8 codepoint |
| Transition / dedup rules (pre-existing) | `notify_state::tests::*` | only fires on real transitions, dedup window, active-suppression, opt-in waiting |
| Settings parsing | `settings::tests::notifications_backend_parses_each_variant`, `notifications_backend_rejects_unknown_value`, `notifications_table_defaults` | `backend` parses all four variants, rejects junk, defaults to `auto` |
| CLI | `cli::notify::tests::*`, `cli::tests::parse_notify_with_and_without_test` | diagnostic shape, headline branches, arg parsing |
| Architecture | `architecture_rules` | `cli` may use `notifications`; `notifications` stays a leaf (`session` + `paths`) |

## Manual verification

### WSL2 (Windows Terminal) — the fixed case

```bash
cargo build --bin thurbox-cli
./target/debug/thurbox-cli notify          # expect backend = windows-toast, deliverable = yes
./target/debug/thurbox-cli notify --test   # expect a Windows toast to appear in the Action Center
```

Verified on `microsoft-standard-WSL2`: `notify` reports
`windows-toast (powershell.exe)` and `--test` shows the toast.

### Forced backends / failure surfacing

```bash
# In settings.toml: [notifications] backend = "dbus"
./target/debug/thurbox-cli notify --test
# On a host with no dbus daemon this now FAILS LOUDLY (prints the dbus
# I/O error and exits non-zero) instead of dropping silently.

# [notifications] backend = "off"
./target/debug/thurbox-cli notify   # headline: "no working backend"; --test refuses
```

### Normal Linux desktop (dbus)

```bash
./target/debug/thurbox-cli notify          # backend = dbus, click-to-focus = yes
./target/debug/thurbox-cli notify --test   # banner via the desktop notification daemon
```

In the TUI, trigger a real `Attention` (let an agent ring the bell / emit
OSC 9) in a non-focused session and confirm the banner fires; click it and
confirm the TUI switches to that session.

### macOS

```bash
./target/debug/thurbox-cli notify --test   # native banner; clicks are ignored (documented)
```

## Notes / limitations

- The Windows-toast path does **not** support click-to-focus (a Windows
  toast can't call back into the WSL process); the banner is informational.
- Notifications only fire while the **TUI is open** (the PTY parser that
  observes the bell isn't running during a headless `automation tick`).
- `powershell.exe` cold-start latency (~1–2 s) is absorbed by the
  background dispatcher thread and never blocks the UI tick.
