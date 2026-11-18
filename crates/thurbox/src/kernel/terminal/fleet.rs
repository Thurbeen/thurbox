//! A Fleet session's panes (ADR-41): the terminal stream `fleet attach
//! <task> --json` names, and its companion shell, the one `fleet shell <task>
//! --json` names, each opened as a [stream pane](super::stream_pane).
//!
//! The pane's backend is the stream itself, not one of the registry's
//! multiplexers: a resize goes up as a `resize` frame, and everything a
//! multiplexer would do to a window (create, kill, stamp, list) is Fleet's,
//! through its command line, so it is refused here or does nothing. Killing
//! the pane detaches from the stream; the agent keeps running.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use term_stream::{Size, Up};

use super::stream_pane::{adopt_with_upstream, Notices, Upstream};

/// What a Fleet row's backend is named: [`crate::kernel::snapshot::fleet`]
/// publishes every Task under it.
pub(super) const BACKEND: &str = crate::kernel::snapshot::fleet::BACKEND;

/// Open `task`'s pane at `rows` × `cols`: ask the Fleet `link` reaches for
/// the Task's stream, and open it on that Fleet's machine: the argv names
/// paths there, so it runs through the same link (over ssh, with no pty, so
/// the stream's frames stay 8-bit clean). Input it refuses is said on
/// `notices`.
pub(super) fn open(
    link: &crate::fleet_link::FleetLink,
    task: &str,
    rows: u16,
    cols: u16,
    scrollback: usize,
    notices: Notices,
) -> Result<crate::backend::Session, OpenFailure> {
    let argv = stream_argv(link, "attach", task)?;
    let (adopted, upstream) = adopt_with_upstream(link.launch(&argv)?, rows, cols, Some(notices))
        .map_err(|e| format!("the stream pane: {e}"))?;
    let backend: Arc<dyn crate::backend::SessionBackend> = Arc::new(Stream(upstream));
    Ok(crate::backend::Session::adopt_io(
        task.to_string(),
        rows,
        cols,
        task,
        &backend,
        adopted,
        scrollback,
    ))
}

/// Open `task`'s companion shell at `rows` × `cols`, as [`open`] opens its
/// pane: the stream `fleet shell` names, run through the same link. The
/// runtime keeps the shell, on the Task's place: a second open finds the one
/// there, and Fleet's release of the Task closes it. The backend that comes
/// back is the stream's way up, which the shell's input and resize take.
pub(super) fn open_shell(
    link: &crate::fleet_link::FleetLink,
    task: &str,
    rows: u16,
    cols: u16,
    scrollback: usize,
    notices: Notices,
) -> Result<
    (
        crate::backend::pane::ShellPane,
        Arc<dyn crate::backend::SessionBackend>,
    ),
    String,
> {
    let argv = stream_argv(link, "shell", task).map_err(|failure| failure.message)?;
    let (adopted, upstream) = adopt_with_upstream(link.launch(&argv)?, rows, cols, Some(notices))
        .map_err(|e| format!("the shell's stream: {e}"))?;
    let backend: Arc<dyn crate::backend::SessionBackend> = Arc::new(Stream(upstream));
    let shell = crate::backend::Session::shell_over(
        adopted,
        &backend,
        &super::shell_surface(task),
        rows,
        cols,
        scrollback,
    );
    Ok((shell, backend))
}

/// The argv `fleet <verb> <task> --json` answers, `attach` or `shell`:
/// `{"stream": {"argv": […]}}`.
fn stream_argv(
    link: &crate::fleet_link::FleetLink,
    verb: &str,
    task: &str,
) -> Result<Vec<String>, OpenFailure> {
    let out = link
        .command(&[verb, task, "--json"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not start fleet {verb}: {e}"))?;
    if let Some(why) = link.unreachable(&out) {
        return Err(why.into());
    }
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr
            .lines()
            .find_map(|line| line.strip_prefix("error: "))
            .unwrap_or(stderr.trim());
        return Err(OpenFailure {
            message: format!("fleet {verb}: {why}"),
            refused: out.status.code() == Some(FLEET_EXIT_REFUSED_BY_TASK_STATE),
        });
    }
    let answer: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("fleet {verb} answered no JSON: {e}"))?;
    let argv: Vec<String> = answer["stream"]["argv"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|arg| arg.as_str().map(str::to_string))
        .collect();
    if argv.is_empty() {
        return Err(format!("fleet {verb} answered no stream: {answer}").into());
    }
    Ok(argv)
}

const FLEET_EXIT_REFUSED_BY_TASK_STATE: i32 = 3;

#[derive(Debug)]
pub(super) struct OpenFailure {
    pub(super) message: String,
    pub(super) refused: bool,
}

impl From<String> for OpenFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            refused: false,
        }
    }
}

fn refused(what: &str) -> crate::backend::BackendError {
    anyhow::anyhow!("a Fleet session's pane cannot {what}: Fleet owns the session").into()
}

/// The backend of one Fleet pane: its stream's way up.
struct Stream(Upstream);

impl crate::backend::PaneStream for Stream {
    fn spawn(
        &self,
        _: &str,
        _: &str,
        _: &[String],
        _: Option<&Path>,
        _: &HashMap<String, String>,
        _: u16,
        _: u16,
    ) -> crate::backend::BackendResult<crate::backend::SpawnedSession> {
        Err(refused("spawn"))
    }

    fn adopt(
        &self,
        _: &str,
        _: u16,
        _: u16,
        _: Option<Vec<u8>>,
    ) -> crate::backend::BackendResult<crate::backend::AdoptedSession> {
        Err(refused("be adopted again"))
    }

    fn resize(&self, _: &str, rows: u16, cols: u16) -> crate::backend::BackendResult<()> {
        self.0
            .send(Up::Resize(Size { rows, cols }))
            .map_err(|e| anyhow::anyhow!("resize the stream: {e}").into())
    }

    fn detach(&self, _: &str) -> crate::backend::BackendResult<()> {
        let _ = self.0.send(Up::Detach);
        Ok(())
    }
}

impl crate::backend::WindowLifecycle for Stream {
    fn check_available(&self) -> crate::backend::BackendResult<()> {
        Ok(())
    }

    fn ensure_ready(&self) -> crate::backend::BackendResult<()> {
        Ok(())
    }

    fn pane_state(&self, _: &str) -> crate::backend::BackendResult<crate::backend::PaneState> {
        Err(refused("report a pane state"))
    }

    fn pane_path(&self, _: &str) -> crate::backend::BackendResult<Option<String>> {
        Ok(None)
    }

    fn create_window(
        &self,
        _: &crate::backend::WindowSpec<'_>,
    ) -> crate::backend::BackendResult<crate::backend::PaneId> {
        Err(refused("create a window"))
    }

    fn locate(
        &self,
        _: crate::backend::Owner<'_>,
    ) -> crate::backend::BackendResult<crate::backend::Placed> {
        Err(refused("locate a window"))
    }

    fn rename_windows(
        &self,
        _: crate::backend::Owner<'_>,
        _: &str,
    ) -> crate::backend::BackendResult<()> {
        Err(refused("rename a window"))
    }

    fn stamp_window(
        &self,
        _: &str,
        _: &str,
        _: crate::backend::WindowRole,
    ) -> crate::backend::BackendResult<()> {
        Ok(())
    }

    fn window_panes(&self, _: &str) -> crate::backend::BackendResult<Vec<(String, bool)>> {
        Ok(Vec::new())
    }

    fn set_pane_retention(&self, _: &str, _: bool) -> crate::backend::BackendResult<()> {
        Ok(())
    }

    fn shutdown(&self) {}

    fn default_shell(&self) -> String {
        "/bin/sh".to_string()
    }

    fn discover(&self) -> crate::backend::BackendResult<Vec<crate::backend::DiscoveredSession>> {
        Ok(Vec::new())
    }

    fn is_dead(&self, _: &str) -> crate::backend::BackendResult<bool> {
        Ok(false)
    }

    /// A detach: deleting the session is a command, through Fleet.
    fn kill(&self, pane: &str) -> crate::backend::BackendResult<()> {
        crate::backend::PaneStream::detach(self, pane)
    }

    fn pane_pid(&self, _: &str) -> crate::backend::BackendResult<Option<u32>> {
        Ok(None)
    }
}

impl crate::backend::PaneIo for Stream {
    fn send_text(&self, _: &str, text: &str, submit: bool) -> crate::backend::BackendResult<()> {
        let mut bytes = text.as_bytes().to_vec();
        if submit {
            bytes.push(b'\r');
        }
        self.0
            .send(Up::Input(bytes))
            .map_err(|e| anyhow::anyhow!("type into the stream: {e}").into())
    }

    fn send_text_after(
        &self,
        _: &str,
        _: &str,
        _: std::time::Duration,
    ) -> crate::backend::BackendResult<()> {
        Err(refused("type later"))
    }

    fn send_key(&self, _: &str, _: &crate::backend::Key) -> crate::backend::BackendResult<String> {
        Err(refused("send a named key"))
    }

    fn capture(&self, _: &str, _: u32, _: bool) -> crate::backend::BackendResult<String> {
        Err(refused("capture"))
    }
}

impl crate::backend::StatusChannel for Stream {
    fn hook_signal_command(&self) -> Option<String> {
        None
    }

    fn record_hook_state(&self, _: &str, _: &str) -> crate::backend::BackendResult<()> {
        Err(refused("record a hook state"))
    }

    fn hook_states(&self) -> crate::backend::BackendResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    fn take_hook_state_events(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

impl crate::backend::SessionBackend for Stream {
    fn name(&self) -> &str {
        BACKEND
    }

    fn capabilities(&self) -> crate::backend::Capabilities {
        crate::backend::Capabilities {
            needs_liveness_poll: false,
            ..crate::backend::Capabilities::NONE
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The attach is asked of the Fleet through its link, and its stream's
    /// argv read from the answer.
    #[test]
    fn an_attach_goes_through_the_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "[ \"$1 $2 $3\" = 'attach review --json' ] || exit 1\n\
             echo '{\"stream\":{\"argv\":[\"runtime-mux\",\"view\",\"w 1\"]}}'",
        );
        let argv = crate::paths::with_path(remote.path(), || {
            stream_argv(&remote.link, "attach", "review")
        })
        .expect("the stream's argv");
        assert_eq!(argv, ["runtime-mux", "view", "w 1"]);
        assert!(
            remote.called("attach review --json"),
            "{:?}",
            remote.calls()
        );
    }

    /// The companion shell is asked of the Fleet through its link too.
    #[test]
    fn a_shell_goes_through_the_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "[ \"$1 $2 $3\" = 'shell review --json' ] || exit 1\n\
             echo '{\"stream\":{\"argv\":[\"runtime-mux\",\"view\",\"--pane\",\"%7\"]}}'",
        );
        let argv = crate::paths::with_path(remote.path(), || {
            stream_argv(&remote.link, "shell", "review")
        })
        .expect("the shell stream's argv");
        assert_eq!(argv, ["runtime-mux", "view", "--pane", "%7"]);
    }
}
