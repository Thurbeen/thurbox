use super::*;

/// RMUX's control mode can deliver a pane's `%output` inside a command's
/// reply (captured from `a_missing_rmux_window_never_borrows_a_live_panes_pid`
/// failing intermittently); the answer is still its last line.
#[test]
fn a_pane_pid_answer_is_read_past_pane_output_in_the_reply() {
    let raw = "%output %0 \\033[?1h\\033=\\033[?2004h\n1826732 %1 tb-live\n";
    assert_eq!(parse_pane_pid_answer("%1", raw), Some(1826732));
    assert_eq!(parse_pane_pid_answer("s:=tb-live", raw), Some(1826732));
    assert_eq!(parse_pane_pid_answer("s:=tb-gone", raw), None);
    assert_eq!(
        parse_pane_pid_answer("s:=tb-two words", "7 %2 tb-two words"),
        Some(7)
    );
}

/// An adopted pane's history reaches the reader in pieces, so replaying a
/// long scrollback never holds the parser's lock (and a paint behind it)
/// for the whole of it; every byte still comes, in order, before any live
/// output.
#[test]
fn an_adopt_seed_is_handed_over_in_bounded_pieces_ahead_of_live_output() {
    let seed: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    let live = Box::new(std::iter::once(PaneChunk::Output(b"live".to_vec())));
    let chunks: Vec<Vec<u8>> = seeded(seed.clone(), live)
        .map(|chunk| match chunk {
            PaneChunk::Output(bytes) => bytes,
            other => panic!("expected output, got {other:?}"),
        })
        .collect();
    let (last, history) = chunks.split_last().expect("chunks");
    assert_eq!(last, b"live");
    // 4096 spelled out, so the bound is pinned rather than whatever the
    // constant happens to say.
    assert_eq!(
        history.iter().map(Vec::len).collect::<Vec<_>>(),
        [4096, 4096, 1808],
        "seed pieces"
    );
    assert_eq!(history.concat(), seed);
}

use crate::backend::identity::agent_window_name;
use crate::backend::instance::{host_socket, known_host_socket, learn_host_socket, TMUX_SOCKET};
use crate::backend::tmux_compat::control_mode::PANE_CHANNEL_CAPACITY;

/// A multiplexer for the shared code's own tests, which may name no
/// adapter: tmux's name, so the routes read as a row's would, and POSIX
/// window commands. What it answers beyond those is no adapter's claim.
pub(super) struct TestMux;

pub(super) type TestBackend = Server<TestMux>;

#[cfg(unix)]
#[test]
fn an_unfinished_or_mixed_process_reply_is_not_typed_absence() {
    use std::os::unix::process::ExitStatusExt;
    let backend = TestBackend::local(crate::session::settings::default_scrollback_lines());
    for (status, stdout) in [
        (libc::SIGKILL, Vec::new()),
        (1 << 8, b"partial listing".to_vec()),
    ] {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout,
            stderr: b"can't find session: thurbox".to_vec(),
        };
        assert!(
            !matches!(
                backend.command_failure(&["list-windows"], &output),
                crate::backend::BackendError::Absent(_)
            ),
            "an incomplete answer cannot authorize relaunch"
        );
    }
}

#[test]
fn cells_before_counts_a_wide_character_as_two_columns() {
    assert_eq!(cells_before("❯ hi", 2), "❯ ");
    assert_eq!(cells_before("> 漢字x", 4), "> 漢");
    assert_eq!(cells_before("short", 40), "short");
}

struct TypedKeys;

impl PaneInput for TypedKeys {
    fn send_keys(&self, pane_id: &str, buf: &[u8]) -> Vec<String> {
        control_mode::hex_send_keys_commands(pane_id, buf)
    }
    fn paste(&self, _: &str, _: &str) -> Option<Result<()>> {
        None
    }
}

impl TmuxCompatible for TestMux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Tmux;
    const WINDOW_OPTIONS: bool = true;
    const WINDOW_SETTINGS: bool = true;
    const WINDOW_EVENTS: bool = true;
    const PANE_MONITORING: bool = true;
    const SNAPSHOTS: bool = true;
    const COMMAND_LISTS: bool = true;
    const COMMAND_LIST_SINGLE_REPLY: bool = false;
    const ONE_SHOT_SPAWN_ANSWERS: bool = true;
    const CONDITIONAL_RESIZE: bool = true;
    const SERVER_SCOPE: &str = "-s";
    const DISPLAY_FLAGS: &[&str] = &[];
    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = None;
    fn check_banner(_: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn session_config(_: &str) -> Vec<ConfigOption> {
        Vec::new()
    }
    fn quote(arg: &str) -> String {
        shell_escape(arg)
    }
    fn env_flags(_: &HashMap<String, String>) -> String {
        String::new()
    }
    fn window_command(
        server: &Server<Self>,
        window_name: &str,
        command: &str,
        args: &[String],
        _: &HashMap<String, String>,
    ) -> Result<String> {
        Ok(server.posix_window_command(window_name, command, args))
    }
    fn push_window_program(
        cmd: &mut Command,
        _: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<()> {
        push_posix_window_program(cmd, command, args, env);
        Ok(())
    }
    fn paste_args(_: &str, _: &str) -> Vec<Vec<String>> {
        Vec::new()
    }
    fn deferred_paste_script(_: &str, _: &str, _: &str, _: &str) -> Result<String> {
        Ok(String::new())
    }
    fn pane_input(_: &TmuxTransport, _: &str) -> Arc<dyn PaneInput> {
        Arc::new(TypedKeys)
    }
    fn control_policy(_: &TmuxTransport, _: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control_command: Some("refresh-client -f pause-after=5"),
            implicit_attach_reply: true,
            tagged_blocks: true,
            command_list_single_reply: false,
            subscriptions: true,
            status_poll: None,
            confirm_close: None,
        }
    }
    const HOOK_STATUS: bool = true;

    fn hook_signal_command(_: &Server<Self>) -> String {
        "tmux set-option -p @thurbox_state ".to_string()
    }
}

#[cfg(unix)]
struct UnmonitoredMux;

#[cfg(unix)]
impl TmuxCompatible for UnmonitoredMux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Tmux;
    const WINDOW_OPTIONS: bool = false;
    const WINDOW_SETTINGS: bool = false;
    const WINDOW_EVENTS: bool = false;
    const PANE_MONITORING: bool = false;
    const SNAPSHOTS: bool = false;
    const COMMAND_LISTS: bool = false;
    const COMMAND_LIST_SINGLE_REPLY: bool = false;
    const ONE_SHOT_SPAWN_ANSWERS: bool = false;
    const CONDITIONAL_RESIZE: bool = false;
    const SERVER_SCOPE: &str = "-s";
    const DISPLAY_FLAGS: &[&str] = &[];
    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = None;

    fn check_banner(_: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn session_config(_: &str) -> Vec<ConfigOption> {
        Vec::new()
    }
    fn paste_args(_: &str, _: &str) -> Vec<Vec<String>> {
        Vec::new()
    }
    fn deferred_paste_script(_: &str, _: &str, _: &str, _: &str) -> Result<String> {
        Ok(String::new())
    }
    fn pane_input(_: &TmuxTransport, _: &str) -> Arc<dyn PaneInput> {
        Arc::new(TypedKeys)
    }
    fn control_policy(_: &TmuxTransport, _: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control_command: None,
            implicit_attach_reply: false,
            tagged_blocks: false,
            command_list_single_reply: false,
            subscriptions: false,
            status_poll: None,
            confirm_close: None,
        }
    }
    const HOOK_STATUS: bool = false;
    fn hook_signal_command(_: &Server<Self>) -> String {
        String::new()
    }
}

/// A multiplexer with a version floor: below `2.0` is refused.
#[cfg(unix)]
struct FlooredMux;

#[cfg(unix)]
fn refuse_below_two(version: &str, _: &str) -> Result<()> {
    match version.trim() {
        "1.0" => bail!("1.0 is too old"),
        _ => Ok(()),
    }
}

#[cfg(unix)]
impl TmuxCompatible for FlooredMux {
    const MULTIPLEXER: Multiplexer = Multiplexer::Tmux;
    const WINDOW_OPTIONS: bool = false;
    const WINDOW_SETTINGS: bool = false;
    const WINDOW_EVENTS: bool = false;
    const PANE_MONITORING: bool = false;
    const SNAPSHOTS: bool = false;
    const COMMAND_LISTS: bool = false;
    const COMMAND_LIST_SINGLE_REPLY: bool = false;
    const ONE_SHOT_SPAWN_ANSWERS: bool = false;
    const CONDITIONAL_RESIZE: bool = false;
    const SERVER_SCOPE: &str = "-s";
    const DISPLAY_FLAGS: &[&str] = &[];
    const VERSION_FLOOR: Option<fn(&str, &str) -> Result<()>> = Some(refuse_below_two);

    fn check_banner(_: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn session_config(_: &str) -> Vec<ConfigOption> {
        Vec::new()
    }
    fn paste_args(_: &str, _: &str) -> Vec<Vec<String>> {
        Vec::new()
    }
    fn deferred_paste_script(_: &str, _: &str, _: &str, _: &str) -> Result<String> {
        Ok(String::new())
    }
    fn pane_input(_: &TmuxTransport, _: &str) -> Arc<dyn PaneInput> {
        Arc::new(TypedKeys)
    }
    fn control_policy(_: &TmuxTransport, _: &str) -> ControlPolicy {
        ControlPolicy {
            flow_control_command: None,
            implicit_attach_reply: false,
            tagged_blocks: false,
            command_list_single_reply: false,
            subscriptions: false,
            status_poll: None,
            confirm_close: None,
        }
    }
    const HOOK_STATUS: bool = false;
    fn hook_signal_command(_: &Server<Self>) -> String {
        String::new()
    }
}

/// A stand-in multiplexer with a floor, logging every command it is given
/// beside itself; `answer` is printed as its version, one-shot and over
/// control mode alike. Answers the backend, and how often the version was
/// asked so far.
#[cfg(unix)]
fn floored_mux(root: &std::path::Path, answer: &str) -> (Server<FlooredMux>, impl Fn() -> usize) {
    use std::os::unix::fs::PermissionsExt;

    let mux = root.join("floored-mux");
    std::fs::write(
        &mux,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n\
                   *' -C '*) while IFS= read -r line; do\n\
                       printf '%s\\n' \"$line\" >> \"$0.log\"\n\
                       printf '%%begin 1 1 0\\n'\n\
                       case \"$line\" in *'#{{version}}'*) printf '{answer}' ;; esac\n\
                       printf '%%end 1 1 0\\n'\n\
                     done ;;\n\
                   *' -V'*) printf 'tmux 2.0\\n' ;;\n\
                   *) printf '%s\\n' \"$*\" >> \"$0.log\"\n\
                      case \"$*\" in *'#{{version}}'*) printf '{answer}' ;; esac ;;\n\
                 esac\n"
        ),
    )
    .expect("write fake mux");
    std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let log = root.join("floored-mux.log");
    let asked = move || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .matches("#{version}")
            .count()
    };
    let backend = Server::<FlooredMux>::with_transport(
        TmuxTransport::local(mux.to_string_lossy()),
        "unused",
        "unused",
        "local:tmux",
        crate::session::settings::default_scrollback_lines(),
    );
    (backend, asked)
}

/// An admission is good for the connection it was asked on: a replacement
/// may be to a server started since, on other code, so it is asked again —
/// once, however many panes are then named.
#[cfg(unix)]
#[test]
fn each_new_connection_asks_the_server_its_version_once() {
    let root = tempfile::tempdir().expect("tempdir");
    let (backend, asked) = floored_mux(root.path(), "2.0\\n");

    backend.target("%1").expect("admitted by a one-shot");
    assert_eq!(asked(), 1);
    backend.ensure_control().expect("control client");
    backend.target("%1").expect("admitted on the connection");
    backend.target("%2").expect("still admitted");
    assert_eq!(asked(), 2);
    backend.reconnect_control(None).expect("replaced");
    backend.target("%1").expect("admitted on the replacement");
    assert_eq!(asked(), 3);
}

#[test]
fn only_dotted_numbers_are_a_version_answer() {
    for answer in ["3.3.8", "3.4", "10.0.1"] {
        assert!(is_version_answer(answer), "{answer}");
    }
    for answer in [
        "",
        " ",
        "3",
        "3.",
        ".3",
        "3.3.7a",
        "tmux 3.3.8",
        "3.3.8\n3.3.7",
        "ERROR: x",
        "3.4294967296",
        "+3.3",
    ] {
        assert!(!is_version_answer(answer), "{answer:?}");
    }
}

/// The paint's resize and the input loop's dead-check cannot wait on the
/// version question, so they name a pane only on a connection already
/// admitted: after a reconnect to a server not yet asked, or refused, they
/// send nothing.
#[cfg(unix)]
#[test]
fn the_loops_name_no_pane_on_a_connection_not_admitted() {
    let resized = |root: &std::path::Path| {
        std::fs::read_to_string(root.join("floored-mux.log"))
            .unwrap_or_default()
            .matches("resize-window")
            .count()
    };

    let root = tempfile::tempdir().expect("tempdir");
    let (backend, _asked) = floored_mux(root.path(), "2.0\\n");
    backend.ensure_control().expect("control client");
    backend.resize("%1", 24, 80).expect("skipped, not failed");
    assert!(backend.is_dead("%1").is_err());
    assert_eq!(resized(root.path()), 0, "named before admission");
    backend.target("%1").expect("admitted");
    backend.resize("%1", 24, 80).expect("resized");
    // Sent detached: the writer thread puts it on the wire in its own time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while resized(root.path()) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(resized(root.path()), 1);

    let root = tempfile::tempdir().expect("tempdir");
    let (backend, _asked) = floored_mux(root.path(), "1.0\\n");
    backend.ensure_control().expect("control client");
    assert!(backend.target("%1").is_err());
    backend.resize("%1", 24, 80).expect("skipped, not failed");
    assert!(backend.is_dead("%1").is_err());
    assert_eq!(resized(root.path()), 0, "named to a refused server");
}

/// The admission was the connection's: once it is shut down, the next
/// name asks again rather than ride on it.
#[cfg(unix)]
#[test]
fn a_shut_down_connection_takes_its_admission_with_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let (backend, asked) = floored_mux(root.path(), "2.0\\n");
    backend.ensure_control().expect("control client");
    backend.target("%1").expect("admitted on the connection");
    assert_eq!(asked(), 1);
    backend.shutdown();
    backend.target("%1").expect("asked again with a one-shot");
    assert_eq!(asked(), 2);
}

/// A stand-in control-mode server for [`TestMux`] that answers `%error`
/// with `reply` to any line containing `refuse`, and `%end` otherwise.
#[cfg(unix)]
fn refusing_mux(root: &std::path::Path, refuse: &str, reply: &str) -> TestBackend {
    use std::os::unix::fs::PermissionsExt;

    let mux = root.join("refusing-mux");
    std::fs::write(
        &mux,
        format!(
            "#!/bin/sh\nreply=\"{reply}\"\nprintf '%%begin 1 1 0\\n%%end 1 1 0\\n'\n\
                 while IFS= read -r line; do\n\
                   case \"$line\" in\n\
                     *'{refuse}'*) printf '%%begin 2 2 0\\n%s\\n%%error 2 2 0\\n' \"$reply\" ;;\n\
                     *) printf '%%begin 2 2 0\\n%%end 2 2 0\\n' ;;\n\
                   esac\n\
                 done\n"
        ),
    )
    .expect("write fake mux");
    std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    TestBackend::with_transport(
        TmuxTransport::local(mux.to_string_lossy()),
        "unused",
        "unused",
        "local:tmux",
        crate::session::settings::default_scrollback_lines(),
    )
}

/// A control-mode `%error` is typed by the adapter's words, both on a
/// command and on the flow-control command sent while the connection
/// starts.
#[cfg(unix)]
#[test]
fn a_control_mode_error_is_typed_by_the_adapter() {
    use crate::backend::contract::BackendError;

    let kind = |error: anyhow::Error| match error.downcast::<BackendError>() {
        Ok(BackendError::Absent(_)) => "absent",
        Ok(BackendError::Refused(_)) => "refused",
        Ok(BackendError::Busy(_)) => "busy",
        other => panic!("untyped: {other:?}"),
    };
    let roots: Vec<_> = (0..3)
        .map(|_| tempfile::tempdir().expect("tempdir"))
        .collect();

    let backend = refusing_mux(roots[0].path(), "pause-after", "can't find session: unused");
    let error = backend.ensure_control().expect_err("flow control refused");
    assert_eq!(kind(error), "absent");

    let backend = refusing_mux(roots[1].path(), "kill-window", "can't find window: @9");
    backend.ensure_control().expect("control client");
    let error = backend
        .ctrl_command("kill-window -t @9")
        .expect_err("refused");
    assert_eq!(kind(error), "absent");

    let backend = refusing_mux(
        roots[2].path(),
        "kill-window",
        "unknown command: kill-window",
    );
    backend.ensure_control().expect("control client");
    let error = backend
        .ctrl_command("kill-window -t @9")
        .expect_err("refused");
    assert_eq!(kind(error), "refused");
}

/// Over control mode an answer that is not a version may be a reply that
/// belonged to another command, so it admits nothing.
#[cfg(unix)]
#[test]
fn a_connection_that_answers_no_version_names_no_pane() {
    let root = tempfile::tempdir().expect("tempdir");
    let (backend, _asked) = floored_mux(root.path(), "");
    backend.ensure_control().expect("control client");
    let err = backend.target("%1").unwrap_err().to_string();
    assert!(err.contains("the server did not say its version"), "{err}");
}

#[cfg(unix)]
#[test]
fn an_unmonitored_pane_streams_output_without_refreshing_on_attach_or_detach() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let mux = root.path().join("unmonitored-mux");
    std::fs::write(
            &mux,
            "#!/bin/sh\nwhile IFS= read -r line; do\n\
             printf '%s\\n' \"$line\" >> \"$0.log\"\n\
             case \"$line\" in\n\
               *'refresh-client -A'*) printf '%%begin 1 1 0\\n%%error 1 1 0\\n' ;;\n\
               *' ; '*) printf '%%begin 1 1 0\\n%%end 1 1 0\\n%%begin 1 1 0\\n%%end 1 1 0\\n%%output %%1 streamed\\n' ;;\n\
               *) printf '%%begin 1 1 0\\n%%end 1 1 0\\n' ;;\n\
             esac\n\
             done\n",
        )
        .expect("write fake mux");
    std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let transport = TmuxTransport::local(mux.to_string_lossy());
    let backend = Server::<UnmonitoredMux>::with_transport(
        transport.clone(),
        "unused",
        "unused",
        "local:tmux",
        crate::session::settings::default_scrollback_lines(),
    );
    let control = ControlMode::start(
        &transport,
        "unused",
        "unused",
        "tests",
        &UnmonitoredMux::control_policy(&transport, "unused"),
    )
    .expect("control client");
    *backend.link.control.lock().unwrap() = Some(Arc::new(control));

    let mut pane = backend
        .connect_pane("%1", Some("@1"), 24, 80)
        .expect("attach without monitoring command");
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let output = pane.output.find_map(|chunk| match chunk {
            PaneChunk::Output(bytes) => Some(bytes),
            _ => None,
        });
        let _ = tx.send(output);
    });
    let output = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("streamed output arrived")
        .expect("streamed output read");
    reader.join().expect("reader thread");
    assert_eq!(output, b"streamed");
    backend.detach("%1").expect("detach");
    drop(backend);
    let commands = std::fs::read_to_string(mux.with_extension("log")).expect("command log");
    assert!(commands.contains("resize-window"), "{commands}");
    assert!(!commands.contains("refresh-client -A"), "{commands}");
}

/// Exercise overflow through the subprocess, control reader and bounded
/// pane channel, up to the reader a pane's grid is fed from. Not reading
/// makes the overflow deterministic without depending on how quickly a
/// real tmux batches bytes. Installing what comes out onto a grid is
/// `pane`'s, tested there: this module may not name it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overflowed_pane_is_caught_up_without_blocking_control() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    let root = tempfile::tempdir().expect("tempdir");
    let mux = root.path().join("overflow-mux");
    std::fs::write(
        &mux,
        format!(
            r#"#!/bin/sh
reply() {{
    printf '%%begin 1 1 0\n'
    [ "$#" -eq 0 ] || printf '%s\n' "$1"
    printf '%%end 1 1 0\n'
}}
while IFS= read -r line; do
    case "$line" in
        *'display-message -p flood'*)
            i=0
            while [ "$i" -lt {chunks} ]; do
                printf '%s\n' '%output %1 \033['
                i=$((i + 1))
            done
            printf '%s\n' '%output %1 32mRECOVERED\033[0m'
            reply ;;
        *'capture-pane'*)
            reply '20 4 9 0 0'
            reply "$(printf '\033[32mRECOVERED\033[0m')"
            reply ;;
        *'display-message -p suffix'*)
            printf '%s\n' '%output %1 !'
            reply ;;
        *'resize-window'*)
            reply; reply; reply; reply; reply ;;
        *'#{{pane_id}} #{{window_id}}'*) reply '%1 @1 4 20' ;;
        *) reply ;;
    esac
done
"#,
            chunks = PANE_CHANNEL_CAPACITY + 64,
        ),
    )
    .expect("write control server");
    std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let transport = TmuxTransport::local(mux.to_string_lossy());
    let backend = Arc::new(TestBackend::with_transport(
        transport.clone(),
        "unused",
        "unused",
        "local:tmux",
        crate::session::settings::default_scrollback_lines(),
    ));
    // Shut down even on assertion failure, so the runtime's blocking reader
    // cannot outlive the subprocess that owns its channel.
    struct Shutdown(Arc<TestBackend>);
    impl Drop for Shutdown {
        fn drop(&mut self) {
            self.0.shutdown();
        }
    }
    let _shutdown = Shutdown(Arc::clone(&backend));
    let control = Arc::new(
        ControlMode::start(
            &transport,
            "unused",
            "unused",
            "tests",
            &ControlPolicy {
                flow_control_command: None,
                implicit_attach_reply: false,
                tagged_blocks: true,
                command_list_single_reply: false,
                subscriptions: false,
                status_poll: None,
                confirm_close: None,
            },
        )
        .expect("control client"),
    );
    *backend.link.control.lock().unwrap() = Some(Arc::clone(&control));
    let (reader, _) = backend
        .register_pane("%1", Some("@1"))
        .expect("resident pane");

    control
        .send_command("display-message -p flood")
        .expect("control replies still arrive while the pane channel is full");
    // A barrier after the flood proves that all notifications reached the
    // dispatcher before the consumer is released. No further output is
    // needed to trigger recovery, even if the snapshot itself found no room.
    control
        .send_command("display-message -p barrier")
        .expect("another command is not held behind the flooded pane");

    // Read on a thread of its own, so a reader that is never handed the
    // resync fails the test at its deadline instead of hanging it.
    let (items, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for chunk in reader {
            if items.send(chunk).is_err() {
                return;
            }
        }
    });
    let next = || {
        received
            .recv_timeout(Duration::from_secs(5))
            .expect("overflowed resident pane was never caught up")
    };

    let mut queued = 0;
    let snapshot = loop {
        match next() {
            PaneChunk::Output(bytes) => {
                assert_eq!(bytes, b"\x1b[", "only output from before the gap");
                queued += 1;
            }
            PaneChunk::Resync(snapshot) => break snapshot,
            other => panic!("a resync, which replaces a grid it has, not {other:?}"),
        }
    };
    assert!(queued <= PANE_CHANNEL_CAPACITY, "{queued}");
    assert_eq!(
        *snapshot,
        crate::backend::contract::PaneSnapshot {
            cols: 20,
            rows: 4,
            cursor: (9, 0),
            normal: vec!["\x1b[32mRECOVERED\x1b[0m".into()],
            alternate: None,
        }
    );
    control
        .send_command("display-message -p suffix")
        .expect("output after recovery");
    assert!(
        matches!(next(), PaneChunk::Output(bytes) if bytes == b"!"),
        "output flows again"
    );
}

/// A control server standing in for a host whose link has gone slow: every
/// connection it starts logs `start`, waits out [`STALL`] first when a
/// `.slow` file sits beside it, and answers `list-windows` only after
/// [`STALL`] too. Everything else is answered at once.
#[cfg(unix)]
fn stalling_backend(root: &std::path::Path) -> (Server<UnmonitoredMux>, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let mux = root.join("stalling-mux");
    std::fs::write(
            &mux,
            format!(
                "#!/bin/sh\nprintf 'start\\n' >> \"$0.log\"\n\
                 [ -e \"$0.slow\" ] && sleep {secs}\n\
                 while IFS= read -r line; do\n\
                 printf '%s\\n' \"$line\" >> \"$0.log\"\n\
                 case \"$line\" in\n\
                   list-windows*) sleep {secs}; printf '%%begin 1 1 0\\n%%end 1 1 0\\n' ;;\n\
                   *' ; '*) printf '%%begin 1 1 0\\n%%end 1 1 0\\n%%begin 1 1 0\\n%%end 1 1 0\\n' ;;\n\
                   *) printf '%%begin 1 1 0\\n%%end 1 1 0\\n' ;;\n\
                 esac\n\
                 done\n",
                secs = STALL.as_secs()
            ),
        )
        .expect("write fake mux");
    std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let transport = TmuxTransport::local(mux.to_string_lossy());
    let backend = Server::<UnmonitoredMux>::with_transport(
        transport,
        "unused",
        "unused",
        "local:tmux",
        crate::session::settings::default_scrollback_lines(),
    );
    backend.ensure_control().expect("control client");
    (backend, mux)
}

#[cfg(unix)]
const STALL: std::time::Duration = std::time::Duration::from_secs(3);

/// Block until the fake mux's log satisfies `seen`.
#[cfg(unix)]
fn await_log(mux: &std::path::Path, seen: impl Fn(&str) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !std::fs::read_to_string(mux.with_extension("log")).is_ok_and(|log| seen(&log)) {
        assert!(
            std::time::Instant::now() < deadline,
            "fake mux never got there"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// #1472: the paint path's resize waited for the control lock while
/// another caller's round trip held it, and was refused as busy once
/// [`LOOP_COMMAND_BUDGET`] ran out — every frame, for every pane of a host
/// whose link had gone slow. Commands are already serialised on the
/// connection itself, so a stalled answer must cost nobody else a wait.
#[cfg(unix)]
#[test]
fn a_paint_is_not_held_behind_another_callers_stalled_round_trip() {
    let root = tempfile::tempdir().expect("tempdir");
    let (backend, mux) = stalling_backend(root.path());
    let backend = Arc::new(backend);
    let slow = std::thread::spawn({
        let backend = Arc::clone(&backend);
        move || backend.ctrl_command("list-windows -a")
    });
    await_log(&mux, |log| log.contains("list-windows"));

    let began = std::time::Instant::now();
    let resized = backend.resize("%1", 24, 80);
    let took = began.elapsed();

    assert!(resized.is_ok(), "{resized:?}");
    assert!(took < LOOP_COMMAND_BUDGET / 2, "the paint waited {took:?}");
    slow.join().expect("slow caller").expect("slow answer");
}

/// #1472: a reconnect held the control lock through the new connection's
/// start — an ssh handshake on a remote host — so the paint path stalled
/// for as long as the handshake took.
#[cfg(unix)]
#[test]
fn a_paint_is_not_held_behind_a_slow_reconnect() {
    let root = tempfile::tempdir().expect("tempdir");
    let (backend, mux) = stalling_backend(root.path());
    let backend = Arc::new(backend);
    std::fs::write(mux.with_extension("slow"), "").expect("slow marker");
    let reconnect = std::thread::spawn({
        let backend = Arc::clone(&backend);
        move || backend.reconnect_control(None)
    });
    await_log(&mux, |log| log.matches("start").count() == 2);

    let began = std::time::Instant::now();
    let resized = backend.resize("%1", 24, 80);
    let took = began.elapsed();

    assert!(resized.is_ok(), "{resized:?}");
    assert!(took < LOOP_COMMAND_BUDGET / 2, "the paint waited {took:?}");
    reconnect.join().expect("reconnect").expect("reconnected");
}

/// Another caller's reconnect must not take the connection out from under a
/// command already sent on it: closing it lost the answer, and the caller
/// resent the command on the new connection — a `new-window` that tmux had
/// already run opened a second window.
#[cfg(unix)]
#[test]
fn a_command_in_flight_across_a_reconnect_runs_once_and_is_answered() {
    let root = tempfile::tempdir().expect("tempdir");
    let (backend, mux) = stalling_backend(root.path());
    let backend = Arc::new(backend);
    let first = backend.connection().expect("connection");
    let in_flight = std::thread::spawn({
        let backend = Arc::clone(&backend);
        move || backend.ctrl_command("list-windows -a")
    });
    await_log(&mux, |log| log.contains("list-windows"));

    backend
        .reconnect_control(Some(&first))
        .expect("reconnected");
    let answered = in_flight.join().expect("in-flight caller");

    assert!(answered.is_ok(), "{answered:?}");
    let log = std::fs::read_to_string(mux.with_extension("log")).expect("log");
    assert_eq!(log.matches("list-windows").count(), 1, "{log}");
}

/// A command written whose answer never came may have run, so it is not
/// sent again — the caller hears the failure instead. Only a write that
/// failed outright, which tmux never saw, is retried.
#[cfg(unix)]
#[test]
fn a_command_whose_answer_was_lost_is_not_resent() {
    let root = tempfile::tempdir().expect("tempdir");
    let (backend, mux) = stalling_backend(root.path());
    let backend = Arc::new(backend);
    let first = backend.connection().expect("connection");
    let in_flight = std::thread::spawn({
        let backend = Arc::clone(&backend);
        move || backend.ctrl_command("list-windows -a")
    });
    await_log(&mux, |log| log.contains("list-windows"));

    first.close();
    let answered = in_flight.join().expect("in-flight caller");

    assert!(answered.is_err(), "{answered:?}");
    let log = std::fs::read_to_string(mux.with_extension("log")).expect("log");
    assert_eq!(log.matches("list-windows").count(), 1, "{log}");
}

/// A quit that lands while a reconnect is replacing the connection must
/// still close the one replaced. It was recorded as retiring only after
/// the swap let go of `control`, so a `shutdown` in that gap took the new
/// connection, found nothing retiring, and the old one's child outlived
/// the quit. With the swap parked at its record step (by holding
/// `retiring`), the new connection must not be visible yet.
#[cfg(unix)]
#[test]
fn a_quit_during_a_reconnect_closes_the_replaced_connection() {
    let root = tempfile::tempdir().expect("tempdir");
    let (backend, _mux) = stalling_backend(root.path());
    let backend = Arc::new(backend);
    let old = backend.connection().expect("connection");
    let fresh = Arc::new(
        ControlMode::start(
            &backend.located_transport(),
            &backend.socket(),
            &backend.session,
            &backend.link.sizer,
            &UnmonitoredMux::control_policy(&backend.transport, &backend.session),
        )
        .expect("second connection"),
    );

    let parked = backend.link.retiring.lock().expect("retiring lock");
    let swap = std::thread::spawn({
        let (backend, fresh) = (Arc::clone(&backend), Arc::clone(&fresh));
        move || backend.swap(&fresh).map(|old| old.is_some())
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
    let mut visible = false;
    while !visible && std::time::Instant::now() < deadline {
        let current = backend.link.control.try_lock().ok().and_then(|c| c.clone());
        visible = current.is_some_and(|c| Arc::ptr_eq(&c, &fresh));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drop(parked);
    let swapped = swap.join().expect("swap");
    backend.shutdown();

    assert!(
        !visible,
        "the swap showed before the old connection was recorded"
    );
    assert!(swapped.is_ok_and(|replaced| replaced), "nothing replaced");
    let answer = old.send_command_within("display-message -p ''", LOOP_COMMAND_BUDGET);
    assert!(
        answer.is_err(),
        "the replaced connection outlived the quit: {answer:?}"
    );
}

/// The whole point of the fix: what tmux is handed must not need tmux's own
/// `PATH` (nor the `PATH` of the shell tmux runs a single-token command
/// with) to be found.
#[test]
#[cfg(unix)]
fn a_local_window_command_is_an_absolute_path() {
    let dir = tempfile::TempDir::new().unwrap();
    let expected = agent_only_thurbox_can_see(dir.path(), "tbx-spawn-probe");

    // Through the shared helper: `PATH` is process state, and the unit
    // tests that set it run concurrently under plain `cargo test`.
    let (local, free, remote) = crate::paths::with_path(dir.path(), || {
        (
            TestBackend::local(crate::session::settings::default_scrollback_lines())
                .program_for_window("tbx-spawn-probe"),
            resolve_local_program("tbx-spawn-probe"),
            // A remote host's PATH is the host's, so its command is the
            // host's to resolve — and it is login-wrapped instead.
            TestBackend::for_host(
                &crate::session::HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                crate::session::settings::default_scrollback_lines(),
            )
            .program_for_window("tbx-spawn-probe"),
        )
    });

    assert_eq!(local, expected.to_string_lossy());
    assert_eq!(free, expected.to_string_lossy());
    assert_eq!(remote, "tbx-spawn-probe");
}

/// Where a server is looked for: the directory a new one would use
/// first, then where the host's CLI or an earlier build put one.
#[test]
fn a_hosts_server_is_looked_for_where_its_own_thurbox_said_first() {
    let host_data = SocketDir::HostData {
        app: crate::paths::app_dir_name().into(),
    };
    let backend = |name: &str| {
        let host = crate::session::HostDef {
            name: name.into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        let mut backend =
            TestBackend::for_host(&host, crate::session::settings::default_scrollback_lines());
        backend.transport = backend.transport.clone().with_socket_dir(host_data.clone());
        (host, backend)
    };
    let (_, unasked) = backend("socket-dir-unasked");
    assert_eq!(
        unasked.socket_dir_candidates(),
        [host_data.clone(), SocketDir::Inherited]
    );
    let (older, older_backend) = backend("socket-dir-older-cli");
    crate::backend::instance::learn_host_socket_dir(&older, None);
    assert_eq!(
        older_backend.socket_dir_candidates(),
        [SocketDir::Inherited, host_data.clone()],
        "a CLI that reported no directory spawns into tmux's default"
    );
    let (newer, newer_backend) = backend("socket-dir-newer-cli");
    crate::backend::instance::learn_host_socket_dir(&newer, Some("/srv/me/thurbox/tmux"));
    assert_eq!(
        newer_backend.socket_dir_candidates(),
        [
            SocketDir::Dir("/srv/me/thurbox/tmux".into()),
            host_data,
            SocketDir::Inherited
        ]
    );
}

#[test]
fn build_shell_command_simple() {
    let cmd = TestBackend::build_shell_command("claude", &[]);
    assert_eq!(cmd, "claude");
}

#[test]
fn build_shell_command_with_args() {
    let args = vec![
        "--resume".to_string(),
        "abc-123".to_string(),
        "--permission-mode".to_string(),
        "default".to_string(),
    ];
    let cmd = TestBackend::build_shell_command("claude", &args);
    assert_eq!(cmd, "claude --resume abc-123 --permission-mode default");
}

#[test]
fn build_shell_command_with_spaces_in_args() {
    let args = vec![
        "--allowed-tools".to_string(),
        "Read Bash(git:*)".to_string(),
    ];
    let cmd = TestBackend::build_shell_command("claude", &args);
    assert_eq!(cmd, "claude --allowed-tools 'Read Bash(git:*)'");
}

#[test]
fn build_shell_command_escapes_command_path() {
    // The command token is interpreted by the server's shell, so a path
    // with a space (or any metacharacter) must be quoted, not left bare —
    // otherwise the shell would split it and the launch would break.
    let cmd = TestBackend::build_shell_command("/opt/My Agents/codex", &["--foo".to_string()]);
    assert_eq!(cmd, "'/opt/My Agents/codex' --foo");
}

#[test]
fn backend_default_has_no_control_mode() {
    let backend = TestBackend::local(crate::session::settings::default_scrollback_lines());
    let guard = backend.link.control.lock().unwrap();
    assert!(guard.is_none());
}

#[test]
fn for_host_builds_named_ssh_backend() {
    let host = crate::session::HostDef {
        name: "devbox".into(),
        destination: "me@devbox".into(),
        ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
        ..Default::default()
    };
    let backend =
        TestBackend::for_host(&host, crate::session::settings::default_scrollback_lines());
    assert_eq!(backend.name(), "ssh:devbox:tmux");
    assert!(backend.transport.is_remote());
    // Falls back to the default socket/session when the host omits them.
    assert_eq!(backend.socket, TMUX_SOCKET);
    assert_eq!(backend.session, TMUX_SESSION);
}

/// A socket a host's thurbox reported is that instance's address, so every
/// multiplexer on the host reaches it — one learned while driving tmux is
/// the one its psmux backend uses too.
#[test]
fn a_learned_socket_is_the_hosts_whichever_multiplexer_learned_it() {
    let host = crate::session::HostDef {
        name: "learned-socket-host".into(),
        destination: "me@learned".into(),
        ..Default::default()
    };
    learn_host_socket(&host, "thurbox-elsewhere");
    assert_eq!(
        TestBackend::for_host(&host, crate::session::settings::default_scrollback_lines()).socket(),
        "thurbox-elsewhere"
    );
    let mut served = host.clone();
    served.multiplexer = Some("psmux".into());
    assert_eq!(host_socket(&served), "thurbox-elsewhere");
    assert_eq!(known_host_socket(&served).unwrap(), "thurbox-elsewhere");
}

#[test]
fn for_host_builds_named_wsl_backend() {
    let host = crate::session::HostDef::wsl("Ubuntu");
    let backend =
        TestBackend::for_host(&host, crate::session::settings::default_scrollback_lines());
    assert_eq!(backend.name(), "wsl:Ubuntu:tmux");
    assert!(backend.transport.is_remote());
    assert_eq!(backend.transport.launcher(), "wsl.exe");
    let argv: Vec<String> = backend
        .transport
        .tmux_command("s", &[])
        .expect("command")
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(argv[..2], ["-d", "Ubuntu"]);
    assert_eq!(backend.socket, TMUX_SOCKET);
    assert_eq!(backend.session, TMUX_SESSION);
}

#[test]
fn default_shell_matches_host_os_not_local() {
    // The local $SHELL (e.g. /bin/zsh) may not exist on the host: a remote
    // Windows pane got "CommandNotFoundException", a zsh-less Linux host a
    // dead pane. Remote backends pick by transport.
    let winbox = TestBackend::for_host(
        &crate::session::HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..Default::default()
        },
        crate::session::settings::default_scrollback_lines(),
    );
    assert_eq!(winbox.default_shell(), "powershell");

    let devbox = TestBackend::for_host(
        &crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        },
        crate::session::settings::default_scrollback_lines(),
    );
    assert_eq!(devbox.default_shell(), "/bin/sh");

    let wsl = TestBackend::for_host(
        &crate::session::HostDef::wsl("Ubuntu"),
        crate::session::settings::default_scrollback_lines(),
    );
    assert_eq!(wsl.default_shell(), "/bin/sh");

    // Local keeps the platform default ($SHELL / %COMSPEC%).
    let local = TestBackend::local(crate::session::settings::default_scrollback_lines());
    #[cfg(not(windows))]
    assert_eq!(
        local.default_shell(),
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    );
    #[cfg(windows)]
    assert_eq!(
        local.default_shell(),
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    );
}

/// The `default-command` a backend pins on its server, if any.
fn pinned_default_command(backend: &TestBackend) -> Option<String> {
    backend.session_config().into_iter().find_map(|option| {
        let at = option.args.iter().position(|a| a == "default-command")?;
        option.args.get(at + 1).cloned()
    })
}

fn windows_host(mux: &str) -> crate::session::HostDef {
    crate::session::HostDef {
        name: "winbox".into(),
        destination: "me@winbox".into(),
        multiplexer: Some(mux.into()),
        platform: Some(crate::session::Platform::Windows),
        ..Default::default()
    }
}

/// A WSL distro is Linux whatever machine drives it, so its server gets
/// `/bin/sh` as `default-command` from a Windows thurbox as from a Linux
/// one. Simulated: the build OS is replaced by `shell::local_platform`'s test
/// override, which is the only way a Linux run reaches the Windows branch.
#[test]
fn a_windows_thurbox_pins_posix_default_command_on_a_wsl_host() {
    use crate::session::{HostDef, Platform};
    use crate::shell::simulate_local;
    for local in Platform::ALL {
        let pinned = simulate_local(local, || {
            pinned_default_command(&TestBackend::for_host(
                &HostDef::wsl("Ubuntu"),
                crate::session::settings::default_scrollback_lines(),
            ))
        });
        assert_eq!(pinned.as_deref(), Some("/bin/sh"), "thurbox on {local:?}");
    }
}

/// A Windows host is Windows whichever multiplexer serves it: PowerShell
/// for its shell panes, its own native `default-command`, no `/bin/sh -lc`
/// wrap — the platform is the host's, never inferred from `psmux`.
#[test]
fn a_windows_host_on_a_non_psmux_multiplexer_keeps_native_shell_semantics() {
    use crate::session::Platform;
    use crate::shell::simulate_local;
    for local in Platform::ALL {
        simulate_local(local, || {
            let backend = TestBackend::for_host(
                &windows_host("tmux"),
                crate::session::settings::default_scrollback_lines(),
            );
            assert_eq!(backend.transport.mux(), "tmux");
            assert_eq!(backend.default_shell(), "powershell", "from {local:?}");
            assert_eq!(pinned_default_command(&backend), None, "from {local:?}");
            assert_eq!(backend.login_wrap_for_remote("agent"), "agent");
        });
    }
}

/// A companion shell pane on a Windows host opens PowerShell whichever
/// multiplexer serves it: the POSIX login-shell bootstrap is `/bin/sh`,
/// which such a host does not have. A POSIX host keeps the bootstrap.
#[test]
fn a_windows_hosts_shell_pane_is_not_bootstrapped_through_sh() {
    use crate::session::HostDef;
    let window = format!("{SHELL_WINDOW_PREFIX}work");
    let shell_pane = |backend: &TestBackend| {
        TestMux::window_command(
            backend,
            &window,
            &backend.default_shell(),
            &[],
            &HashMap::new(),
        )
        .expect("a shell pane's command")
    };
    let windows = TestBackend::for_host(
        &windows_host("tmux"),
        crate::session::settings::default_scrollback_lines(),
    );
    assert_eq!(shell_pane(&windows), "powershell");
    let posix = TestBackend::for_host(
        &HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        },
        crate::session::settings::default_scrollback_lines(),
    );
    assert_eq!(shell_pane(&posix), posix.remote_shell_pane_command());
}

/// The backend a route builds keeps the host's platform, the host's
/// launcher and the route's multiplexer, each unaffected by the others and
/// by the OS thurbox is built for.
#[test]
fn a_backend_keeps_platform_launcher_and_multiplexer_apart() {
    use crate::session::{HostDef, HostKind, Multiplexer, Platform};
    use crate::shell::simulate_local;
    for local in Platform::ALL {
        for kind in [HostKind::Ssh, HostKind::Wsl] {
            for platform in [Platform::Posix, Platform::Windows] {
                for preferred in [None, Some("psmux"), Some("rmux")] {
                    let host = HostDef {
                        name: "box".into(),
                        kind,
                        destination: "me@box".into(),
                        platform: Some(platform),
                        multiplexer: preferred.map(str::to_string),
                        ..Default::default()
                    };
                    let backend = simulate_local(local, || {
                        TestBackend::for_host(
                            &host,
                            crate::session::settings::default_scrollback_lines(),
                        )
                    });
                    let case = format!("{kind:?}/{platform:?}/{preferred:?} from {local:?}");
                    assert_eq!(backend.platform, host.platform(), "{case}");
                    assert_eq!(backend.transport.mux(), "tmux", "{case}");
                    let launcher = match kind {
                        HostKind::Ssh => "ssh",
                        HostKind::Wsl => "wsl.exe",
                    };
                    assert_eq!(backend.transport.launcher(), launcher, "{case}");
                    assert_eq!(
                        backend.name(),
                        host.route(Some(Multiplexer::Tmux)).format(),
                        "{case}"
                    );
                    assert!(!backend.capabilities().needs_liveness_poll, "{case}");
                }
            }
        }
    }
}

/// A legacy entry is Windows because it names psmux; a backend built to
/// serve one of its rows on another multiplexer is still on Windows.
#[test]
fn a_route_on_another_multiplexer_keeps_the_hosts_platform() {
    let legacy = crate::session::HostDef {
        name: "win".into(),
        destination: "me@win".into(),
        multiplexer: Some("psmux".into()),
        ..Default::default()
    };
    let backend = TestBackend::for_host(
        &legacy,
        crate::session::settings::default_scrollback_lines(),
    );
    assert_eq!(backend.transport.mux(), "tmux");
    assert_eq!(backend.platform, crate::session::Platform::Windows);
    assert_eq!(backend.default_shell(), "powershell");
}

#[test]
fn remote_shell_pane_opens_users_login_shell() {
    // The companion shell pane on a remote/WSL host should give the user
    // their own interactive login shell (the SSH-login environment: rc
    // files, prompt, aliases, PATH) — not the bare `/bin/sh` the generic
    // login-wrap would produce. Bootstrap through the always-present
    // `/bin/sh -l` (exports `$SHELL`), then `exec "$SHELL" -l`.
    //
    // Crucially the `$SHELL` probe is a `command -v` guard, NOT
    // `exec "$SHELL" -l 2>/dev/null`: an `exec … 2>/dev/null` redirection
    // persists into the exec'd shell, drops stderr off the TTY, and bash/zsh
    // then start non-interactive (no prompt) — a blank pane.
    const EXPECT: &str =
            "/bin/sh -lc 'command -v \"$SHELL\" >/dev/null 2>&1 && exec \"$SHELL\" -l; exec /bin/sh -l'";
    let ssh = TestBackend::for_host(
        &crate::session::HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        },
        crate::session::settings::default_scrollback_lines(),
    );
    assert_eq!(ssh.remote_shell_pane_command(), EXPECT);

    let wsl = TestBackend::for_host(
        &crate::session::HostDef::wsl("Ubuntu"),
        crate::session::settings::default_scrollback_lines(),
    );
    assert_eq!(wsl.remote_shell_pane_command(), EXPECT);

    // The interactive shell must keep stderr on the PTY — a stray
    // `exec … 2>` would make it non-interactive.
    assert!(!EXPECT.contains("-l 2>"));
}

#[test]
fn login_wrap_wraps_remote_command_in_login_shell() {
    // Remote/WSL: the window command runs under a login shell so the user's
    // profile PATH (e.g. `~/.local/bin/claude`) is present, or the agent
    // binary isn't found and the pane dies instantly.
    let backend = TestBackend::for_host(
        &crate::session::HostDef::wsl("Ubuntu"),
        crate::session::settings::default_scrollback_lines(),
    );
    let wrapped = backend.login_wrap_for_remote("claude --resume x");
    assert_eq!(wrapped, "/bin/sh -lc 'exec claude --resume x'");
}

#[test]
fn login_wrap_is_noop_for_local() {
    // Local backends inherit the user's interactive PATH — no wrap needed.
    let backend = TestBackend::local(crate::session::settings::default_scrollback_lines());
    assert_eq!(backend.login_wrap_for_remote("claude"), "claude");
}

#[test]
fn for_host_honors_socket_and_session_overrides() {
    let host = crate::session::HostDef {
        name: "vm".into(),
        destination: "vm".into(),
        socket: Some("tb-vm".into()),
        session: Some("sess-vm".into()),
        ..Default::default()
    };
    let backend =
        TestBackend::for_host(&host, crate::session::settings::default_scrollback_lines());
    assert_eq!(backend.socket, "tb-vm");
    assert_eq!(backend.session, "sess-vm");
}

/// The same refusal through the contract: a teardown, a restart's listing
/// and a rename on such a host all stop before a command reaches it —
/// nothing is killed on a server of this build's guessing, and none of
/// them opens control mode, which would create one.
#[test]
fn nothing_is_done_on_a_host_whose_socket_is_a_guess() {
    let host = crate::session::HostDef {
        name: "devbox".into(),
        destination: "me@devbox".into(),
        ..Default::default()
    };
    let backend =
        TestBackend::for_host(&host, crate::session::settings::default_scrollback_lines());
    let owner = Owner::new("00000000-0000-4000-8000-000000000001", "remote").remembering("%3", "");
    let refusals = [
        backend.discover().map(drop),
        backend.locate(owner).map(drop),
        backend.kill("%3"),
        backend.rename_windows(owner, "moved"),
    ];
    for refusal in refusals {
        let refusal = format!("{:#}", refusal.unwrap_err());
        assert!(
            refusal.contains("socket unknown for host 'devbox'"),
            "{refusal}"
        );
    }
    assert!(!backend.attached(), "a refusal opened a connection");
}

use crate::backend::identity::tests::listed;
use crate::backend::identity::{program_window_name, shell_window_name};
use crate::backend::tmux_compat::control_mode::{
    decode_octal, format_send_keys, parse_notification, shell_escape, Notification,
};

/// The one distinction the remote teardown rests on. Each answer below is
/// what tmux 3.5/3.7 actually printed when asked for a listing it could not
/// give (captured against the linux-container e2e host); each failure below
/// is a question that was never answered. Reading the second group as the
/// first is what let a force delete against a host that was down for a
/// minute report nothing to kill and leave the agent running there.
#[test]
fn only_the_multiplexers_own_refusal_counts_as_an_empty_answer() {
    let answers = [
        "error connecting to /tmp/tmux-0/thurbox (No such file or directory)",
        "no server running on /tmp/tmux-0/thurbox",
        "can't find session: thurbox",
    ];
    for answer in answers {
        assert!(
            TestMux::answered_absent(answer) == Some(Missing::Server),
            "the multiplexer answered: {answer}"
        );
    }
    let unanswered = [
        "ssh: connect to host devbox port 22: Connection refused",
        "ssh: connect to host devbox port 22: Operation timed out",
        "Permission denied (publickey).",
        "bash: line 1: tmux: command not found",
        "Failed to run tmux command",
    ];
    for failure in unanswered {
        assert!(
            TestMux::answered_absent(failure).is_none(),
            "nothing answered: {failure}"
        );
    }
}

/// The trap `error connecting to` sets, and the reason it is not a prefix
/// match. tmux prints it both for a socket that is not there and for one it
/// cannot open **while a server is alive behind it** — the `(Permission
/// denied)` line below is what a live server on another user's socket
/// actually prints (reproduced by chmod-ing a running server's socket dir).
/// Reading that as absence is a reachable failure mistaken for "nothing to
/// kill", which is the orphan this whole path exists to prevent.
#[test]
fn a_socket_that_cannot_be_opened_is_not_a_server_that_is_not_there() {
    assert!(
        TestMux::answered_absent(
            "error connecting to /tmp/tmux-0/thurbox (No such file or directory)"
        ) == Some(Missing::Server),
        "no socket at all is the one reason that means absence"
    );
    for live in [
        "error connecting to /tmp/tmux-1000/thurbox (Permission denied)",
        "error connecting to /tmp/tmux-1000/thurbox (Connection refused)",
        "error connecting to /tmp/tmux-1000/thurbox (Connection reset by peer)",
    ] {
        assert!(
            TestMux::answered_absent(live).is_none(),
            "a server may be alive behind this socket: {live}"
        );
    }
}

/// Layer before text. `ssh` exits 255 for its own failures and passes a
/// remote command's status through untouched, so 255 means the question
/// never arrived — even when the bytes on stderr happen to read exactly
/// like tmux answering, which is the case no amount of message-matching
/// can get right on its own.
#[test]
fn ssh_failing_on_its_own_account_is_never_absence() {
    let tmux_said_absent = "no server running on /tmp/tmux-0/thurbox";

    assert!(
        mux_answered::<TestMux>(true, Some(1), tmux_said_absent).is_some(),
        "ssh passed through tmux's own answer"
    );
    assert!(
        mux_answered::<TestMux>(true, Some(255), tmux_said_absent).is_none(),
        "255 is ssh's own failure; nothing on the host answered"
    );
    assert!(
        mux_answered::<TestMux>(false, Some(255), tmux_said_absent).is_some(),
        "without ssh in the path, 255 carries none of that meaning"
    );
    assert!(
        mux_answered::<TestMux>(true, Some(127), "bash: tmux: command not found").is_none(),
        "reached the host, but nothing there answered the question"
    );
    assert!(
        mux_answered::<TestMux>(true, None, tmux_said_absent).is_none(),
        "killed by a signal: no status to reason from"
    );
}

// The control-mode primitives are re-exported through this module. Their
// behavior is covered exhaustively in `control_mode`'s own test module;
// this single smoke check just asserts the re-export path still resolves
// (the per-case bodies that used to be duplicated here added no coverage).
#[test]
fn control_mode_reexports_resolve() {
    assert_eq!(shell_escape("hello world"), "'hello world'");
    assert_eq!(decode_octal(b"\\033"), vec![27]);
    assert_eq!(format_send_keys("%1", b"A"), "send-keys -t %1 -H 41\n");
    assert_eq!(
        parse_notification("%pause %1"),
        Notification::Pause {
            pane_id: "%1".to_string()
        }
    );
}

// --- path_led_by (the PATH a pane is handed) ---

#[cfg(not(windows))]
#[test]
fn the_cli_directory_leads_the_path_it_was_missing_from() {
    let inherited = std::ffi::OsString::from("/usr/bin:/bin");
    let led = path_led_by(Path::new("/opt/thurbox/bin"), &inherited).expect("joinable");
    assert_eq!(
        led,
        std::ffi::OsString::from("/opt/thurbox/bin:/usr/bin:/bin")
    );
}

/// Promoted rather than prepended again: the pane must not be handed a
/// `PATH` naming one directory twice.
#[cfg(not(windows))]
#[test]
fn a_directory_already_on_the_path_is_moved_not_duplicated() {
    let inherited = std::ffi::OsString::from("/usr/bin:/opt/thurbox/bin:/bin");
    let led = path_led_by(Path::new("/opt/thurbox/bin"), &inherited).expect("joinable");
    assert_eq!(
        led,
        std::ffi::OsString::from("/opt/thurbox/bin:/usr/bin:/bin")
    );
}

/// An unset `PATH` is not an error: the directory alone is a better answer
/// than declining, and is exactly what the pane needs.
#[cfg(not(windows))]
#[test]
fn an_empty_path_becomes_the_cli_directory_alone() {
    let led =
        path_led_by(Path::new("/opt/thurbox/bin"), std::ffi::OsStr::new("")).expect("joinable");
    assert_eq!(led, std::ffi::OsString::from("/opt/thurbox/bin"));
}

// --- path_from_prefix (reading a pane's PATH back) ---

#[test]
fn a_window_command_opening_with_env_yields_its_path() {
    assert_eq!(
        path_from_prefix("/usr/bin/env PATH=/opt/tbx:/usr/bin /usr/bin/sh -c \"sleep 30\""),
        Some("/opt/tbx:/usr/bin".to_string())
    );
}

/// Anchored at the second token: a `PATH=` further along is an argument of
/// the agent's own, and reading it as the pane's environment would be a
/// confident wrong answer.
#[test]
fn a_path_assignment_elsewhere_in_the_command_is_not_the_panes() {
    assert_eq!(path_from_prefix("/usr/bin/claude --env PATH=/nope"), None);
    assert_eq!(path_from_prefix("/usr/bin/claude"), None);
    assert_eq!(path_from_prefix(""), None);
}

/// The shape a **command session** produces, copied from a real
/// `#{pane_start_command}`: one token for the whole command, quoted by
/// tmux. The opening quote rides on the program, which this discards.
#[test]
fn a_quoted_single_token_command_still_yields_its_path() {
    assert_eq!(
        path_from_prefix("\"/usr/bin/env PATH=/opt/tbx:/usr/bin sh -c 'sleep 300'\""),
        Some("/opt/tbx:/usr/bin".to_string())
    );
}

/// tmux quotes a token holding whitespace, so a `PATH` with a space in a
/// component reads as unknown rather than as a truncated answer.
#[test]
fn a_quoted_prefix_reads_as_unknown() {
    assert_eq!(
        path_from_prefix("/usr/bin/env \"PATH=/opt/my tools:/usr/bin\" /usr/bin/sh"),
        None
    );
}

// --- local command resolution ---

/// An executable on a directory only *this process* has on `PATH` — the
/// shape an agent installed by `fish_add_path` is in.
#[cfg(unix)]
fn agent_only_thurbox_can_see(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
    p
}

/// Best-effort: a name nothing on `PATH` matches is passed through, so a
/// shell function, an alias, or a binary installed after this ran keeps
/// working exactly as it did before.
#[test]
fn an_unresolvable_local_command_is_passed_through() {
    assert_eq!(
        resolve_local_program("tbx-agent-that-is-not-installed"),
        "tbx-agent-that-is-not-installed"
    );
    assert_eq!(
        resolve_local_program("/opt/My Agents/codex"),
        "/opt/My Agents/codex"
    );
}

// --- named keys ---

#[test]
fn tmux_names_every_key() {
    for name in Key::NAMED {
        let key = Key::parse(name).expect("a listed key");
        assert!(!tmux_key_name(&key).is_empty(), "{name}");
    }
    assert_eq!(tmux_key_name(&Key::parse("pgup").unwrap()), "PageUp");
    assert_eq!(tmux_key_name(&Key::parse("delete").unwrap()), "DC");
    for letter in 'a'..='z' {
        let key = Key::parse(&format!("ctrl-{letter}")).unwrap();
        assert_eq!(tmux_key_name(&key), format!("C-{letter}"));
    }
}

#[test]
fn an_answer_counts_only_from_the_pane_asked_about() {
    assert!(answered_for("%3", Some("tb-x"), Some("%3")));
    // A gone pane id answers nothing; an unresolved target answers for
    // the client's current pane.
    assert!(!answered_for("%3", None, None));
    assert!(!answered_for("%3", Some("tb-y"), Some("%0")));
    assert!(answered_for("thurbox:=tb-x", Some("tb-x"), Some("%9")));
    assert!(!answered_for("thurbox:=tb-x", Some("tb-y"), Some("%9")));
}

/// Where ids are per session, a bare `%2` reached the `__warm__` session's
/// `%2` (measured on psmux 3.3.7), so no id leaves without its session.
/// Where they are server-wide (tmux), nothing changes.
#[test]
fn an_id_target_is_qualified_only_where_ids_are_per_session() {
    assert_eq!(qualify_target("thurbox", "%2", true), "thurbox:.%2");
    assert_eq!(qualify_target("thurbox", "@7", true), "thurbox:@7");
    assert_eq!(
        qualify_target("thurbox", "thurbox:=tb-x", true),
        "thurbox:=tb-x"
    );
    assert_eq!(qualify_target("thurbox", "thurbox", true), "thurbox");
    for id in ["%2", "@7", "thurbox:=tb-x"] {
        assert_eq!(qualify_target("thurbox", id, false), id);
    }
}

/// Where ids are server-wide and windows are killed as windows (tmux), the
/// kill is what it always was. psmux's is pinned beside its adapter.
#[test]
fn a_kill_of_a_server_wide_id_is_unchanged() {
    assert_eq!(
        TestBackend::local(crate::session::settings::default_scrollback_lines())
            .kill_spelling("%8"),
        ("kill-window", Cow::Borrowed("%8"))
    );
}

/// A `display-message` answered for another pane — psmux's answer for a
/// pane it cannot find — records no window, so no close of the active
/// window can end this pane's reader.
#[test]
fn a_pane_location_is_only_taken_from_the_panes_own_answer() {
    assert_eq!(
        parse_pane_location("%7", "%7 @3 24 80 me\n"),
        Some(PaneLocation {
            window: "@3".into(),
            size: Some((24, 80)),
            sized_by: Some("me".into()),
        })
    );
    assert_eq!(
        parse_pane_location("%7", "%7 @3"),
        Some(PaneLocation {
            window: "@3".into(),
            size: None,
            sized_by: None,
        })
    );
    assert_eq!(parse_pane_location("%9", "%1 @1\n"), None);
    assert_eq!(parse_pane_location("%7", "%7 nonsense"), None);
    assert_eq!(parse_pane_location("%7", ""), None);
}

#[test]
fn a_shareable_host_that_has_not_said_which_socket_it_uses_is_refused() {
    // A remote teardown used to fall back on this build's own socket name.
    // On a host running its own thurbox that is a guess about somebody
    // else's machine — a dev build aims at `thurbox-dev` while the host's
    // release binary runs `thurbox`, and a relocated data dir derives a
    // name of its own — so the teardown acted on an empty server, and
    // `ensure_ready` created one there while it was at it.
    let host = crate::session::HostDef {
        name: "devbox".into(),
        destination: "me@devbox".into(),
        ..Default::default()
    };
    let refusal = format!("{:#}", known_host_socket(&host).unwrap_err());
    assert!(
        refusal.contains("socket unknown for host 'devbox'"),
        "{refusal}"
    );

    // Sharing off: nothing but this thurbox writes there, so its own socket
    // is the host's by construction.
    let solo = crate::session::HostDef {
        share_sessions: false,
        ..host.clone()
    };
    assert_eq!(known_host_socket(&solo).unwrap(), TMUX_SOCKET);

    // And a pinned socket answers without asking anyone.
    let pinned = crate::session::HostDef {
        socket: Some("thurbox".into()),
        ..host
    };
    assert_eq!(known_host_socket(&pinned).unwrap(), "thurbox");
}

// Compile-time check: channel capacity must be large enough to buffer heavy output.
const _: () = assert!(PANE_CHANNEL_CAPACITY >= 1024);

#[test]
fn env_flag_simple_value() {
    // Simple key=value should not be quoted.
    let env_part: String = [("RUST_LOG".to_string(), "debug".to_string())]
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>()
        .iter()
        .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
        .collect();
    assert_eq!(env_part, " -e RUST_LOG=debug");
}

#[test]
fn env_flag_value_with_spaces() {
    // Values with spaces must be quoted as a single KEY=VALUE unit.
    let env_part: String = [("MSG".to_string(), "hello world".to_string())]
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>()
        .iter()
        .map(|(k, v)| format!(" -e {}", shell_escape(&format!("{k}={v}"))))
        .collect();
    assert_eq!(env_part, " -e 'MSG=hello world'");
}

/// Only an agent's window keeps its corpse — and the answer is read off the
/// *name*, so it is pinned against the three name builders rather than
/// against hand-written prefixes that could drift from them.
#[test]
fn an_agent_window_keeps_its_corpse_and_the_other_two_do_not() {
    assert!(keeps_dead_pane(&agent_window_name("Foo Bar")));
    assert!(!keeps_dead_pane(&shell_window_name("Foo Bar")));
    assert!(!keeps_dead_pane(&program_window_name("abcd1234", "watch")));
    // A window thurbox did not create is not thurbox's to keep open either.
    assert!(!keeps_dead_pane("zsh"));
}

const ONE: &str = "11111111-1111-4111-8111-111111111111";
const TWO: &str = "22222222-2222-4222-8222-222222222222";

/// A status question that never reached a server is no answer: the poll
/// must keep the held states rather than be told there are none.
#[test]
fn an_unanswered_status_listing_is_an_error() {
    let backend = TestBackend::with_transport(
        TmuxTransport::local("thurbox-test-no-such-multiplexer"),
        "thurbox-test",
        "thurbox-test",
        "local:tmux",
        crate::session::settings::default_scrollback_lines(),
    );
    assert!(backend.hook_states().is_err());
}

/// The teardown's kill takes one path on this machine and on a host: a
/// multiplexer that cannot be launched is the missing dependency it is,
/// named as such, wherever the server runs.
#[test]
fn a_teardown_kill_names_a_missing_multiplexer() {
    let backend = TestBackend::with_transport(
        TmuxTransport::local("thurbox-test-no-such-multiplexer"),
        "thurbox-test",
        "thurbox-test",
        "local:tmux",
        crate::session::settings::default_scrollback_lines(),
    );
    let error = backend
        .kill("%1")
        .expect_err("nothing to launch")
        .to_string();
    assert!(
        error.contains("thurbox-test-no-such-multiplexer (thurbox's multiplexer) is not installed"),
        "{error}"
    );
}

/// A multiplexer binary that answers every command it is not told about
/// with `stderr` and exit 1, and `list-windows` with `windows`.
#[cfg(unix)]
pub(super) fn answering_mux(dir: &std::path::Path, windows: &str, stderr: &str) -> TestBackend {
    use std::os::unix::fs::PermissionsExt;
    let mux = dir.join("mux");
    let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  *list-windows*) [ -n '{windows}' ] && {{ echo '{windows}'; exit 0; }} ;;\nesac\ncat >&2 <<'EOF'\n{stderr}\nEOF\nexit 1\n"
        );
    std::fs::write(&mux, script).unwrap();
    std::fs::set_permissions(&mux, std::fs::Permissions::from_mode(0o700)).unwrap();
    TestBackend::with_transport(
        TmuxTransport::local(mux.to_string_lossy().into_owned()),
        "thurbox-test",
        "thurbox-test",
        "local:tmux",
        crate::session::settings::default_scrollback_lines(),
    )
}

/// tmux prints `error connecting to` for a socket it cannot open while a
/// server is alive behind it — another user's, or a stale one. That is a
/// question nobody answered, so status and the heartbeat must say so
/// rather than report no states and no heartbeat.
#[cfg(unix)]
#[test]
fn a_socket_that_cannot_be_opened_is_no_answer() {
    let dir = tempfile::tempdir().unwrap();
    for refused in [
        "error connecting to /tmp/tmux-1/thurbox-test (Permission denied)",
        "error connecting to /tmp/tmux-1/thurbox-test (Connection refused)",
    ] {
        let backend = answering_mux(dir.path(), "", refused);
        assert!(backend.hook_states().is_err(), "hook_states: {refused}");
        assert!(
            backend.heartbeat_running().is_err(),
            "heartbeat_running: {refused}"
        );
    }
    for absent in [
        "error connecting to /tmp/tmux-1/thurbox-test (No such file or directory)",
        "can't find session: thurbox-test",
    ] {
        let backend = answering_mux(dir.path(), "", absent);
        assert_eq!(backend.hook_states().unwrap(), Vec::new(), "{absent}");
        assert!(!backend.heartbeat_running().unwrap(), "{absent}");
    }
}

/// The headless status poll reads an empty listing as "every pane quiet",
/// so only the server's own "nothing here" may become one.
#[cfg(unix)]
#[test]
fn only_the_server_saying_no_session_is_an_empty_status_answer() {
    use std::os::unix::process::ExitStatusExt;
    let backend = TestBackend::local(crate::session::settings::default_scrollback_lines());
    let has_session = |stderr: &str| {
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        matches!(
            backend.command_failure(&["has-session", "-t", "thurbox"], &output),
            crate::backend::BackendError::Absent(_)
        )
    };
    for absent in [
        "can't find session: thurbox",
        "no server running on /tmp/tmux-1/thurbox",
        "error connecting to /tmp/tmux-1/thurbox (No such file or directory)",
    ] {
        assert!(has_session(absent), "{absent}");
    }
    for unanswered in [
        "bash: line 1: tmux: command not found",
        "error connecting to /tmp/tmux-1/thurbox (Permission denied)",
    ] {
        assert!(!has_session(unanswered), "{unanswered}");
    }
}

/// The format is a literal because a `const` cannot interpolate another;
/// this is what keeps it honest.
#[test]
fn the_discover_format_reads_both_stamps() {
    assert!(DISCOVER_FORMAT.contains(&format!("#{{{WINDOW_SESSION_OPTION}}}")));
    assert!(DISCOVER_FORMAT.contains(&format!("#{{{WINDOW_ROLE_OPTION}}}")));
}

/// The retirement reads its own listing, so its format is pinned the same
/// way — and in the order `stamped_windows_in` splits on.
#[test]
fn the_retire_format_reads_the_window_and_both_stamps() {
    assert_eq!(
        RETIRE_FORMAT,
        format!("#{{window_id}}|#{{{WINDOW_SESSION_OPTION}}}|#{{{WINDOW_ROLE_OPTION}}}")
    );
}

/// The whole rule, on the listing the sweep actually parses. Oldest first,
/// so the keeper is the last entry — and only this session's `role` windows
/// are in it at all.
#[test]
fn a_listing_orders_one_sessions_windows_by_the_id_tmux_issued() {
    let listing =
        format!("@10|{ONE}|agent\n@2|{ONE}|agent\n@3|{TWO}|agent\n@4|{ONE}|shell\n@5||\n");
    assert_eq!(
        stamped_windows_in(&listing, ONE, WindowRole::Agent),
        vec!["@2".to_string(), "@10".to_string()],
        "ordered by the number, not by the text, and nobody else's window is in it"
    );
    assert_eq!(
        stamped_windows_in(&listing, ONE, WindowRole::Shell),
        vec!["@4".to_string()],
        "a session owns one window per role, so the roles are counted apart"
    );
}

/// A window nothing can place in the order is not one to decide a kill
/// about — which is also what keeps a multiplexer that hands the format
/// string back rather than expanding it from being read as a listing.
#[test]
fn a_window_id_that_does_not_parse_is_left_out_of_the_order() {
    let listing = format!("#{{window_id}}|{ONE}|agent\n@7|{ONE}|agent\n");
    assert_eq!(
        stamped_windows_in(&listing, ONE, WindowRole::Agent),
        vec!["@7".to_string()]
    );
}

/// A pane learns of its own ending only through the window it is in, and
/// the only free moment to learn which window that is, is the answer to the
/// command that made it. Dropping `#{window_id}` from here would cost
/// nothing visible — spawning still works, and the exit simply stops being
/// announced under load — so it is pinned.
#[test]
fn the_spawn_format_asks_for_the_window_too() {
    assert!(SPAWN_FORMAT.contains("#{pane_id}"));
    assert!(SPAWN_FORMAT.contains("#{window_id}"));
    // Parsed by splitting on whitespace, so the two must be separable.
    assert!(SPAWN_FORMAT.split_whitespace().count() == 2);
}

/// Both ids are read off the wire, so both are checked the same way.
#[test]
fn a_window_id_is_an_at_sign_and_digits() {
    assert!(control_mode::is_valid_window_id("@0"));
    assert!(control_mode::is_valid_window_id("@42"));
    assert!(!control_mode::is_valid_window_id("@"));
    assert!(!control_mode::is_valid_window_id("%3"));
    assert!(!control_mode::is_valid_window_id("@3x"));
    assert!(!control_mode::is_valid_window_id(""));
}

/// The rule a teardown reads the companion shell by. `shell_backend_id` is
/// written only once the interface has opened one, so most rows carry no id
/// for their shell at all and the stamp is the whole answer — including the
/// answer that a namesake's shell is *not* this session's to kill.
#[test]
fn a_shell_window_is_owned_by_its_stamp_the_way_an_agent_window_is() {
    let theirs = WindowIndex::from_listing([listed("%5", "tbs-fleet", TWO, WindowRole::Shell)]);
    assert_eq!(theirs.shell_window(ONE, "fleet"), Located::Absent);

    // Unstamped and alone: the pre-ADR-25 shape, and psmux's permanent one.
    let legacy = WindowIndex::from_listing([listed("%5", "tbs-fleet", "", WindowRole::Shell)]);
    assert_eq!(legacy.shell_window(ONE, "fleet"), Located::At("%5".into()));

    // Two of them, neither stamped: nobody can say, and a teardown that
    // guessed would take a live session's shell down.
    let ambiguous = WindowIndex::from_listing([
        listed("%5", "tbs-fleet", "", WindowRole::Shell),
        listed("%6", "tbs-fleet", "", WindowRole::Shell),
    ]);
    assert_eq!(ambiguous.shell_window(ONE, "fleet"), Located::Unknown);
}

/// Anyone can set a window option, and a multiplexer that does not expand
/// `#{@...}` hands the format string straight back — so only a stamp that
/// is a session id is believed.
#[test]
fn only_a_session_id_counts_as_a_stamp() {
    let parsed = parse_discovered("%1|tb-fleet|0|#{@thurbox_session}|agent", true).expect("parsed");
    assert_eq!(parsed.session, "");
    assert_eq!(parsed.role, WindowRole::Agent);
    assert_eq!(
        parse_discovered(&format!("%1|tb-fleet|0|{ONE}|agent"), true)
            .unwrap()
            .session,
        ONE
    );
}

/// A listing that stops short — the psmux divergence (ADR-13) — reads as an
/// unstamped window rather than being dropped, and the prefix still says
/// what the window is.
#[test]
fn a_listing_without_the_stamp_fields_still_discovers_the_window() {
    let parsed = parse_discovered("%1|tbs-fleet|0", true).expect("parsed");
    assert_eq!(parsed.session, "");
    assert_eq!(parsed.role, WindowRole::Shell);
    assert!(parsed.is_alive);
    assert!(parse_discovered("%1|someone-elses|0", true).is_none());
    assert!(parse_discovered("not-a-pane|tb-fleet|0", true).is_none());
}

/// psmux has **no per-window options**: `set-option -w -t <pane> @k v`
/// writes a *global* one, and `#{@k}` then expands to it on every window
/// (measured on a Windows host, psmux 3.3.6 — ADR-13). So the stamp thurbox
/// wrote for one session is handed back as every window's, and both readings
/// of that lose the pane: the session it names sees several windows claiming
/// it, and every *other* session sees its own window claiming somebody else.
/// Both end at "session has no pane yet", which is the whole of Windows
/// being unattachable from the second session on.
#[test]
fn a_global_stamp_is_not_read_as_every_windows_identity() {
    let listing = |stamps: bool| {
        WindowIndex::from_listing([
            parse_discovered(&format!("%1|tb-first|0|{TWO}|agent"), stamps).expect("parsed"),
            parse_discovered(&format!("%3|tb-second|0|{TWO}|agent"), stamps).expect("parsed"),
        ])
    };

    // A multiplexer whose `#{@...}` is per-window is believed, so one id on
    // two windows is the ambiguity it looks like.
    let stamped = listing(true);
    assert_eq!(stamped.agent_window(TWO, "second"), Located::Unknown);
    assert_eq!(stamped.agent_window(ONE, "first"), Located::Absent);

    // A multiplexer whose `#{@...}` is not per-window says nothing about
    // whose window this is, so the name decides — the pre-ADR-25 shape
    // a server without window options keeps everywhere else.
    let unstamped = listing(false);
    assert_eq!(
        unstamped.agent_window(TWO, "second"),
        Located::At("%3".into())
    );
    assert_eq!(
        unstamped.agent_window(ONE, "first"),
        Located::At("%1".into())
    );
}

/// The role travels on the same global option, so it is dropped with it —
/// otherwise a session's companion shell reports `agent` and is indexed as
/// the agent window, which is the same pane confusion one layer down.
#[test]
fn a_global_role_never_makes_a_shell_window_an_agent() {
    let shell = parse_discovered(&format!("%5|tbs-fleet|0|{ONE}|agent"), false).expect("parsed");
    assert_eq!(shell.role, WindowRole::Shell);
    assert_eq!(shell.session, "");
}

#[test]
fn parse_pane_dead_only_accepts_one() {
    assert!(parse_pane_dead("1"));
    assert!(parse_pane_dead("1\n"));
    assert!(!parse_pane_dead("0\n"));

    // A missing window makes `display-message` exit 0 printing nothing.
    // Reading that as dead would mask the `send-keys` "can't find window"
    // error that actually diagnoses it, turning a typo into "has exited".
    assert!(!parse_pane_dead(""));
    assert!(!parse_pane_dead("\n"));

    // Never infer deadness from anything but the flag itself.
    assert!(!parse_pane_dead("10"));
    assert!(!parse_pane_dead("dead"));
}

#[test]
fn exit_zero_mux_errors_are_still_errors() {
    let mut output = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .output()
            .expect("run a successful command")
    } else {
        std::process::Command::new("true")
            .output()
            .expect("run a successful command")
    };
    assert!(output.status.success());

    output.stderr = b"psmux diagnostic".to_vec();
    assert!(mux_reported_error(&output, false, false));
    assert!(mux_reported_error(&output, true, false));
    output.stderr.clear();
    output.stdout = b"ERROR: can't find window".to_vec();
    assert!(mux_reported_error(&output, false, true));
    assert!(!mux_reported_error(&output, false, false));
    output.stdout = b"ordinary command answer".to_vec();
    assert!(!mux_reported_error(&output, false, true));
    output.stderr = b"** informational SSH advisory\n".to_vec();
    assert!(!mux_reported_error(&output, true, true));
    output.stdout = b"ERROR: remote psmux failure".to_vec();
    assert!(mux_reported_error(&output, true, true));
    assert_eq!(mux_failure(&output, true), "ERROR: remote psmux failure");
}

// --- mouse_seed_bytes tests (adopt-time mouse-mode restore) ---

#[test]
fn mouse_seed_replays_the_mode_codex_asks_for() {
    // What tmux reports for a running Codex: `?1003` with SGR encoding.
    assert_eq!(mouse_seed_bytes("0,0,1,1,1,0"), b"\x1b[?1003h\x1b[?1006h");
}

#[test]
fn mouse_seed_is_empty_for_a_pane_with_no_tracking() {
    assert!(mouse_seed_bytes("0,0,0,0,0,0").is_empty());
    // An encoding alone tracks nothing, so it is not replayed either.
    assert!(mouse_seed_bytes("0,0,0,0,1,0").is_empty());
    assert!(mouse_seed_bytes("").is_empty());
}

#[test]
fn mouse_seed_falls_back_to_press_reporting_on_an_unknown_mode_flag() {
    // A server that leaves a flag's format unexpanded still says some
    // mode is on; the wheel needs no more than `?1000`.
    assert_eq!(mouse_seed_bytes(",,,1,1,"), b"\x1b[?1000h\x1b[?1006h");
}

#[test]
fn mouse_seed_lands_a_parser_in_the_reported_mode() {
    let mut parser = vt100::Parser::new(2.try_into().unwrap(), 2.try_into().unwrap(), 0);
    parser.process(&mouse_seed_bytes("0,1,0,1,0,1"));
    let screen = parser.screen();
    assert_eq!(
        screen.mouse_protocol_mode(),
        vt100::MouseProtocolMode::ButtonMotion
    );
    assert_eq!(
        screen.mouse_protocol_encoding(),
        vt100::MouseProtocolEncoding::Utf8
    );
}

// --- title_seed_bytes tests (adopt-time activity-line restore) ---

#[test]
fn title_seed_replays_an_agent_title_as_osc_2() {
    assert_eq!(
        title_seed_bytes("devbox", "\u{2733} Terminal name lost on restart"),
        "\x1b]2;\u{2733} Terminal name lost on restart\x1b\\".as_bytes()
    );
}

#[test]
fn title_seed_suppresses_tmuxs_default_title() {
    // A pane nothing ever titled reads back as the host's own short name.
    assert!(title_seed_bytes("devbox", "devbox").is_empty());
    assert!(title_seed_bytes("devbox", "  devbox  ").is_empty());
    assert!(title_seed_bytes("devbox", "   ").is_empty());
}

#[test]
fn title_seed_drops_control_characters() {
    // The title is remote-controlled text and the sequence it goes into is
    // terminated by an escape, so a title carrying one must not close it.
    let seed = title_seed_bytes("h", "done\x1b\\ + rm -rf\x07\nnext");
    assert_eq!(seed, "\x1b]2;done\\ + rm -rfnext\x1b\\".as_bytes());
}

#[test]
fn title_seed_bounds_a_huge_title() {
    let seed = title_seed_bytes("h", &"\u{00e9}".repeat(4_000));
    // The budget is the payload's; the introducer and terminator sit
    // outside it. Multi-byte chars must not be split to reach it either.
    assert!(
        seed.len() <= control_mode::MAX_TITLE_SEED_BYTES + 6,
        "{}",
        seed.len()
    );
    assert!(std::str::from_utf8(&seed).is_ok());
}

// --- pane state (cursor / foreground process / live cwd) ---

/// Build the `display-message` answer tmux produces for the format
/// `pane_state` asks for, so the tests speak in fields rather than bytes.
fn pane_state_answer(fields: &[&str]) -> String {
    format!("{}\n", fields.join(&PANE_STATE_SEP.to_string()))
}

#[test]
fn parse_pane_state_reads_every_field() {
    let (state, tty, _) = parse_pane_state(&pane_state_answer(&[
        "12",
        "34",
        "node",
        "/home/u/repo",
        "/dev/pts/7",
    ]));
    assert_eq!(state.cursor_row, Some(12));
    assert_eq!(state.cursor_col, Some(34));
    assert_eq!(state.foreground_process.as_deref(), Some("node"));
    assert_eq!(state.foreground_cwd.as_deref(), Some("/home/u/repo"));
    assert_eq!(tty.as_deref(), Some("/dev/pts/7"));
    // Only the `ps` pass can fill this in — the tmux answer never does.
    assert_eq!(state.foreground_command, None);
}

#[test]
fn parse_pane_state_reads_the_answer_an_older_tmux_prints() {
    // Byte for byte what tmux 3.4 — ubuntu-24.04's, and so CI's — answers
    // the same `display-message`: its `vis(3)` pass rewrites the separator
    // to its octal escape, which used to parse as one field and report
    // every pane fact null.
    let raw = "12\\03734\\037node\\037/home/u/repo\\037/dev/pts/7\\0370\\037tb-demo\n";
    let (state, tty, window) = parse_pane_state(raw);
    assert_eq!(state.cursor_row, Some(12));
    assert_eq!(state.cursor_col, Some(34));
    assert_eq!(state.foreground_process.as_deref(), Some("node"));
    assert_eq!(state.foreground_cwd.as_deref(), Some("/home/u/repo"));
    assert_eq!(state.dead, Some(false));
    assert_eq!(tty.as_deref(), Some("/dev/pts/7"));
    assert_eq!(window.as_deref(), Some("tb-demo"));
}

#[test]
fn parse_pane_state_keeps_a_path_with_spaces_whole() {
    // Why the separator is a control byte and not whitespace: a path may
    // contain spaces, and splitting on them would report half of one.
    let (state, tty, _) = parse_pane_state(&pane_state_answer(&[
        "0",
        "0",
        "my agent",
        "/home/u/My Repo/sub dir",
        "/dev/pts/1",
    ]));
    assert_eq!(
        state.foreground_cwd.as_deref(),
        Some("/home/u/My Repo/sub dir")
    );
    assert_eq!(state.foreground_process.as_deref(), Some("my agent"));
    assert_eq!(tty.as_deref(), Some("/dev/pts/1"));
}

#[test]
fn parse_pane_state_reports_an_unanswered_field_as_absent() {
    // A multiplexer that does not know a format expands it to nothing
    // (psmux). An empty string would read downstream as a real answer —
    // a cursor at an unknown row is not a cursor at row 0.
    let (state, tty, _) = parse_pane_state(&pane_state_answer(&["", "", "", "", ""]));
    assert_eq!(state, PaneState::default());
    assert_eq!(tty, None);

    // And a truncated answer leaves the fields it never carried absent
    // rather than shifting later values into earlier slots.
    let (state, tty, _) = parse_pane_state(&pane_state_answer(&["3", "4"]));
    assert_eq!(state.cursor_row, Some(3));
    assert_eq!(state.cursor_col, Some(4));
    assert_eq!(state.foreground_cwd, None);
    assert_eq!(tty, None);
}

#[test]
fn parse_pane_state_reports_which_window_answered() {
    // The field that makes the answer attributable: `display-message`
    // against a target it cannot resolve answers for the client's current
    // pane and exits 0, so without this the caller cannot tell a session's
    // own pane from a stranger's.
    let (_, _, window) = parse_pane_state(&pane_state_answer(&[
        "0",
        "0",
        "claude",
        "/w",
        "/dev/pts/2",
        "0",
        "tb-demo",
    ]));
    assert_eq!(window.as_deref(), Some("tb-demo"));
}

#[test]
fn parse_pane_state_reads_whether_the_panes_command_has_exited() {
    // `remain-on-exit=on` keeps a dead pane's frame, and tmux keeps naming
    // the command that died in it — so "what is running here" is only
    // answerable with this flag beside it.
    let (state, _, _) = parse_pane_state(&pane_state_answer(&[
        "0",
        "0",
        "claude",
        "/w",
        "/dev/pts/2",
        "1",
    ]));
    assert_eq!(state.dead, Some(true));
    assert_eq!(state.foreground_process.as_deref(), Some("claude"));

    let (live, _, _) = parse_pane_state(&pane_state_answer(&[
        "0",
        "0",
        "claude",
        "/w",
        "/dev/pts/2",
        "0",
    ]));
    assert_eq!(live.dead, Some(false));

    // A multiplexer that does not know the format expands it to nothing,
    // and "not answered" is not "alive".
    let (unknown, _, _) = parse_pane_state(&pane_state_answer(&[
        "0",
        "0",
        "claude",
        "/w",
        "/dev/pts/2",
        "",
    ]));
    assert_eq!(unknown.dead, None);
}

#[test]
fn parse_pane_state_reads_a_dead_panes_exit_status() {
    let answer = |status: &str| {
        parse_pane_state(&pane_state_answer(&[
            "0",
            "0",
            "env",
            "/w",
            "/dev/pts/2",
            "1",
            "tb-demo",
            "%3",
            status,
        ]))
        .0
        .exit_status
    };
    assert_eq!(answer("1"), Some(1));
    assert_eq!(answer("127"), Some(127));
    assert_eq!(answer(""), None);
}

#[test]
fn parse_pane_state_reads_whether_a_dead_pane_was_reaped() {
    let reaped = |dead: &str, status: &str, signal: &str, version: &str| {
        parse_pane_state(&pane_state_answer(&[
            "0",
            "0",
            "env",
            "/w",
            "/dev/pts/2",
            dead,
            "tb-demo",
            "%3",
            status,
            signal,
            version,
        ]))
        .0
        .reaped
    };
    assert_eq!(reaped("1", "3", "", "3.4"), Some(true));
    assert_eq!(reaped("1", "", "KILL", "3.4"), Some(true));
    assert_eq!(reaped("1", "3", "", "3.2a"), Some(true));
    // Dead on tmux 3.3–3.6 before its program is reaped.
    assert_eq!(reaped("1", "", "", "3.4"), Some(false));
    // tmux 3.2 cannot say whether a signal killed it.
    assert_eq!(reaped("1", "", "", "3.2a"), None);
    assert_eq!(reaped("1", "", "", ""), None);
    assert_eq!(reaped("0", "", "", "3.4"), None);
}

#[test]
fn parse_pane_state_survives_a_dead_or_missing_pane() {
    // `display-message` against a window that is gone exits 0 printing an
    // empty line — the same shape `parse_pane_dead` guards against.
    let (state, tty, _) = parse_pane_state("");
    assert_eq!(state, PaneState::default());
    assert_eq!(tty, None);
}

#[test]
fn ps_foreground_prefers_the_group_leader_over_its_pipeline() {
    // `tpgid` is the tty's foreground group; the rows whose own `pgid`
    // equals it are that job, and its leader is the command to report.
    let out = "\
 4210  4210  4300 -bash
 4300  4300  4300 node /opt/cursor-agent/cli.js --resume
 4301  4300  4300 tee /tmp/log
";
    let (argv0, command) = parse_ps_foreground(out).expect("a foreground job");
    assert_eq!(argv0, "node");
    // The whole point of the argv: a bare command *name* is `node` for both
    // an agent CLI and a REPL, and only this tells them apart.
    assert_eq!(command, "node /opt/cursor-agent/cli.js --resume");
}

#[test]
fn ps_foreground_falls_back_to_a_group_member() {
    // The leader can have exited while the rest of its group runs on.
    let out = " 4301  4300  4300 tee /tmp/log\n";
    assert_eq!(
        parse_ps_foreground(out).map(|(argv0, _)| argv0),
        Some("tee".to_string())
    );
}

#[test]
fn ps_foreground_reports_nothing_when_nothing_holds_the_tty() {
    // -1 is "no foreground group"; 0 is `ps` saying it does not know.
    // Neither is a process, and reporting the background shell for either
    // would be a plausible wrong answer rather than an honest absence.
    assert_eq!(parse_ps_foreground(" 4210 4210 -1 -bash\n"), None);
    assert_eq!(parse_ps_foreground(" 4210 4210 0 -bash\n"), None);
    assert_eq!(parse_ps_foreground(""), None);
    // A background job is not the foreground one either.
    assert_eq!(parse_ps_foreground(" 4210 4210 4300 -bash\n"), None);
}

#[test]
fn ps_rows_survive_right_aligned_padding_and_junk() {
    // `ps` pads its numeric columns to the widest value, so a narrow pid
    // arrives behind several spaces — which is what `splitn` mis-parses.
    let out = "\
    9     9  4300 sh
 4300  4300  4300 vim notes.md
ERROR: something ps printed
";
    assert_eq!(
        parse_ps_foreground(out),
        Some(("vim".to_string(), "vim notes.md".to_string()))
    );
}

// --- history_seed_bytes tests (adopt-time scrollback seeding) ---

#[test]
fn history_seed_converts_newlines_and_trims_trailing_blanks() {
    let raw = b"line1\nline2\n\n\n".to_vec();
    assert_eq!(history_seed_bytes(raw), b"line1\r\nline2".to_vec());
}

#[test]
fn history_seed_empty_capture_yields_empty_seed() {
    assert_eq!(history_seed_bytes(Vec::new()), Vec::<u8>::new());
    assert_eq!(history_seed_bytes(b"\n\n\n".to_vec()), Vec::<u8>::new());
}

#[test]
fn history_seed_preserves_escape_sequences_and_inner_blanks() {
    let raw = b"\x1b[31mred\x1b[0m\n\nplain\n".to_vec();
    assert_eq!(
        history_seed_bytes(raw),
        b"\x1b[31mred\x1b[0m\r\n\r\nplain".to_vec()
    );
}

#[test]
fn seeded_parser_exposes_history_as_scrollback() {
    // Feed more lines than the screen height: the overflow must land in
    // the parser's scrollback, scrollable from the UI.
    let mut parser = vt100::Parser::new(5.try_into().unwrap(), 80.try_into().unwrap(), 100);
    let raw: Vec<u8> = (1..=10)
        .map(|i| format!("line{i}\n"))
        .collect::<String>()
        .into_bytes();
    parser.process(&history_seed_bytes(raw));

    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 5);
    assert!(parser.screen().contents().contains("line1"));
    parser.screen_mut().set_scrollback(0);
    assert!(parser.screen().contents().contains("line10"));
}
