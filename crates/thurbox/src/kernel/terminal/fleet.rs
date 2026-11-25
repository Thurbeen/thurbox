use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use term_stream::{Size, Up};

use super::stream_pane::{adopt_with_upstream, Notices, Upstream};

pub(super) const BACKEND: &str = crate::kernel::snapshot::fleet::BACKEND;

pub(super) fn open(
    link: &crate::fleet_link::FleetLink,
    task: &str,
    rows: u16,
    cols: u16,
    scrollback: usize,
    notices: Notices,
) -> Result<crate::backend::Session, OpenFailure> {
    let stream = link.stream("attach", task)?;
    let (adopted, upstream) = adopt_with_upstream(stream, rows, cols, Some(notices))
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
    let stream = link
        .stream("shell", task)
        .map_err(|failure| failure.message)?;
    let (adopted, upstream) = adopt_with_upstream(stream, rows, cols, Some(notices))
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

impl From<crate::fleet_link::StreamFailure> for OpenFailure {
    fn from(failure: crate::fleet_link::StreamFailure) -> Self {
        Self {
            refused: failure.exit == Some(FLEET_EXIT_REFUSED_BY_TASK_STATE),
            message: failure.message,
        }
    }
}

fn refused(what: &str) -> crate::backend::BackendError {
    anyhow::anyhow!("a Fleet session's pane cannot {what}: Fleet owns the session").into()
}

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
    #[test]
    fn an_attach_goes_through_the_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "[ \"$1 $2 $3\" = 'attach review --json' ] || exit 1\n\
             echo '{\"stream\":{\"argv\":[\"runtime-mux\",\"view\",\"w 1\"]}}'",
        );
        let stream =
            crate::paths::with_path(remote.path(), || remote.link.stream("attach", "review"))
                .expect("the stream");
        let launched = remote_words(&stream);
        assert!(launched.ends_with("runtime-mux view 'w 1'"), "{launched}");
        assert!(
            remote.called("attach review --json"),
            "{:?}",
            remote.calls()
        );
    }

    #[test]
    fn a_shell_goes_through_the_link() {
        let remote = crate::fleet_link::testing::Remote::new(
            "[ \"$1 $2 $3\" = 'shell review --json' ] || exit 1\n\
             echo '{\"stream\":{\"argv\":[\"runtime-mux\",\"view\",\"--pane\",\"%7\"]}}'",
        );
        let stream =
            crate::paths::with_path(remote.path(), || remote.link.stream("shell", "review"))
                .expect("the shell's stream");
        let launched = remote_words(&stream);
        assert!(
            launched.ends_with("runtime-mux view --pane '%7'"),
            "{launched}"
        );
    }

    fn remote_words(command: &std::process::Command) -> String {
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}
