//! Which server a backend talks to: the socket directory it is found in,
//! on this machine or on a host, and the search that finds a server whose
//! socket file went missing.

use super::*;

/// Where a server was last found, and when it was last searched for in full
/// (`None`: only its socket file was seen, on first use).
#[derive(Clone)]
pub(super) struct LocatedDir {
    dir: SocketDir,
    searched: Option<std::time::Instant>,
    /// What that search concluded, for a throttled question to repeat.
    outcome: Search,
}

/// How long a search for a server stands for the throttled questions — see
/// `Server::relocate`.
pub(super) const RELOCATE_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// The socket directory each server was last found in, by
/// `Server::located_key` — see `Server::socket_dir`.
pub(super) fn located_dirs() -> &'static Mutex<HashMap<String, LocatedDir>> {
    static LOCATED: std::sync::OnceLock<Mutex<HashMap<String, LocatedDir>>> =
        std::sync::OnceLock::new();
    LOCATED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether a socket file for `socket` is in `dir` on this machine. A file,
/// not a server: tmux never unlinks one, so it can outlive its server.
pub(super) fn local_socket_file(dir: &SocketDir, socket: &str) -> bool {
    #[cfg(unix)]
    {
        let base = match dir {
            SocketDir::Dir(dir) => std::path::PathBuf::from(dir),
            SocketDir::Inherited => std::env::var_os("TMUX_TMPDIR")
                .filter(|d| !d.is_empty())
                .map_or_else(
                    || std::path::PathBuf::from("/tmp"),
                    std::path::PathBuf::from,
                ),
            SocketDir::HostData { .. } => return false,
        };
        // SAFETY: `getuid` cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        base.join(format!("tmux-{uid}"))
            .join(socket)
            .symlink_metadata()
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, socket);
        false
    }
}

/// The body of [`locate_script`], after the candidate directories `d0…` and
/// their count `n`. `$1` is the multiplexer, `$2` the socket name.
///
/// It answers with one line, or none when no server was found:
///
/// - `answered <i>` — a server answers in candidate `i`;
/// - `recovered <i> <pid> <others> <path>` — none answered, but `pid` was
///   bound to candidate `i`'s missing socket path, was sent `SIGUSR1`, and now
///   answers (`others` more servers were bound there too);
/// - `unanswered <i> <pid> <others> <path>` — the same, and it has not answered;
/// - `bound <pids> <path>` — a socket file is at `path` and does not answer,
///   while `pids` are bound to it: nothing is signalled. It is given the
///   same three seconds as `SIGUSR1` first, because a peer's `new-session`
///   binds the socket between the first `answers` and `bound` — every
///   relaunch after a reboot searches at once — and a server still starting
///   then reads as one out of reach.
///
/// The bound-socket question is asked of the kernel, because a server's
/// command line need not name its socket: `/proc/net/unix` lists each
/// listening socket by its bound path, the unlinked included, and
/// `/proc/<pid>/fd` says whose it is — one `ls` for every process this user
/// can read. Elsewhere (macOS) `lsof` reports the same bound path. A path is
/// spelled as tmux binds it: the directory resolved, `tmux-<uid>` beneath —
/// and read whole, spaces and all: it is everything after `/proc/net/unix`'s
/// seven fixed columns, and reaches `awk` through the environment, where
/// `-v` would have interpreted its backslashes.
pub(super) const LOCATE_BODY: &str = r#"mux=$1 name=$2 uid=$(id -u)
answers() { TMUX_TMPDIR=$1 "$mux" -L "$name" list-sessions >/dev/null 2>&1; }
answers_within() {
  t=0
  while [ "$t" -lt 30 ] && ! answers "$1"; do sleep 0.1; t=$((t + 1)); done
  answers "$1"
}
sock() {
  if b=$(cd "$1/tmux-$uid" 2>/dev/null && pwd -P); then :
  elif b=$(cd "$1" 2>/dev/null && pwd -P); then b=$b/tmux-$uid
  else b=$1/tmux-$uid; fi
  printf '%s/%s\n' "$b" "$name"
}
bound() {
  if [ -r /proc/net/unix ]; then
    want=$(P=$1 awk '$4 == "00010000" {
      path = $0; for (k = 0; k < 7; k++) sub(/^ *[^ ]+ /, "", path)
      if (path == ENVIRON["P"]) printf " %s ", $7 }' /proc/net/unix)
    [ -n "$want" ] || return 0
    ls -l /proc/[0-9]*/fd 2>/dev/null | awk -v want="$want" '
      /^\/proc\/[0-9]+\/fd:$/ { split($0, a, "/"); pid = a[3]; next }
      / -> socket:\[/ { i = $NF; gsub(/^socket:\[|\]$/, "", i); if (index(want, " " i " ")) print pid }'
  elif command -v lsof >/dev/null 2>&1; then
    lsof -w -a -U -u "$uid" -F pn 2>/dev/null |
      P=$1 awk '/^p/ { pid = substr($0, 2) } /^n/ && substr($0, 2) == ENVIRON["P"] { print pid }'
  fi | sort -un
}
i=0
while [ "$i" -lt "$n" ]; do
  eval "d=\$d$i"
  if answers "$d"; then echo "answered $i"; exit 0; fi
  i=$((i + 1))
done
i=0 seen=' '
while [ "$i" -lt "$n" ]; do
  eval "d=\$d$i"
  p=$(sock "$d")
  case $seen in *" $p "*) i=$((i + 1)); continue ;; esac
  seen="$seen$p "
  pids=$(bound "$p" | tr '\n' ' ')
  if [ -n "$pids" ]; then
    if [ -e "$p" ]; then
      if answers_within "$d"; then echo "answered $i"; exit 0; fi
      echo "bound $(echo $pids | tr ' ' ',') $p"
    else
      set -- $pids
      pid=$1
      shift
      mkdir -p -m 700 "${p%/*}"
      if kill -USR1 "$pid" 2>/dev/null; then
        if answers_within "$d"; then echo "recovered $i $pid $# $p"; else echo "unanswered $i $pid $# $p"; fi
        exit 0
      fi
    fi
  fi
  i=$((i + 1))
done
"#;

/// The script `Server::find_server` runs where the multiplexer does: `candidates`
/// as `d0…`, then [`LOCATE_BODY`].
pub(super) fn locate_script(candidates: &[SocketDir], socket: &str) -> String {
    let mut script = String::new();
    for (i, dir) in candidates.iter().enumerate() {
        script.push_str(&format!("d{i}={}\n", dir.shell_expr(socket)));
    }
    script.push_str(&format!("n={}\n", candidates.len()));
    script.push_str(LOCATE_BODY);
    script
}

/// What [`LOCATE_BODY`] said.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ServerFound {
    Answered(usize),
    Recovered {
        at: usize,
        pid: u32,
        others: usize,
        path: String,
    },
    Unanswered {
        at: usize,
        pid: u32,
        path: String,
    },
    Bound {
        pids: String,
        path: String,
    },
    Nothing,
}

/// Read [`LOCATE_BODY`]'s answer; anything else is [`ServerFound::Nothing`].
pub(super) fn parse_located(answer: &str) -> ServerFound {
    let line = answer.lines().next().unwrap_or("").trim_end();
    let mut words = line.splitn(5, ' ');
    let verb = words.next().unwrap_or("");
    let mut next = || words.next().unwrap_or("");
    match verb {
        "answered" => next()
            .parse()
            .map_or(ServerFound::Nothing, ServerFound::Answered),
        "recovered" | "unanswered" => {
            let (at, pid, others, path) = (next(), next(), next(), next());
            let (Ok(at), Ok(pid), Ok(others)) = (at.parse(), pid.parse(), others.parse()) else {
                return ServerFound::Nothing;
            };
            let path = path.to_string();
            match verb {
                "recovered" => ServerFound::Recovered {
                    at,
                    pid,
                    others,
                    path,
                },
                _ => ServerFound::Unanswered { at, pid, path },
            }
        }
        "bound" => {
            let rest = line.strip_prefix("bound ").unwrap_or("");
            match rest.split_once(' ') {
                Some((pids, path)) => ServerFound::Bound {
                    pids: pids.to_string(),
                    path: path.to_string(),
                },
                None => ServerFound::Nothing,
            }
        }
        _ => ServerFound::Nothing,
    }
}

impl<M: TmuxCompatible> Server<M> {
    /// The transport, carrying the socket directory the server was found in —
    /// what a control-mode client is started over.
    pub(super) fn located_transport(&self) -> TmuxTransport {
        self.host_transport().with_socket_dir(self.socket_dir())
    }

    /// The transport, carrying the host's `PATH` (`path_prepend`, then its
    /// login shells', then the launcher's — [`crate::agent::host_path`]), so
    /// every command thurbox runs there finds the multiplexer where the
    /// new-session flow's probe found it (#1461). Asked per command rather
    /// than at construction: the first ask probes the host, and building a
    /// backend must not wait on one.
    pub(super) fn host_transport(&self) -> TmuxTransport {
        let path = match (&self.host, self.platform) {
            (Some(host), Platform::Posix) => crate::agent::host_path::assignment_for(host),
            _ => None,
        };
        self.transport.clone().with_path(path)
    }

    /// Whether this server's socket directory is thurbox's to choose and to
    /// look in ([`TmuxCompatible::SOCKET_DIR`]) — only on a POSIX machine.
    pub(super) fn manages_socket_dir(&self) -> bool {
        M::SOCKET_DIR && self.platform == Platform::Posix
    }

    /// Where a server started now would put its socket: what the host's own
    /// CLI said its servers use, once it has said, else the directory this
    /// backend was built with.
    pub(super) fn preferred_socket_dir(&self) -> SocketDir {
        let learned = self.host.as_ref().and_then(|host| {
            crate::backend::instance::learned_host_socket_dir(&host.backend_name())
        });
        match learned {
            Some(Some(dir)) => SocketDir::Dir(dir),
            Some(None) => SocketDir::Inherited,
            None => self.transport.socket_dir().clone(),
        }
    }

    /// Every directory a running server for this socket may be in, the
    /// preferred one first: that one, the one this backend was built with
    /// when a host has since said otherwise, and tmux's default — where every
    /// server ran before thurbox chose a directory, and so where one started
    /// by an earlier build still is.
    pub(in crate::backend) fn socket_dir_candidates(&self) -> Vec<SocketDir> {
        let mut dirs = vec![self.preferred_socket_dir()];
        for dir in [self.transport.socket_dir().clone(), SocketDir::Inherited] {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    /// The machine and socket a located directory is remembered under: one
    /// answer per server, shared by every backend this process builds for it.
    pub(super) fn located_key(&self) -> String {
        let machine = self
            .host
            .as_ref()
            .map_or_else(|| "local".to_string(), HostDef::backend_name);
        format!("{machine}\0{}", self.socket())
    }

    /// The directory this server's socket is in. Found once per process
    /// ([`Self::socket_file_seen`], else [`Self::find_server`]) before the
    /// first command, and again whenever a
    /// command finds no server ([`Self::relocate`]); remembered in between.
    pub(in crate::backend) fn socket_dir(&self) -> SocketDir {
        if !self.manages_socket_dir() {
            return SocketDir::Inherited;
        }
        let key = self.located_key();
        if let Some(found) = located_dirs()
            .lock()
            .ok()
            .and_then(|m| m.get(&key).cloned())
        {
            return found.dir;
        }
        let (dir, searched, outcome) = match self.socket_file_seen() {
            Some(dir) => (dir, None, Search::Found),
            None => {
                let (dir, outcome) = self.find_server();
                (dir, Some(std::time::Instant::now()), outcome)
            }
        };
        if let Ok(mut map) = located_dirs().lock() {
            map.entry(key).or_insert(LocatedDir {
                dir: dir.clone(),
                searched,
                outcome,
            });
        }
        dir
    }

    /// Look for this socket's server again, after a command found none:
    /// [`Search::Found`] when one was — answering in a directory, or brought
    /// back by `SIGUSR1` — so the caller asks again rather than starting a
    /// second, and [`Search::Stuck`] when one runs out of reach.
    ///
    /// `throttled` is for the questions asked on a timer — a listing, a
    /// reconnect — which on a host with no server would otherwise pay a
    /// search per pass: those repeat the last search's verdict within
    /// [`RELOCATE_EVERY`] of it, a server out of reach included. What is about
    /// to start a server always searches.
    pub(super) fn relocate(&self, throttled: bool) -> Search {
        if !self.manages_socket_dir() {
            return Search::Absent;
        }
        let key = self.located_key();
        if throttled {
            let recent = located_dirs().lock().ok().and_then(|m| {
                m.get(&key)
                    .filter(|found| {
                        found
                            .searched
                            .is_some_and(|at| at.elapsed() < RELOCATE_EVERY)
                    })
                    .map(|found| found.outcome.clone())
            });
            if let Some(outcome) = recent {
                return match outcome {
                    Search::Stuck(why) => Search::Stuck(why),
                    Search::Found | Search::Absent => Search::Absent,
                };
            }
        }
        let (dir, outcome) = self.find_server();
        if let Ok(mut map) = located_dirs().lock() {
            map.insert(
                key,
                LocatedDir {
                    dir,
                    searched: Some(std::time::Instant::now()),
                    outcome: outcome.clone(),
                },
            );
        }
        outcome
    }

    /// The error a server out of reach ([`Search::Stuck`]) is reported as.
    pub(super) fn out_of_reach(&self, why: &str) -> anyhow::Error {
        crate::backend::contract::BackendError::Unreachable(format!(
            "{}: {why}; not starting a second server beside it \
             (see 'An orphaned tmux server' in docs/CONFIG.md)",
            self.name
        ))
        .into()
    }

    /// After the session probe found nothing and before anything starts a
    /// server: whether looking for it again ([`Self::relocate`]) brought the
    /// session back. Says at INFO which way it went when it did not — into a
    /// running server, or a new one — and fails when a server runs out of
    /// reach ([`Search::Stuck`]), which nothing may start a second beside.
    pub(super) fn found_after_all(&self) -> Result<bool> {
        if !self.manages_socket_dir() {
            return Ok(false);
        }
        let found = match self.relocate(false) {
            Search::Stuck(why) => return Err(self.out_of_reach(&why)),
            Search::Found => true,
            Search::Absent => false,
        };
        if found && self.session_exists() {
            return Ok(true);
        }
        let mux = self.transport.mux();
        let socket = self.socket();
        match found {
            true => info!(
                "{}: the {mux} server for socket '{socket}' has no session '{}'; creating it there",
                self.name, self.session
            ),
            false => info!(
                "{}: no {mux} server is running for socket '{socket}'; starting one in {}",
                self.name,
                self.socket_dir().describe()
            ),
        }
        Ok(false)
    }

    /// On first use, a local socket file at the preferred path is taken as
    /// the answer, so the common start costs no process; a stale file fails
    /// the first command, which then asks again in full.
    pub(super) fn socket_file_seen(&self) -> Option<SocketDir> {
        let preferred = self.preferred_socket_dir();
        (!self.transport.is_remote() && local_socket_file(&preferred, &self.socket()))
            .then_some(preferred)
    }

    /// Find the running server for this socket: the directory to use, and
    /// whether a server is there.
    ///
    /// A server whose socket file is gone keeps running — macOS sweeps
    /// `/tmp`, and a socket's access time is not bumped by `connect()` — and
    /// nothing reaches it. Starting another on the same name is how one Mac
    /// came to hold 93 processes and 3.1 GB of agents nobody could see, 13 of
    /// them duplicates of sessions resumed into the new server. So before
    /// anything concludes there is no server, [`LOCATE_SCRIPT`] asks on the
    /// machine itself: each candidate directory ([`Self::socket_dir_candidates`])
    /// in turn, then for a live server still bound to a candidate's missing
    /// socket path, which it sends `SIGUSR1` — tmux's documented way to
    /// recreate a removed socket. A path where a socket file exists is never
    /// signalled: tmux unlinks the path before binding, and would orphan
    /// whichever server owns it now.
    ///
    pub(super) fn find_server(&self) -> (SocketDir, Search) {
        let candidates = self.socket_dir_candidates();
        let preferred = candidates[0].clone();
        let socket = self.socket();
        let script = locate_script(&candidates, &socket);
        let output = self
            .host_transport()
            .shell_command(&script, &["sh", self.transport.mux(), &socket])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        let output = match output {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                debug!(
                    "{}: could not look for the server of socket '{socket}': {}",
                    self.name,
                    mux_failure(&output, self.transport.is_ssh())
                );
                return (preferred, Search::Absent);
            }
            Err(e) => {
                debug!(
                    "{}: could not look for the server of socket '{socket}': {e}",
                    self.name
                );
                return (preferred, Search::Absent);
            }
        };
        let answer = String::from_utf8_lossy(&output.stdout);
        match parse_located(&answer) {
            ServerFound::Answered(at) => {
                let dir = candidates[at].clone();
                if at != 0 {
                    info!(
                        "{}: adopting the running {} server for socket '{socket}' in {} \
                         rather than starting one in {}; it moves there once this one exits",
                        self.name,
                        self.transport.mux(),
                        dir.describe(),
                        preferred.describe()
                    );
                }
                (dir, Search::Found)
            }
            ServerFound::Recovered {
                at,
                pid,
                others,
                path,
            } => {
                info!(
                    "{}: the {} server for socket '{socket}' (pid {pid}) had lost its \
                     socket {path}; recreated it with SIGUSR1 and reattached",
                    self.name,
                    self.transport.mux()
                );
                if others > 0 {
                    warn!(
                        "{}: {others} more server(s) are bound to {path} with no way back in; \
                         they are orphaned, and killing them is the only remedy",
                        self.name
                    );
                }
                (candidates[at].clone(), Search::Found)
            }
            ServerFound::Unanswered { at, pid, path } => {
                let why = format!(
                    "sent SIGUSR1 to the {} server for socket '{socket}' (pid {pid}) to \
                     recreate {path}, and it has not answered since",
                    self.transport.mux()
                );
                warn!("{}: {why}", self.name);
                (candidates[at].clone(), Search::Stuck(why))
            }
            ServerFound::Bound { pids, path } => {
                let why = format!(
                    "{path} exists but does not answer, and server(s) {pids} are still \
                     bound to it; not signalling them, which would orphan whichever owns it"
                );
                warn!("{}: {why}", self.name);
                (preferred, Search::Stuck(why))
            }
            ServerFound::Nothing => (preferred, Search::Absent),
        }
    }

    /// Refuse to act on a host whose socket is a guess (see
    /// [`known_host_socket`]). Always fine locally.
    pub(super) fn known_socket(&self) -> Result<()> {
        match &self.host {
            Some(host) => known_host_socket(host).map(drop),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_locate_script_says_is_read_word_for_word() {
        assert_eq!(parse_located("answered 1\n"), ServerFound::Answered(1));
        assert_eq!(
            parse_located("recovered 0 4242 1 /private/tmp/tmux-501/a b\n"),
            ServerFound::Recovered {
                at: 0,
                pid: 4242,
                others: 1,
                path: "/private/tmp/tmux-501/a b".into()
            }
        );
        assert_eq!(
            parse_located("unanswered 1 7 0 /tmp/tmux-0/thurbox"),
            ServerFound::Unanswered {
                at: 1,
                pid: 7,
                path: "/tmp/tmux-0/thurbox".into()
            }
        );
        assert_eq!(
            parse_located("bound 7,9 /tmp/tmux-0/thurbox"),
            ServerFound::Bound {
                pids: "7,9".into(),
                path: "/tmp/tmux-0/thurbox".into()
            }
        );
        for nothing in ["", "answered", "answered x", "recovered 0 p 0 /x", "hello"] {
            assert_eq!(parse_located(nothing), ServerFound::Nothing, "{nothing:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_locate_script_is_valid_sh_and_names_every_candidate_in_order() {
        let script = locate_script(
            &[
                SocketDir::Dir("/home/me/.local/share/thurbox/tmux".into()),
                SocketDir::Inherited,
                SocketDir::HostData {
                    app: "thurbox".into(),
                },
            ],
            "thurbox",
        );
        assert!(script.starts_with(
            "d0=/home/me/.local/share/thurbox/tmux\nd1=${TMUX_TMPDIR:-/tmp}\nd2=$(d="
        ));
        assert!(script.contains("\nn=3\n"));
        let parsed = std::process::Command::new("sh")
            .args(["-n", "-c", &script])
            .output()
            .expect("sh");
        assert!(
            parsed.status.success(),
            "{}",
            String::from_utf8_lossy(&parsed.stderr)
        );
    }

    /// Runs [`LOCATE_BODY`] against one candidate directory holding a
    /// listening socket bound by this process, with a stand-in multiplexer
    /// whose `list-sessions` succeeds once `ready` exists.
    #[cfg(target_os = "linux")]
    fn locate_beside_a_bound_socket(answer_after: Option<std::time::Duration>) -> ServerFound {
        let root = tempfile::tempdir().expect("tempdir");
        // SAFETY: `getuid` cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let sockets = root.path().join(format!("tmux-{uid}"));
        std::fs::create_dir_all(&sockets).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(sockets.join("located")).unwrap();
        let ready = root.path().join("ready");
        let mux = root.path().join("mux");
        std::fs::write(&mux, format!("#!/bin/sh\ntest -e '{}'\n", ready.display())).unwrap();
        std::fs::set_permissions(&mux, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let script = locate_script(
            &[SocketDir::Dir(root.path().display().to_string())],
            "located",
        );
        let starting = answer_after.map(|delay| {
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                std::fs::write(ready, "").unwrap();
            })
        });
        let out = std::process::Command::new("sh")
            .args(["-c", &script, "sh"])
            .arg(&mux)
            .arg("located")
            .output()
            .expect("sh");
        if let Some(starting) = starting {
            starting.join().unwrap();
        }
        parse_located(&String::from_utf8_lossy(&out.stdout))
    }

    /// The race behind a cold start's `not starting a second server beside
    /// it`: every relaunch after a reboot looks for the server at once, and a
    /// peer's `new-session` binds the socket between this search asking it and
    /// looking for who is bound to it. A server still starting is not one out
    /// of reach.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_server_that_binds_while_the_search_runs_is_found_not_stuck() {
        assert_eq!(
            locate_beside_a_bound_socket(Some(std::time::Duration::from_millis(300))),
            ServerFound::Answered(0)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_bound_socket_that_never_answers_is_still_refused() {
        assert!(matches!(
            locate_beside_a_bound_socket(None),
            ServerFound::Bound { .. }
        ));
    }
}
