//! Local, instance-scoped control transport for a running interface.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::OnceLock;
use std::time::Duration;

const MAX_REQUEST: usize = 16 * 1024;
const MAX_REPLY: usize = 256 * 1024;
const WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub pid: u32,
    pub started_at_unix_ms: u128,
    pub label: String,
    pub terminal: Option<String>,
    pub endpoint: PathBuf,
}

fn label() -> String {
    if std::env::var_os("SSH_CONNECTION").is_some() {
        "ssh TUI".into()
    } else {
        "local TUI".into()
    }
}

#[cfg(unix)]
fn terminal_hint() -> Option<String> {
    if let Some(tty) = std::env::var_os("SSH_TTY") {
        return Some(tty.to_string_lossy().into_owned());
    }
    let mut buffer = [0i8; 256];
    let status = unsafe { libc::ttyname_r(libc::STDIN_FILENO, buffer.as_mut_ptr(), buffer.len()) };
    (status == 0).then(|| {
        unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    })
}

#[cfg(windows)]
fn terminal_hint() -> Option<String> {
    std::env::var("TERM").ok()
}

fn started_at_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Ping,
    State,
    Watch {
        since: Option<u64>,
    },
    Actions,
    Action {
        name: String,
        args: Value,
    },
    Confirm {
        ticket: String,
    },
    Input {
        target: String,
        input: InputOperation,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputOperation {
    Key { chord: String },
    Text { text: String },
    Scroll { up: bool },
}

#[derive(Debug, Serialize, Deserialize)]
struct WireRequest {
    request_id: String,
    request: Request,
}

impl WireRequest {
    fn valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.request_id).is_ok()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub instance_id: String,
    pub request_id: String,
    pub revision: u64,
    pub result: Value,
}

impl Reply {
    /// Whether this reply fits the local control transport's frame limit.
    pub fn exceeds_limit(&self) -> bool {
        serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > MAX_REPLY)
    }
}

pub struct Pending {
    pub request_id: String,
    pub request: Request,
    /// The transport has already authenticated this local peer.
    pub peer: u32,
    pub reply: mpsc::Sender<Reply>,
    pub deadline: std::time::Instant,
}

pub struct Server {
    pub instance: Instance,
    pub pending: Receiver<Pending>,
    #[cfg(unix)]
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(unix)]
    record: PathBuf,
    #[cfg(windows)]
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    #[cfg(windows)]
    record: PathBuf,
}

pub fn directory() -> Result<PathBuf, String> {
    Ok(crate::paths::data_directory()
        .ok_or("cannot resolve the data directory")?
        .join("ui-control"))
}

struct AuditJob {
    instance: String,
    peer: u32,
    action: String,
    target: Option<String>,
    outcome: String,
    request_id: String,
    reply: Option<mpsc::Sender<Result<(), String>>>,
}

static AUDIT_WORKER: OnceLock<Result<SyncSender<AuditJob>, String>> = OnceLock::new();

fn audit_worker() -> Result<SyncSender<AuditJob>, String> {
    AUDIT_WORKER
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel::<AuditJob>(64);
            std::thread::Builder::new()
                .name("thurbox-ui-audit".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        let result = write_audit(
                            &job.instance,
                            job.peer,
                            &job.action,
                            job.target.as_deref(),
                            &job.outcome,
                            &job.request_id,
                        );
                        if let Some(reply) = job.reply {
                            let _ = reply.send(result);
                        }
                    }
                })
                .map_err(|error| error.to_string())?;
            Ok(tx)
        })
        .clone()
}

/// Record an action before a destructive command or ticket can be issued.
/// The bounded wait keeps a slow audit device from freezing the interface.
pub fn audit(
    instance: &str,
    peer: u32,
    action: &str,
    target: Option<&str>,
    outcome: &str,
    request_id: &str,
) -> Result<(), String> {
    let (reply, result) = mpsc::channel();
    audit_worker()?
        .try_send(AuditJob {
            instance: instance.into(),
            peer,
            action: action.into(),
            target: target.map(str::to_owned),
            outcome: outcome.into(),
            request_id: request_id.into(),
            reply: Some(reply),
        })
        .map_err(|error| error.to_string())?;
    result
        .recv_timeout(Duration::from_millis(100))
        .map_err(|error| error.to_string())?
}

/// Non-destructive telemetry never holds up a user action.
pub fn audit_best_effort(
    instance: &str,
    peer: u32,
    action: &str,
    target: Option<&str>,
    outcome: &str,
    request_id: &str,
) {
    if let Ok(worker) = audit_worker() {
        let _ = worker.try_send(AuditJob {
            instance: instance.into(),
            peer,
            action: action.into(),
            target: target.map(str::to_owned),
            outcome: outcome.into(),
            request_id: request_id.into(),
            reply: None,
        });
    }
}

/// Persist only metadata; query text, input, and terminal content never enter
/// the worker's job.
fn write_audit(
    instance: &str,
    peer: u32,
    action: &str,
    target: Option<&str>,
    outcome: &str,
    request_id: &str,
) -> Result<(), String> {
    #[cfg(unix)]
    let file = unix::open_audit()?;
    #[cfg(windows)]
    let file = windows::open_audit()?;
    #[cfg(unix)]
    let _lock = unix::lock_audit(&file)?;
    #[cfg(windows)]
    let _lock = windows::lock_audit(&file)?;
    let line = serde_json::json!({
        "timestamp_ms": started_at_unix_ms(),
        "instance_id": instance,
        "peer": peer,
        "action": action,
        "target_id": target,
        "outcome": outcome,
        "request_id": request_id,
    });
    if file.metadata().map_err(|e| e.to_string())?.len() > 1024 * 1024 {
        file.set_len(0).map_err(|e| e.to_string())?;
    }
    use std::io::Write;
    writeln!(&file, "{line}").map_err(|e| e.to_string())?;
    file.sync_data().map_err(|e| e.to_string())
}

#[cfg(unix)]
mod unix {
    use super::{
        directory, label, started_at_unix_ms, terminal_hint, Instance, Pending, Reply, Request,
        Server, WireRequest, MAX_REPLY, MAX_REQUEST, WAIT,
    };
    use std::fs;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, SyncSender};
    use std::sync::Arc;

    fn secure_directory() -> Result<PathBuf, String> {
        let dir = directory()?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| e.to_string())?;
        let meta = fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err("UI control directory must be owned by this user and mode 0700".into());
        }
        Ok(dir)
    }

    pub(super) fn open_audit() -> Result<fs::File, String> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = secure_directory()?.join("audit.jsonl");
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err("UI control audit file must be owner-only".into());
        }
        Ok(file)
    }

    pub(super) struct AuditLock<'a>(&'a fs::File);

    pub(super) fn lock_audit(file: &fs::File) -> Result<AuditLock<'_>, String> {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(AuditLock(file))
    }

    impl Drop for AuditLock<'_> {
        fn drop(&mut self) {
            unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }

    #[cfg(test)]
    #[test]
    fn audit_rollover_lock_is_exclusive_across_file_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let first = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let second = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let held = lock_audit(&first).unwrap();
        assert!(lock_audit(&second).is_err());
        drop(held);
        assert!(lock_audit(&second).is_ok());
    }

    fn peer_ok(stream: &UnixStream) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            let rc = unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut cred as *mut libc::ucred).cast(),
                    &mut len,
                )
            };
            rc == 0
                && len as usize == std::mem::size_of::<libc::ucred>()
                && cred.uid == unsafe { libc::geteuid() }
        }
        #[cfg(not(target_os = "linux"))]
        {
            use std::os::fd::AsRawFd;
            let mut uid = 0;
            let mut gid = 0;
            unsafe {
                libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0
                    && uid == libc::geteuid()
            }
        }
    }

    fn write_frame(stream: &mut UnixStream, value: &Reply) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_REPLY {
            return Err("UI control reply is too large".into());
        }
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .map_err(|e| e.to_string())?;
        stream.write_all(&bytes).map_err(|e| e.to_string())
    }

    fn read_frame(stream: &mut UnixStream) -> Result<WireRequest, String> {
        let mut len = [0; 4];
        stream.read_exact(&mut len).map_err(|e| e.to_string())?;
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_REQUEST {
            return Err("UI control request is too large".into());
        }
        let mut bytes = vec![0; len];
        stream.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }

    fn handle(mut stream: UnixStream, sender: SyncSender<Pending>) {
        let _ = stream.set_read_timeout(Some(WAIT));
        let _ = stream.set_write_timeout(Some(WAIT));
        if !peer_ok(&stream) {
            return;
        }
        let Ok(request) = read_frame(&mut stream) else {
            return;
        };
        if !request.valid() {
            return;
        }
        let (reply, rx) = mpsc::channel();
        if sender
            .try_send(Pending {
                request_id: request.request_id,
                request: request.request,
                peer: unsafe { libc::geteuid() },
                reply,
                deadline: std::time::Instant::now() + WAIT,
            })
            .is_err()
        {
            return;
        }
        if let Ok(result) = rx.recv_timeout(WAIT) {
            let _ = write_frame(&mut stream, &result);
        }
    }

    impl Server {
        pub fn start() -> Result<Self, String> {
            let dir = secure_directory()?;
            let id = uuid::Uuid::new_v4().to_string();
            let endpoint = dir.join(format!("{id}.sock"));
            let record = dir.join(format!("{id}.json"));
            let staged = dir.join(format!("{id}.tmp"));
            let listener = UnixListener::bind(&endpoint).map_err(|e| e.to_string())?;
            if let Err(error) = fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)) {
                let _ = fs::remove_file(&endpoint);
                return Err(error.to_string());
            }
            let instance = Instance {
                id,
                pid: std::process::id(),
                started_at_unix_ms: started_at_unix_ms(),
                label: label(),
                terminal: terminal_hint(),
                endpoint: endpoint.clone(),
            };
            let published = (|| -> Result<(), String> {
                fs::write(
                    &staged,
                    serde_json::to_vec(&instance).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                fs::set_permissions(&staged, fs::Permissions::from_mode(0o600))
                    .map_err(|e| e.to_string())?;
                fs::rename(&staged, &record).map_err(|e| e.to_string())
            })();
            if let Err(error) = published {
                let _ = fs::remove_file(&staged);
                let _ = fs::remove_file(&endpoint);
                return Err(error);
            }
            let (tx, pending) = mpsc::sync_channel(32);
            let alive = Arc::new(AtomicBool::new(true));
            let running = Arc::clone(&alive);
            let clients = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            std::thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if !running.load(Ordering::Relaxed) {
                                break;
                            }
                            if clients.fetch_add(1, Ordering::AcqRel) >= 8 {
                                clients.fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            let sender = tx.clone();
                            let clients = Arc::clone(&clients);
                            std::thread::spawn(move || {
                                handle(stream, sender);
                                clients.fetch_sub(1, Ordering::AcqRel);
                            });
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            });
            Ok(Self {
                instance,
                pending,
                alive,
                record,
            })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.alive.store(false, Ordering::Relaxed);
            let _ = UnixStream::connect(&self.instance.endpoint);
            let _ = fs::remove_file(&self.record);
            let _ = fs::remove_file(&self.instance.endpoint);
        }
    }

    pub fn send(instance: &Instance, request: &Request) -> Result<Reply, String> {
        let mut stream = UnixStream::connect(&instance.endpoint).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(WAIT))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(WAIT))
            .map_err(|e| e.to_string())?;
        if !peer_ok(&stream) {
            return Err("UI control endpoint belongs to another user".into());
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let bytes = serde_json::to_vec(&WireRequest {
            request_id: request_id.clone(),
            request: request.clone(),
        })
        .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_REQUEST {
            return Err("UI control request is too large".into());
        }
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .map_err(|e| e.to_string())?;
        stream.write_all(&bytes).map_err(|e| e.to_string())?;
        let mut len = [0; 4];
        stream.read_exact(&mut len).map_err(|e| e.to_string())?;
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_REPLY {
            return Err("UI control reply is too large".into());
        }
        let mut bytes = vec![0; len];
        stream.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let reply: Reply = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if reply.instance_id != instance.id || reply.request_id != request_id {
            return Err("UI control instance identity changed".into());
        }
        Ok(reply)
    }

    pub fn instances() -> Result<Vec<Instance>, String> {
        let dir = secure_directory()?;
        let mut out = Vec::new();
        let entries: Vec<_> = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        for entry in entries {
            let path = entry.path();
            if path.extension().map_or(true, |ext| ext != "json") {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file()
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o077 != 0
            {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(instance) = serde_json::from_slice::<Instance>(&bytes) else {
                let _ = fs::remove_file(&path);
                continue;
            };
            if uuid::Uuid::parse_str(&instance.id).is_err()
                || path != dir.join(format!("{}.json", instance.id))
                || instance.endpoint != dir.join(format!("{}.sock", instance.id))
            {
                let _ = fs::remove_file(&path);
                continue;
            }
            if send(&instance, &Request::Ping).is_ok() {
                out.push(instance);
            } else if matches!(
                UnixStream::connect(&instance.endpoint).map(|_| ()),
                Err(ref error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    )
            ) {
                let _ = fs::remove_file(&path);
                let _ = fs::remove_file(&instance.endpoint);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

#[cfg(unix)]
pub use unix::{instances, send};

#[cfg(windows)]
mod windows {
    use super::{
        directory, label, started_at_unix_ms, terminal_hint, Instance, Pending, Reply, Request,
        Server, WireRequest, MAX_REPLY, MAX_REQUEST, WAIT,
    };
    use std::fs;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, SyncSender};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use tokio::sync::Semaphore;
    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_INVALID_PARAMETER, STILL_ACTIVE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        SetFileSecurityW, DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
    };
    use windows_sys::Win32::Storage::FileSystem::{LockFile, UnlockFile};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    fn process_exited(pid: u32) -> bool {
        if pid == 0 {
            return true;
        }
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return io::Error::last_os_error().raw_os_error()
                == Some(ERROR_INVALID_PARAMETER as i32);
        }
        let mut code = STILL_ACTIVE as u32;
        let read = unsafe { GetExitCodeProcess(handle, &mut code) };
        unsafe {
            CloseHandle(handle);
        }
        read != 0 && code != STILL_ACTIVE as u32
    }

    pub(super) fn open_audit() -> Result<fs::File, String> {
        let path = directory()?.join("audit.jsonl");
        if !path.exists() {
            fs::File::create(&path).map_err(|e| e.to_string())?;
        }
        OwnerAcl::new()?.protect(&path)?;
        // LockFile requires GENERIC_READ or GENERIC_WRITE; append alone grants
        // only FILE_APPEND_DATA on Windows.
        fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(path)
            .map_err(|e| e.to_string())
    }

    pub(super) struct AuditLock<'a>(&'a fs::File);

    pub(super) fn lock_audit(file: &fs::File) -> Result<AuditLock<'_>, String> {
        let locked = unsafe { LockFile(file.as_raw_handle(), 0, 0, u32::MAX, u32::MAX) };
        if locked == 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        Ok(AuditLock(file))
    }

    impl Drop for AuditLock<'_> {
        fn drop(&mut self) {
            unsafe { UnlockFile(self.0.as_raw_handle(), 0, 0, u32::MAX, u32::MAX) };
        }
    }

    #[cfg(test)]
    #[test]
    fn audit_rollover_lock_is_exclusive_across_file_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let first = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .unwrap();
        let second = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .unwrap();
        let held = lock_audit(&first).unwrap();
        assert!(lock_audit(&second).is_err());
        drop(held);
        assert!(lock_audit(&second).is_ok());
    }

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    // OW grants only the object owner; SY keeps local system access. The same
    // descriptor protects discovery and every pipe instance.
    struct OwnerAcl(*mut std::ffi::c_void);

    impl OwnerAcl {
        fn new() -> Result<Self, String> {
            let sddl = wide(std::ffi::OsStr::new("D:P(A;;GA;;;OW)(A;;GA;;;SY)"));
            let mut descriptor = std::ptr::null_mut();
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            Ok(Self(descriptor))
        }

        fn attrs(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.0,
                bInheritHandle: 0,
            }
        }

        fn protect(&self, path: &std::path::Path) -> Result<(), String> {
            let path = wide(path.as_os_str());
            let ok = unsafe { SetFileSecurityW(path.as_ptr(), DACL_SECURITY_INFORMATION, self.0) };
            if ok == 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            Ok(())
        }
    }

    #[cfg(test)]
    #[test]
    fn pipe_security_descriptor_grants_only_owner_and_system() {
        use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
        let acl = OwnerAcl::new().expect("owner ACL");
        let mut text = std::ptr::null_mut();
        let ok = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                acl.0,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0);
        let len = unsafe { (0..).find(|&i| *text.add(i) == 0).unwrap() };
        let descriptor = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        unsafe { LocalFree(text.cast()) };
        assert!(descriptor.contains("(A;;GA;;;OW)"), "{descriptor}");
        assert!(descriptor.contains("(A;;GA;;;SY)"), "{descriptor}");
        assert!(!descriptor.contains(";;;WD)"), "{descriptor}");
        assert!(!descriptor.contains(";;;AN)"), "{descriptor}");
    }

    impl Drop for OwnerAcl {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    fn runtime() -> Result<tokio::runtime::Runtime, String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
    }

    async fn handle(mut pipe: NamedPipeServer, sender: SyncSender<Pending>) {
        let run = async {
            let mut len = [0; 4];
            pipe.read_exact(&mut len).await?;
            let len = u32::from_be_bytes(len) as usize;
            if len > MAX_REQUEST {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request too large",
                ));
            }
            let mut bytes = vec![0; len];
            pipe.read_exact(&mut bytes).await?;
            let request: WireRequest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if !request.valid() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid request ID",
                ));
            }
            let (reply, rx) = mpsc::channel();
            sender
                .try_send(Pending {
                    request_id: request.request_id,
                    request: request.request,
                    peer: 0, // The pipe ACL admits only its owner (and local system).
                    reply,
                    deadline: std::time::Instant::now() + WAIT,
                })
                .map_err(|_| io::Error::other("UI control queue is full"))?;
            let result = tokio::task::spawn_blocking(move || rx.recv_timeout(WAIT))
                .await
                .map_err(io::Error::other)?
                .map_err(io::Error::other)?;
            let bytes = serde_json::to_vec(&result).map_err(io::Error::other)?;
            if bytes.len() > MAX_REPLY {
                return Err(io::Error::other("UI control reply is too large"));
            }
            pipe.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
            pipe.write_all(&bytes).await?;
            Ok::<(), io::Error>(())
        };
        let _ = tokio::time::timeout(WAIT, run).await;
    }

    impl Server {
        pub fn start() -> Result<Self, String> {
            let dir = directory()?;
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let acl = OwnerAcl::new()?;
            acl.protect(&dir)?;
            let id = uuid::Uuid::new_v4().to_string();
            let endpoint = PathBuf::from(format!(r"\\.\pipe\thurbox-ui-{id}"));
            let record = dir.join(format!("{id}.json"));
            let staged = dir.join(format!("{id}.tmp"));
            let instance = Instance {
                id,
                pid: std::process::id(),
                started_at_unix_ms: started_at_unix_ms(),
                label: label(),
                terminal: terminal_hint(),
                endpoint,
            };
            let (tx, pending) = mpsc::sync_channel(32);
            let (shutdown, mut shutdown_rx) = tokio::sync::oneshot::channel();
            let (started, ready) = mpsc::sync_channel(1);
            let clients = Arc::new(Semaphore::new(8));
            let pipe_name = instance.endpoint.clone();
            std::thread::spawn(move || {
                let Ok(rt) = runtime() else {
                    let _ = started.send(Err("cannot start the UI control runtime".to_string()));
                    return;
                };
                rt.block_on(async move {
                    let Ok(acl) = OwnerAcl::new() else {
                        let _ = started.send(Err("cannot secure the UI control pipe".to_string()));
                        return;
                    };
                    let mut first = true;
                    loop {
                        let mut attrs = acl.attrs();
                        let mut options = ServerOptions::new();
                        options.reject_remote_clients(true).max_instances(255);
                        let Ok(pipe) = (unsafe {
                            options.create_with_security_attributes_raw(
                                &pipe_name,
                                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
                            )
                        }) else {
                            if first {
                                let _ = started
                                    .send(Err("cannot create the UI control pipe".to_string()));
                            }
                            break;
                        };
                        if first {
                            let _ = started.send(Ok(()));
                            first = false;
                        }
                        tokio::select! {
                            result = pipe.connect() => {
                                if result.is_err() { break; }
                                let Ok(permit) = Arc::clone(&clients).try_acquire_owned() else {
                                    continue;
                                };
                                let sender = tx.clone();
                                tokio::spawn(async move {
                                    handle(pipe, sender).await;
                                    drop(permit);
                                });
                            }
                            _ = &mut shutdown_rx => break,
                        }
                    }
                });
            });
            match ready.recv_timeout(WAIT) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    let _ = shutdown.send(());
                    return Err(error);
                }
                Err(error) => {
                    let _ = shutdown.send(());
                    return Err(error.to_string());
                }
            }
            let published = (|| -> Result<(), String> {
                fs::write(
                    &staged,
                    serde_json::to_vec(&instance).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                acl.protect(&staged)?;
                fs::rename(&staged, &record).map_err(|e| e.to_string())
            })();
            if let Err(error) = published {
                let _ = shutdown.send(());
                let _ = fs::remove_file(&staged);
                return Err(error);
            }
            Ok(Self {
                instance,
                pending,
                shutdown: Some(shutdown),
                record,
            })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            let _ = fs::remove_file(&self.record);
        }
    }

    pub fn send(instance: &Instance, request: &Request) -> Result<Reply, String> {
        runtime()?.block_on(async {
            let run = async {
                let mut pipe = ClientOptions::new()
                    .open(&instance.endpoint)
                    .map_err(|e| e.to_string())?;
                let request_id = uuid::Uuid::new_v4().to_string();
                let bytes = serde_json::to_vec(&WireRequest {
                    request_id: request_id.clone(),
                    request: request.clone(),
                })
                .map_err(|e| e.to_string())?;
                if bytes.len() > MAX_REQUEST {
                    return Err("UI control request is too large".into());
                }
                pipe.write_all(&(bytes.len() as u32).to_be_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                pipe.write_all(&bytes).await.map_err(|e| e.to_string())?;
                let mut len = [0; 4];
                pipe.read_exact(&mut len).await.map_err(|e| e.to_string())?;
                let len = u32::from_be_bytes(len) as usize;
                if len > MAX_REPLY {
                    return Err("UI control reply is too large".into());
                }
                let mut bytes = vec![0; len];
                pipe.read_exact(&mut bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                let reply: Reply = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                if reply.instance_id != instance.id || reply.request_id != request_id {
                    return Err("UI control instance identity changed".into());
                }
                Ok(reply)
            };
            tokio::time::timeout(WAIT, run)
                .await
                .map_err(|_| "UI control timed out".to_string())?
        })
    }

    pub fn instances() -> Result<Vec<Instance>, String> {
        let dir = directory()?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let entries: Vec<_> = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        for entry in entries {
            let path = entry.path();
            if path.extension().map_or(true, |ext| ext != "json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(instance) = serde_json::from_slice::<Instance>(&bytes) else {
                let _ = fs::remove_file(&path);
                continue;
            };
            if uuid::Uuid::parse_str(&instance.id).is_err()
                || path != dir.join(format!("{}.json", instance.id))
                || instance.endpoint != format!(r"\\.\pipe\thurbox-ui-{}", instance.id)
            {
                let _ = fs::remove_file(&path);
                continue;
            }
            if send(&instance, &Request::Ping).is_ok() {
                out.push(instance);
            } else if ClientOptions::new()
                .open(&instance.endpoint)
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
                && process_exited(instance.pid)
            {
                let _ = fs::remove_file(&path);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

#[cfg(windows)]
pub use windows::{instances, send};
