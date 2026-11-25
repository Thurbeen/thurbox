use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Result};

use crate::settings::MuxSettings;

pub fn app_dir_name() -> &'static str {
    if cfg!(dev_build) {
        "thurbox-dev"
    } else {
        "thurbox"
    }
}

pub fn resolve_cli_binary() -> PathBuf {
    let cli_name = format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        let dir = exe.parent().map(through_current_release);
        if exe.file_name().and_then(std::ffi::OsStr::to_str) == Some(cli_name.as_str()) {
            return dir
                .map(|dir| dir.join(&cli_name))
                .filter(|cli| cli.exists())
                .unwrap_or(exe);
        }
        if let Some(sibling) = dir.map(|dir| dir.join(&cli_name)) {
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from(cli_name)
}

fn through_current_release(dir: &Path) -> PathBuf {
    let current = || {
        let releases = dir.parent()?.parent()?;
        (dir.file_name()? == "bin" && releases.file_name()? == "releases")
            .then(|| releases.parent())
            .flatten()
            .map(|home| home.join("current/bin"))
    };
    current()
        .filter(|current| current.is_dir())
        .unwrap_or_else(|| dir.to_path_buf())
}

pub fn program_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(through_current_release)
}

pub const TMUX_SOCKET: &str = if cfg!(dev_build) {
    "thurbox-dev"
} else {
    "thurbox"
};

pub const SOCKET_OVERRIDE_ENV: &str = "THURBOX_SOCKET";

pub const SOCKET_OWNER_ENV: &str = "THURBOX_SOCKET_FOR";

pub(crate) fn local_socket(settings: &MuxSettings) -> String {
    if let Some(socket) = settings.socket.as_ref().filter(|s| !s.is_empty()) {
        return socket.clone();
    }
    socket_for(
        std::env::var(SOCKET_OVERRIDE_ENV).ok(),
        std::env::var_os(SOCKET_OWNER_ENV)
            .map(PathBuf::from)
            .as_deref(),
        settings.data_dir.as_deref(),
        settings.relocated_data_dir.as_deref(),
    )
}

fn socket_for(
    override_name: Option<String>,
    socket_owner: Option<&Path>,
    data_dir: Option<&Path>,
    relocated_data_dir: Option<&Path>,
) -> String {
    if let Some(name) = override_name.filter(|s| !s.is_empty()) {
        let inherited_from_elsewhere =
            matches!(socket_owner, Some(owner) if Some(owner) != data_dir);
        if !inherited_from_elsewhere {
            return name;
        }
    }
    match relocated_data_dir {
        Some(dir) => derived_socket(dir),
        None => TMUX_SOCKET.to_string(),
    }
}

fn derived_socket(dir: &Path) -> String {
    let normalized: std::path::PathBuf = dir.components().collect();
    let digest = fnv1a32(normalized.to_string_lossy().as_bytes());
    format!("{TMUX_SOCKET}-{digest:08x}")
}

pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in bytes {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

pub fn local_socket_name(settings: &MuxSettings) -> String {
    local_socket(settings)
}

fn learned_host_sockets() -> &'static Mutex<HashMap<String, String>> {
    static LEARNED: std::sync::OnceLock<Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    LEARNED.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn learn_host_socket(host: &crate::HostDef, socket: &str) {
    if host.socket.is_some() || socket.is_empty() {
        return;
    }
    if let Ok(mut map) = learned_host_sockets().lock() {
        map.insert(host.backend_name(), socket.to_string());
    }
}

pub(crate) fn learned_host_socket(backend_name: &str) -> Option<String> {
    learned_host_sockets()
        .lock()
        .ok()
        .and_then(|map| map.get(backend_name).cloned())
}

pub fn host_socket(host: &crate::HostDef) -> String {
    host.socket
        .clone()
        .or_else(|| learned_host_socket(&host.backend_name()))
        .unwrap_or_else(|| TMUX_SOCKET.to_string())
}

pub const SOCKET_DIR_NAME: &str = "tmux";

const SOCKET_PATH_MAX: usize = 103;

pub fn local_socket_dir(socket: &str, settings: &MuxSettings) -> Option<PathBuf> {
    let dir = socket_dir_for(
        std::env::var_os("TMUX_TMPDIR").as_deref(),
        settings.data_dir.as_deref(),
        socket,
    )?;
    match create_private_dir(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            tracing::warn!(
                "cannot create the tmux socket directory {}: {e}; using tmux's default",
                dir.display()
            );
            None
        }
    }
}

fn socket_dir_for(
    env: Option<&std::ffi::OsStr>,
    data_dir: Option<&Path>,
    socket: &str,
) -> Option<PathBuf> {
    if env.is_some_and(|value| !value.is_empty()) {
        return None;
    }
    let dir = data_dir?.join(SOCKET_DIR_NAME);
    if !socket_path_fits(&dir.to_string_lossy(), socket) {
        tracing::warn!(
            "the tmux socket path under {} would be too long to bind; using tmux's default",
            dir.display()
        );
        return None;
    }
    Some(dir)
}

pub fn socket_path_fits(dir: &str, socket: &str) -> bool {
    // tmux puts its socket at `<dir>/tmux-<uid>/<name>`; 10 digits hold any 32-bit uid.
    dir.len() + "/tmux-".len() + 10 + 1 + socket.len() <= SOCKET_PATH_MAX
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

pub fn local_socket_dir_report(settings: &MuxSettings) -> Option<String> {
    std::env::var("TMUX_TMPDIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .or_else(|| {
            local_socket_dir(&local_socket(settings), settings).map(|d| d.display().to_string())
        })
}

fn learned_host_socket_dirs() -> &'static Mutex<HashMap<String, Option<String>>> {
    static LEARNED: std::sync::OnceLock<Mutex<HashMap<String, Option<String>>>> =
        std::sync::OnceLock::new();
    LEARNED.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn learn_host_socket_dir(host: &crate::HostDef, dir: Option<&str>) {
    if let Ok(mut map) = learned_host_socket_dirs().lock() {
        map.insert(
            host.backend_name(),
            dir.filter(|d| !d.is_empty()).map(str::to_string),
        );
    }
}

pub(crate) fn learned_host_socket_dir(backend_name: &str) -> Option<Option<String>> {
    learned_host_socket_dirs()
        .lock()
        .ok()
        .and_then(|map| map.get(backend_name).cloned())
}

pub(crate) fn known_host_socket(host: &crate::HostDef) -> Result<String> {
    if let Some(socket) = host
        .socket
        .clone()
        .or_else(|| learned_host_socket(&host.backend_name()))
    {
        return Ok(socket);
    }
    if !host.shareable() {
        return Ok(TMUX_SOCKET.to_string());
    }
    bail!(
        "socket unknown for host '{}': it runs a thurbox of its own and has not \
         reported which socket that is (set `socket` in its host entry, or make its \
         thurbox-cli reachable)",
        host.name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_dir_is_under_the_data_dir_unless_the_environment_names_one() {
        let data = Path::new("/home/me/.local/share/thurbox");
        assert_eq!(
            socket_dir_for(None, Some(data), "thurbox"),
            Some(data.join("tmux"))
        );
        assert_eq!(
            socket_dir_for(Some(std::ffi::OsStr::new("")), Some(data), "thurbox"),
            Some(data.join("tmux")),
            "an empty TMUX_TMPDIR is tmux's default, not a choice"
        );
        assert_eq!(
            socket_dir_for(
                Some(std::ffi::OsStr::new("/tmp/tbx-x")),
                Some(data),
                "thurbox"
            ),
            None,
            "tooling that scopes its servers keeps them where it put them"
        );
        assert_eq!(socket_dir_for(None, None, "thurbox"), None);
    }

    #[test]
    fn a_socket_dir_too_deep_to_bind_is_not_used() {
        let deep = format!("/{}", "d".repeat(80));
        assert_eq!(
            socket_dir_for(None, Some(Path::new(&deep)), "thurbox"),
            None
        );
        assert!(socket_path_fits(
            "/Users/me/.local/share/thurbox/tmux",
            "thurbox"
        ));
    }

    #[test]
    fn a_hosts_socket_dir_is_learned_even_when_it_is_the_default() {
        let host = crate::HostDef {
            name: "learned-socket-dir-host".into(),
            destination: "me@h".into(),
            ..Default::default()
        };
        let key = host.backend_name();
        assert_eq!(learned_host_socket_dir(&key), None);
        learn_host_socket_dir(&host, None);
        assert_eq!(learned_host_socket_dir(&key), Some(None));
        learn_host_socket_dir(&host, Some("/home/me/.local/share/thurbox/tmux"));
        assert_eq!(
            learned_host_socket_dir(&key),
            Some(Some("/home/me/.local/share/thurbox/tmux".into()))
        );
    }

    #[test]
    fn a_learned_socket_is_used_unless_the_host_pins_one() {
        let learned = crate::HostDef {
            name: "learned-socket-host".into(),
            destination: "me@h".into(),
            ..Default::default()
        };
        assert_eq!(host_socket(&learned), TMUX_SOCKET);
        learn_host_socket(&learned, "thurbox");
        assert_eq!(host_socket(&learned), "thurbox");
        let pinned = crate::HostDef {
            name: "pinned-socket-host".into(),
            destination: "me@h".into(),
            socket: Some("mine".into()),
            ..Default::default()
        };
        learn_host_socket(&pinned, "thurbox");
        assert_eq!(host_socket(&pinned), "mine");
    }

    #[test]
    fn a_default_instance_keeps_the_build_socket() {
        assert_eq!(socket_for(None, None, None, None), TMUX_SOCKET);
    }

    #[test]
    fn a_relocated_instance_gets_its_own_socket() {
        let lab = socket_for(
            None,
            None,
            Some(Path::new("/tmp/lab/data")),
            Some(Path::new("/tmp/lab/data")),
        );
        let other = socket_for(
            None,
            None,
            Some(Path::new("/tmp/other/data")),
            Some(Path::new("/tmp/other/data")),
        );
        assert_ne!(lab, TMUX_SOCKET, "a relocated instance leaves the default");
        assert_ne!(other, lab, "two of them do not share a server");
        assert_eq!(
            lab,
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/data")),
                Some(Path::new("/tmp/lab/data"))
            ),
            "and it finds the same server on the next run"
        );
        assert!(
            lab.starts_with(TMUX_SOCKET),
            "still recognisable as thurbox's: {lab}"
        );
        assert!(
            lab.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "safe to splice into a `-L` argument: {lab}"
        );
    }

    #[test]
    fn a_relocated_socket_ignores_separator_noise() {
        assert_eq!(
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/data")),
                Some(Path::new("/tmp/lab/data"))
            ),
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab/./data/")),
                Some(Path::new("/tmp/lab/./data/"))
            ),
        );
    }

    #[test]
    fn an_explicit_socket_wins_over_the_derivation() {
        assert_eq!(
            socket_for(
                Some("thurbox-named".into()),
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            ),
            "thurbox-named"
        );
        assert_eq!(
            socket_for(
                Some(String::new()),
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            ),
            socket_for(
                None,
                None,
                Some(Path::new("/tmp/lab")),
                Some(Path::new("/tmp/lab"))
            )
        );
    }

    #[test]
    fn an_inherited_socket_is_dropped_once_the_data_dir_moves() {
        let lab = Path::new("/tmp/lab");
        let home = Path::new("/home/me/.local/share/thurbox");
        assert_eq!(
            socket_for(Some("thurbox".into()), Some(home), Some(home), None),
            "thurbox"
        );
        assert_eq!(
            socket_for(Some("thurbox".into()), Some(home), Some(lab), Some(lab)),
            derived_socket(lab)
        );
        assert_eq!(
            socket_for(Some("thurbox-named".into()), None, Some(lab), Some(lab)),
            "thurbox-named"
        );
    }

    #[test]
    fn local_socket_honors_env_override() {
        // nextest runs one process per test, so this env mutation races no other test.
        std::env::remove_var(SOCKET_OWNER_ENV);
        std::env::set_var(SOCKET_OVERRIDE_ENV, "thurbox-lab-test");
        assert_eq!(local_socket(&MuxSettings::default()), "thurbox-lab-test");
        std::env::set_var(SOCKET_OVERRIDE_ENV, "");
        assert_eq!(local_socket(&MuxSettings::default()), TMUX_SOCKET);
        std::env::remove_var(SOCKET_OVERRIDE_ENV);
        assert_eq!(local_socket(&MuxSettings::default()), TMUX_SOCKET);
    }
}
