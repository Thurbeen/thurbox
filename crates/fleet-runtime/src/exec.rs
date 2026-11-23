//! The `exec` runtime kind: any program that speaks the runtime protocol
//! (docs/runtime/README.md), run as `<program> <verb>` through `fleet-exec`,
//! one bounded process per call, a JSON request on stdin and
//! `{"known": …}` or `{"unknown": {"why": …}}` on stdout.
//!
//! A host names its runtime in `fleet.toml`, so a Task's `spec.host` chooses
//! the program. The adapter knows no runtime: what it sends and reads is the
//! protocol, in serde types of its own. It shares none with any runtime's code;
//! [`check`] and the end-to-end tests that run `runtime-mux` through this
//! adapter are what hold the two to one protocol.
//!
//! Every failure to get a protocol answer (a program that is not there, a
//! timeout, a non-zero exit, an answer that does not parse) is Unknown, and
//! Unknown never concludes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleet_api::Observation;
use fleet_api::event::{OutboxRead, outbox_file};
use fleet_api::harness::{
    claude_trust_dialog, codex_initial_composer, codex_process, codex_trust_dialog,
    copilot_initial_composer, copilot_process, copilot_trust_dialog, kimi_initial_composer,
    kimi_process, profile,
};
use fleet_api::plan::{EffectOutcome, SpawnSpec};
use fleet_api::runtime::{Attach, Place, PlaceReport, Runtime, TrustGap, WorkspaceTrust};
use fleet_api::session::uid_of_session_key;
use fleet_api::signal::{Reported, Signal, Signals, State};
use fleet_api::task::{LOCAL_HOST, SessionRef, Workspace, url_scheme};
use fleet_api::workspace::WorkspaceRef;
use fleet_exec::{Outcome, Spec};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::signals::{Boundary, FileSignals};

/// The newest protocol version Fleet speaks. Every request carries the
/// version the runtime's manifest named, and a runtime that speaks another
/// answers Unknown.
pub const PROTOCOL: u64 = 2;

/// Every version Fleet speaks. Version 2 names a place on each request.
pub const PROTOCOLS: &[u64] = &[1, 2];

/// The verbs every runtime answers.
pub const REQUIRED_VERBS: &[&str] = &["manifest", "start", "inspect", "stop"];

/// The most of a runtime's stdout Fleet reads: a `capture` or an `exec` answer
/// is the largest.
const ANSWER_CAP: usize = 4 * 1024 * 1024;

/// The calls one port operation can make: `spawn` is a `start`, and nothing
/// makes more than three. A call to a runtime whose version this process
/// does not know yet is a `manifest` first; the operations that already ask
/// the manifest learn it there. A restart on another host is the most: the
/// manifest, the signal file's length through `exec`, then `start`.
const MAX_CALLS_PER_OPERATION: u32 = 3;

/// How long one capture answers the screen probe: a session's screen rules
/// are for a view, so a screen this recent is current enough, and a sweep
/// every second does not run a capture per session every second.
pub const SCREEN_REUSE: Duration = Duration::from_secs(2);

/// How long one `inspect` answer serves the probes of one reconcile, which
/// ask it of one key one after the other.
pub const INSPECT_REUSE: Duration = Duration::from_millis(500);

/// What Fleet runs through a runtime's `exec` to read a session's signal
/// file on another host: the `fleet` on that host, which `fleet signal`
/// needs there anyway. One program on every platform, so a native-Windows
/// place needs no `sh`, and it reads by seeking, never the whole file
/// ([`crate::signals::read_split`]).
const PLACE_READ: [&str; 2] = ["fleet", "signal-read"];

/// What a restart learns of the length of the session's signal file on
/// another host.
enum Length {
    At(u64),
    /// The runtime cannot read it there: it does not offer `exec`, or answers
    /// that it cannot run one at the place.
    Unreadable,
    /// The read may answer later: why it did not now.
    Unknown(String),
}

/// The bound on a `workspace` call when `[runtimes.<name>]` names none:
/// a first clone of a repository can take minutes.
pub const DEFAULT_WORKSPACE_TIMEOUT: Duration = Duration::from_secs(600);

/// One program that speaks the protocol, as `[runtimes.<name>]` gives it.
#[derive(Debug, Clone)]
pub struct ExecProgram {
    /// The runtime's name in the config, for messages.
    pub name: String,
    /// The executable, a path or a name on PATH.
    pub program: String,
    /// Arguments before the verb.
    pub args: Vec<String>,
    /// The bound on every call but `workspace`.
    pub call_timeout: Duration,
    /// The bound on a `workspace` call, which may clone a repository.
    pub workspace_timeout: Duration,
    /// The first part of every key this Fleet gives the runtime: a second
    /// Fleet on the same runtime takes its own.
    pub key_prefix: String,
    /// The place each request names (ADR-42): `local` unless the Task names
    /// another of the runtime's places.
    pub place: Place,
    /// Every place of the runtime the config names, which `manifest` asks
    /// about.
    pub places: Vec<Place>,
    /// The protocol version the runtime's last manifest named, when Fleet
    /// speaks it: what each request carries.
    spoken: Arc<Mutex<Option<u64>>>,
}

impl ExecProgram {
    pub fn new(name: impl Into<String>, program: impl Into<String>) -> ExecProgram {
        ExecProgram {
            name: name.into(),
            program: program.into(),
            args: Vec::new(),
            call_timeout: Duration::from_secs(15),
            workspace_timeout: DEFAULT_WORKSPACE_TIMEOUT,
            key_prefix: "fleet".into(),
            place: Place::local(),
            places: vec![Place::local()],
            spoken: Arc::default(),
        }
    }

    /// The version this runtime speaks, as its last manifest named it.
    pub fn spoken(&self) -> Option<u64> {
        *self
            .spoken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn learn(&self, protocol: Option<u64>) {
        *self
            .spoken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = protocol;
    }

    /// The runtime's `manifest`, asked about each of its places. A runtime
    /// of either version answers it, and the version it names is what later
    /// requests carry.
    pub fn manifest(&self) -> Observation<Manifest> {
        let request = json!({ "protocol": PROTOCOL, "places": self.places });
        let manifest = self.ask::<Manifest>("manifest", request);
        // An Unknown manifest proves no version: the next request asks again.
        let protocol = match &manifest {
            Observation::Known(m) => PROTOCOLS.contains(&m.protocol).then_some(m.protocol),
            Observation::Unknown { .. } => None,
        };
        self.learn(protocol);
        manifest
    }

    /// `fields` as a request of `protocol`: version 2 names the place.
    fn request(&self, protocol: u64, fields: Value) -> Value {
        let mut request = json!({ "protocol": protocol });
        if protocol >= 2 {
            request["place"] = json!(self.place);
        }
        if let (Some(r), Value::Object(f)) = (request.as_object_mut(), fields) {
            r.extend(f);
        }
        request
    }

    /// Why a runtime of `protocol` cannot serve `place`, if it cannot:
    /// version 1 names no place, so it serves only `local`, with no config.
    pub fn unplaced(&self, protocol: u64, place: &Place) -> Option<String> {
        (protocol < 2 && (place.name != Place::LOCAL || !place.config.is_empty())).then(|| {
            format!(
                "runtime {} speaks protocol 1, which has no places: it serves only {:?}, with no config, not {:?}",
                self.name,
                Place::LOCAL,
                place.name
            )
        })
    }

    /// The version to speak: the one learned, or the manifest's.
    fn protocol(&self) -> Observation<u64> {
        if let Some(protocol) = self.spoken() {
            return match self.unplaced(protocol, &self.place) {
                Some(why) => Observation::unknown(why),
                None => Observation::Known(protocol),
            };
        }
        bind(self.manifest(), |m| {
            if let Some(why) = self.unplaced(m.protocol, &self.place) {
                Observation::unknown(why)
            } else if PROTOCOLS.contains(&m.protocol) {
                Observation::Known(m.protocol)
            } else {
                Observation::unknown(format!(
                    "runtime {} speaks protocol {}; Fleet speaks {PROTOCOLS:?}",
                    self.name, m.protocol
                ))
            }
        })
    }

    /// `verb` with `fields`, in the version the runtime speaks. An Unknown
    /// answer forgets the version, so a runtime replaced by one of another
    /// version is asked its manifest again.
    fn send<T: DeserializeOwned>(&self, verb: &str, fields: Value) -> Observation<T> {
        let answer = bind(self.protocol(), |protocol| {
            self.ask(verb, self.request(protocol, fields))
        });
        if answer.is_unknown() {
            self.learn(None);
        }
        answer
    }

    /// The answer to one call, or why there is none.
    pub fn call(&self, verb: &str, request: &Value) -> Answer {
        self.call_raw(verb, request.to_string().into_bytes())
    }

    /// The bound on one call of `verb`.
    fn timeout(&self, verb: &str) -> Duration {
        if verb == "workspace" {
            self.workspace_timeout
        } else {
            self.call_timeout
        }
    }

    fn call_raw(&self, verb: &str, request: Vec<u8>) -> Answer {
        let call = crate::telemetry::Call::start(&self.name, verb);
        let answer = self.answer(verb, request);
        call.end(matches!(answer, Answer::Known(_)));
        answer
    }

    fn answer(&self, verb: &str, request: Vec<u8>) -> Answer {
        let mut spec = Spec::new(&self.program, self.timeout(verb))
            .args(&self.args)
            .arg(verb)
            .stdin(request);
        spec.output_limit = Some(ANSWER_CAP);
        crate::telemetry::carry(&mut spec);
        let out = fleet_exec::run(&spec);
        let Outcome::Exited {
            code: Some(0),
            stdout,
            ..
        } = &out
        else {
            return Answer::Unknown(format!(
                "runtime {} ({}) {verb}: {}",
                self.name,
                self.program,
                out.failure()
            ));
        };
        match serde_json::from_slice::<Wire>(stdout) {
            Ok(Wire::Known(value)) => Answer::Known(value),
            Ok(Wire::Unknown { why }) => {
                Answer::Unknown(format!("runtime {} {verb}: {why}", self.name))
            }
            Err(e) => Answer::Unknown(format!(
                "runtime {} {verb} answered outside the protocol: {e}",
                self.name
            )),
        }
    }

    /// `browse` at this program's place: the subdirectories of `dir`, the
    /// place's home when `None`. Unknown when the runtime's manifest does
    /// not offer the verb, which is then not asked.
    pub fn browse(&self, dir: Option<&str>) -> Observation<Browsed> {
        bind(self.offers("browse"), |()| {
            let fields = match dir {
                Some(dir) => json!({ "dir": dir }),
                None => json!({}),
            };
            self.send("browse", fields)
        })
    }

    /// `branches` at this program's place: the branches of the repository
    /// at `repo`, and its default. Unknown when the runtime's manifest does
    /// not offer the verb, which is then not asked.
    pub fn branches(&self, repo: &str) -> Observation<BranchesRead> {
        bind(self.offers("branches"), |()| {
            self.send("branches", json!({ "repo": repo }))
        })
    }

    /// Whether the runtime's manifest offers the optional `verb`.
    fn offers(&self, verb: &str) -> Observation<()> {
        bind(self.manifest(), |m| {
            if m.verbs.iter().any(|v| v == verb) {
                Observation::Known(())
            } else {
                Observation::unknown(format!("runtime {} does not offer {verb}", self.name))
            }
        })
    }

    /// A known answer of verb's shape `T`, or Unknown.
    fn ask<T: DeserializeOwned>(&self, verb: &str, request: Value) -> Observation<T> {
        match self.call(verb, &request) {
            Answer::Known(value) => serde_json::from_value(value).map_or_else(
                |e| {
                    Observation::unknown(format!(
                        "runtime {} {verb} answered outside the protocol: {e}",
                        self.name
                    ))
                },
                Observation::Known,
            ),
            Answer::Unknown(why) => Observation::unknown(why),
        }
    }

    fn key(&self, fleet_key: &str) -> String {
        format!("{}.{}", self.key_prefix, uid_of_session_key(fleet_key))
    }
}

fn now() -> fleet_api::Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `f` on a Known value; Unknown stays Unknown.
fn bind<T, U>(o: Observation<T>, f: impl FnOnce(T) -> Observation<U>) -> Observation<U> {
    match o {
        Observation::Known(v) => f(v),
        Observation::Unknown { why } => Observation::Unknown { why },
    }
}

/// One call's result: the `known` value, or why it is unknown.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Known(Value),
    Unknown(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
enum Wire {
    Known(Value),
    Unknown { why: String },
}

/// `manifest`'s answer.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub protocol: u64,
    /// The runtime's own release, when it reports one.
    #[serde(default)]
    pub version: Option<String>,
    pub verbs: Vec<String>,
    /// What a runtime that declares the `workspace` verb makes.
    #[serde(default)]
    pub workspace: Option<WorkspaceManifest>,
    /// Each place the request asked about, from protocol version 2.
    #[serde(default)]
    pub places: Vec<PlaceReport>,
}

/// A `browse` answer (docs/runtime/README.md, "Browse and branches").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Browsed {
    /// The subdirectories of `dir`, by name.
    Listing {
        dir: String,
        entries: Vec<BrowseEntry>,
    },
    /// The runtime could not read the directory.
    Failed { why: String },
}

/// One subdirectory a `browse` lists: `repo` when it is the top of a git
/// work tree.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct BrowseEntry {
    pub name: String,
    pub repo: bool,
}

/// A `branches` answer (docs/runtime/README.md, "Browse and branches").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BranchesRead {
    /// Each local and `origin/*` branch, and the one a new branch starts
    /// from by default.
    Branches {
        names: Vec<String>,
        default: Option<String>,
    },
    /// The runtime could not read the repository.
    Failed { why: String },
}

/// The manifest's `workspace`: the URL schemes the runtime handles, and
/// whether the files outlive the process.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceManifest {
    #[serde(default)]
    pub schemes: Vec<String>,
    #[serde(default)]
    pub survives_stop: bool,
}

impl Manifest {
    /// The first of `urls` this runtime does not make a directory for: it
    /// lists no `workspace` verb, or not the URL's scheme.
    pub fn unsupported<'a>(&self, urls: &'a [String]) -> Option<&'a String> {
        let schemes = match (&self.workspace, self.verbs.iter().any(|v| v == "workspace")) {
            (Some(w), true) => &w.schemes,
            _ => return urls.first(),
        };
        urls.iter().find(|url| {
            url_scheme(url)
                .is_none_or(|s| !schemes.iter().any(|have| have.eq_ignore_ascii_case(&s)))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Started {
    Started { id: String, adopted: bool },
    Failed { why: String },
}

/// `inspect`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Inspected {
    None,
    Running {
        id: String,
    },
    Exited {
        id: String,
        code: Option<i64>,
    },
    /// Stopped by `pause`, the key kept.
    Paused,
}

/// `pause`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PauseAnswer {
    Paused,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stopped {
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Sent {
    Sent,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Executed {
    Exited {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    TimedOut {
        stdout: String,
        stderr: String,
    },
    Failed {
        why: String,
    },
}

/// `attach`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Attached {
    Stream { argv: Vec<String> },
    Tty { argv: Vec<String> },
    None,
}

/// `capture`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Captured {
    #[serde(rename_all = "camelCase")]
    Screen {
        text: String,
        cursor_row: Option<u64>,
        cursor_col: Option<u64>,
        foreground: Option<String>,
        /// The title the program set; a runtime that predates it omits it.
        #[serde(default)]
        title: Option<String>,
    },
    None,
}

/// What an agent's screen shows before its first report.
enum FirstScreen {
    /// A workspace-trust dialog, which only a person answers.
    TrustDialog,
    /// Codex's empty composer: it takes input, and reports nothing until
    /// its first turn ends.
    CodexComposer,
    /// Kimi Code's empty composer: it takes input, and reports nothing
    /// until its first message starts its session.
    KimiComposer,
    /// GitHub Copilot CLI's empty composer: it takes input, and reports
    /// nothing until its first message starts its session.
    CopilotComposer,
    Other,
}

/// The `exec` kind: one program per host.
#[derive(Debug, Clone, Default)]
pub struct ExecRuntime {
    hosts: BTreeMap<String, ExecProgram>,
    signals: Option<FileSignals>,
    /// The last screen of each session, by session id, with when it was
    /// captured.
    screens: Arc<Mutex<BTreeMap<String, (Instant, CapturedScreen)>>>,
    inspected: Arc<Mutex<Option<LastInspect>>>,
}

type LastInspect = ((String, String), Instant, Observation<Inspected>);

type CapturedScreen = Observation<Option<fleet_api::screen::Screen>>;

impl ExecRuntime {
    /// A verb answering how to open a stream on the key, `attach` or
    /// `shell`, asked only when the runtime's manifest declares it.
    fn opener(&self, verb: &str, session: &SessionRef) -> Observation<Option<Attach>> {
        let rt = match self.program(&session.host) {
            Ok(rt) => rt,
            Err(why) => return Observation::unknown(why),
        };
        bind(rt.manifest(), |manifest| {
            if !manifest.verbs.iter().any(|v| v == verb) {
                return Observation::Known(None);
            }
            let request = json!({ "key": rt.key(&session.key) });
            rt.send::<Attached>(verb, request).map(|a| match a {
                Attached::Stream { argv } => Some(Attach::Stream { argv }),
                Attached::Tty { argv } => Some(Attach::Tty { argv }),
                Attached::None => None,
            })
        })
    }

    /// The program serving `host` when its manifest declares `pause`, else
    /// why not.
    fn pausing(&self, host: &str) -> Observation<Result<&ExecProgram, String>> {
        let rt = match self.program(host) {
            Ok(rt) => rt,
            Err(why) => return Observation::Known(Err(why)),
        };
        rt.manifest().map(|m| {
            if m.verbs.iter().any(|v| v == "pause") {
                Ok(rt)
            } else {
                Err(format!("runtime {} declares no pause", rt.name))
            }
        })
    }

    /// `hosts` maps each host name a Task may give to the program serving it.
    pub fn new(hosts: BTreeMap<String, ExecProgram>) -> ExecRuntime {
        ExecRuntime {
            hosts,
            signals: None,
            screens: Arc::default(),
            inspected: Arc::default(),
        }
    }

    /// Read what each agent reports through `fleet signal` from `signals`:
    /// readiness, workspace trust and rest come from it, as no runtime
    /// knows the agent.
    pub fn with_signals(mut self, signals: FileSignals) -> ExecRuntime {
        self.signals = Some(signals);
        self
    }

    fn program(&self, host: &str) -> Result<&ExecProgram, String> {
        self.hosts
            .get(host)
            .ok_or_else(|| {
                format!(
                    "no runtime has the place {host:?}: name `<runtime>/<place>` from [runtimes.<name>.places.<place>], or `local`"
                )
            })
    }

    fn inspect(&self, host: &str, fleet_key: &str) -> Observation<Inspected> {
        let rt = match self.program(host) {
            Ok(rt) => rt,
            Err(why) => return Observation::unknown(why),
        };
        let asked = (host.to_string(), fleet_key.to_string());
        let last = || {
            self.inspected
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        if let Some((key, at, found)) = last().as_ref()
            && *key == asked
            && at.elapsed() < INSPECT_REUSE
        {
            return found.clone();
        }
        let found: Observation<Inspected> = rt.send("inspect", json!({ "key": rt.key(fleet_key) }));
        *last() = Some((asked, Instant::now(), found.clone()));
        found
    }

    fn forget_inspected(&self) {
        *self
            .inspected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn exec_on(&self, host: &str, argv: &[&str]) -> Observation<Executed> {
        let rt = match self.program(host) {
            Ok(rt) => rt,
            Err(why) => return Observation::unknown(why),
        };
        let timeout = u64::try_from(rt.call_timeout.as_millis()).unwrap_or(u64::MAX) / 2;
        rt.send(
            "exec",
            json!({ "in": "host", "argv": argv, "timeoutMs": timeout.max(1) }),
        )
    }

    /// `start` of `argv` with `spec`'s environment and directory, under its
    /// key: a spawn, a revive and a resume are each one.
    fn start(&self, spec: &SpawnSpec, argv: &[String]) -> Observation<Started> {
        let rt = match self.program(&spec.host) {
            Ok(rt) => rt,
            Err(why) => return Observation::Known(Started::Failed { why }),
        };
        if spec.model.is_some() {
            return Observation::Known(Started::Failed {
                why: format!(
                    "runtime {} passes no model to its harness; drop spec.model",
                    rt.name
                ),
            });
        }
        // A non-empty workspace is made before the first start, and the
        // session runs in its first directory; an empty one runs in the
        // runtime's default directory.
        let dir = spec.ready.as_ref().and_then(WorkspaceRef::dir);
        if dir.is_none() && !spec.workspace.is_empty() {
            return Observation::Known(Started::Failed {
                why: format!(
                    "runtime {}: the workspace is not made yet; Fleet makes it before the first start",
                    rt.name
                ),
            });
        }
        let mut request = json!({
            "key": rt.key(&spec.key),
            "argv": argv,
            "env": spec.env,
        });
        if let Some(dir) = dir {
            request["dir"] = json!(dir);
        }
        self.forget_inspected();
        rt.send("start", request)
    }

    /// Start the agent of `session` again on the harness profile's resume
    /// argv: a process still running is adopted, so nothing restarts it.
    fn restart(
        &self,
        session: &SessionRef,
        conversation: Option<&str>,
        launch: &SpawnSpec,
        done: EffectOutcome,
    ) -> Observation<EffectOutcome> {
        let mut spec = launch.clone();
        spec.host.clone_from(&session.host);
        spec.key.clone_from(&session.key);
        let argv = match spec.launch_argv() {
            Ok(argv) => profile(&spec.harness).resume(&argv, conversation),
            Err(why) => return Observation::Known(EffectOutcome::Failed { why }),
        };
        // What the previous process reported is not the new one's: its
        // readiness and trust wait for the new one's own report.
        if let Some(file) = self
            .signals
            .as_ref()
            .and_then(|s| s.file(&spec.host, &spec.key))
        {
            let mark = Signal {
                at: now(),
                state: State::Started,
                prompts: Vec::new(),
                conversation: None,
                seq: None,
            };
            // On another host the file is the place's: the restart is kept
            // in Fleet's own data, as the file's length then, and a start
            // never waits on writing there.
            let marked = match &self.signals {
                Some(signals) if spec.host != LOCAL_HOST => {
                    let boundary = match self.place_file_length(&spec.host, &file) {
                        Length::At(length) => Boundary::At(length),
                        // A file Fleet cannot read there is never read:
                        // the session's state stays Unknown.
                        Length::Unreadable => Boundary::Unknown,
                        // A read that may answer on the next pass: start
                        // nothing until it does, so no state the previous
                        // process reported passes for the new one's.
                        Length::Unknown(why) => {
                            return Observation::unknown(format!(
                                "cannot read the length of the signal file on {}, which marks the restart: {why}",
                                spec.host
                            ));
                        }
                    };
                    signals
                        .mark_started(&spec.key, boundary)
                        .map_err(|e| e.to_string())
                }
                _ => crate::signals::append(std::path::Path::new(&file), &mark)
                    .map_err(|e| e.to_string()),
            };
            if let Err(e) = marked {
                return Observation::unknown(format!(
                    "cannot mark the restart in the signal file: {e}"
                ));
            }
        }
        self.start(&spec, &argv).map(|started| match started {
            Started::Started { .. } => done,
            Started::Failed { why } => EffectOutcome::Failed { why },
        })
    }

    /// The session's visible screen, through `capture`.
    fn capture(&self, session: &SessionRef) -> Observation<Captured> {
        let rt = match self.program(&session.host) {
            Ok(rt) => rt,
            Err(why) => return Observation::unknown(why),
        };
        rt.send::<Captured>(
            "capture",
            json!({ "key": rt.key(&session.key), "lines": 0 }),
        )
    }

    /// What the agent's screen shows before its first report.
    fn first_screen(&self, session: &SessionRef) -> Observation<FirstScreen> {
        self.capture(session).map(|captured| {
            let Captured::Screen {
                text,
                cursor_row,
                cursor_col,
                foreground,
                ..
            } = captured
            else {
                return FirstScreen::Other;
            };
            let at = |n: Option<u64>| n.and_then(|n| usize::try_from(n).ok());
            let (row, col) = (at(cursor_row), at(cursor_col));
            if codex_trust_dialog(&text)
                || copilot_trust_dialog(&text)
                || row.is_some_and(|r| claude_trust_dialog(&text, r))
            {
                FirstScreen::TrustDialog
            } else if foreground.as_deref().is_some_and(codex_process)
                && row
                    .zip(col)
                    .is_some_and(|(r, c)| codex_initial_composer(&text, r, c))
            {
                FirstScreen::CodexComposer
            } else if foreground.as_deref().is_some_and(kimi_process)
                && row
                    .zip(col)
                    .is_some_and(|(r, c)| kimi_initial_composer(&text, r, c))
            {
                FirstScreen::KimiComposer
            } else if foreground.as_deref().is_some_and(copilot_process)
                && row
                    .zip(col)
                    .is_some_and(|(r, c)| copilot_initial_composer(&text, r, c))
            {
                FirstScreen::CopilotComposer
            } else {
                FirstScreen::Other
            }
        })
    }

    /// What the agent's hooks reported through `fleet signal`: the file on
    /// this machine, or on the session's host through the runtime's `exec`.
    fn reported(&self, session: &SessionRef) -> Observation<Reported> {
        let Some(signals) = &self.signals else {
            return Observation::unknown("no signal port: what the agent reports is not read");
        };
        if session.host == LOCAL_HOST {
            return signals.read(session);
        }
        let Some(file) = signals.file(&session.host, &session.key) else {
            return Observation::unknown("the session key is not a plain name");
        };
        let since = match signals.started(&session.key) {
            Some(Boundary::Unknown) => {
                return Observation::unknown(format!(
                    "where the restart left the signal file on {} is unknown: nothing it holds is taken for the new process's",
                    session.host
                ));
            }
            Some(Boundary::At(offset)) => Some(offset),
            None => None,
        };
        // Two parts, each bounded so the whole answer stays under `exec`'s
        // cap: the end of what the file held at the restart (`since`), for
        // its prompts and conversation, then a line no report writes, then
        // the end of what came after, whose last state is the session's. A
        // file shorter than `since` was made again: all of it came after.
        // Exit 3 is a file never written.
        let offset = since.map(|o| o.to_string());
        let fold = |text: &str| {
            let (before, after) = text
                .split_once(crate::signals::RESTART_SPLIT)
                .unwrap_or(("", text));
            match since {
                Some(_) => Reported::fold_restarted(before, after),
                None => Reported::fold(after),
            }
        };
        bind(
            self.exec_on(
                &session.host,
                &PLACE_READ
                    .into_iter()
                    .chain([file.as_str()])
                    .chain(offset.as_deref())
                    .collect::<Vec<_>>(),
            ),
            |ran| match ran {
                Executed::Exited {
                    code: Some(0),
                    stdout,
                    ..
                } => Observation::Known(fold(&stdout)),
                Executed::Exited { code: Some(3), .. } => Observation::Known(fold("")),
                Executed::Exited { stderr, .. } => Observation::unknown(format!(
                    "cannot read the signal file on {}: {}",
                    session.host,
                    stderr.lines().next().unwrap_or("tail failed")
                )),
                Executed::Failed { why } => Observation::unknown(format!(
                    "cannot read the signal file on {}: {why}",
                    session.host
                )),
                Executed::TimedOut { .. } => Observation::unknown(format!(
                    "reading the signal file on {} timed out",
                    session.host
                )),
            },
        )
    }

    /// The session's outbox on its host from the offset `from`, through
    /// the runtime's `exec`: `fleet signal-read --from` there prints where
    /// its read starts, then the bytes. Exit 3 is a file never written.
    fn place_outbox(&self, session: &SessionRef, from: u64) -> Observation<Option<OutboxRead>> {
        let Some(signals) = &self.signals else {
            return Observation::unknown("no signal port: the outbox is not read");
        };
        let Some(file) = signals.file(&session.host, &session.key) else {
            return Observation::unknown("the session key is not a plain name");
        };
        let file = outbox_file(&file);
        let from = from.to_string();
        let argv = [PLACE_READ[0], PLACE_READ[1], "--from", &from, &file];
        bind(self.exec_on(&session.host, &argv), |ran| match ran {
            Executed::Exited {
                code: Some(0),
                stdout,
                ..
            } => crate::events::decode_place_read(&stdout).map_or_else(
                || Observation::unknown("fleet signal-read --from answered no offset"),
                |read| Observation::Known(Some(read)),
            ),
            Executed::Exited { code: Some(3), .. } => Observation::Known(None),
            Executed::Exited { stderr, .. } => Observation::unknown(format!(
                "cannot read the outbox on {}: {}",
                session.host,
                stderr.lines().next().unwrap_or("fleet signal-read failed")
            )),
            Executed::Failed { why } => {
                Observation::unknown(format!("cannot read the outbox on {}: {why}", session.host))
            }
            Executed::TimedOut { .. } => {
                Observation::unknown(format!("reading the outbox on {} timed out", session.host))
            }
        })
    }

    /// The length in bytes of the signal file `file` on `host`, 0 when it is
    /// not there, through the runtime's `exec`.
    fn place_file_length(&self, host: &str, file: &str) -> Length {
        let rt = match self.program(host) {
            Ok(rt) => rt,
            Err(why) => return Length::Unknown(why),
        };
        match rt.manifest() {
            Observation::Known(m) if !m.verbs.iter().any(|v| v == "exec") => {
                return Length::Unreadable;
            }
            Observation::Known(_) => {}
            Observation::Unknown { why } => return Length::Unknown(why),
        }
        match self.exec_on(host, &[PLACE_READ[0], PLACE_READ[1], "--length", file]) {
            Observation::Known(Executed::Exited {
                code: Some(0),
                stdout,
                ..
            }) => stdout.trim().parse().map_or_else(
                |_| Length::Unknown(format!("fleet signal-read answered {:?}", stdout.trim())),
                Length::At,
            ),
            // The runtime cannot run it at this place, as on a native-Windows
            // one: it cannot read the file either.
            Observation::Known(Executed::Failed { .. }) => Length::Unreadable,
            Observation::Known(Executed::Exited { stderr, .. }) => Length::Unknown(
                stderr
                    .lines()
                    .next()
                    .unwrap_or("fleet signal-read failed")
                    .to_string(),
            ),
            Observation::Known(Executed::TimedOut { .. }) => Length::Unknown("timed out".into()),
            Observation::Unknown { why } => Length::Unknown(why),
        }
    }

    /// The session's identity: the key's, which outlives each process a
    /// revive or a resume starts under it.
    fn session(&self, host: &str, key: &str) -> SessionRef {
        let id = self
            .program(host)
            .map_or_else(|_| key.to_string(), |rt| rt.key(key));
        SessionRef {
            id,
            host: host.to_string(),
            key: key.to_string(),
        }
    }
}

impl Runtime for ExecRuntime {
    fn operation_bound(&self) -> fleet_api::Millis {
        // A workspace call is a manifest, then the `workspace` verb.
        let longest = self
            .hosts
            .values()
            .map(|rt| {
                (fleet_exec::worst_case(rt.call_timeout) * MAX_CALLS_PER_OPERATION).max(
                    fleet_exec::worst_case(rt.call_timeout)
                        + fleet_exec::worst_case(rt.workspace_timeout),
                )
            })
            .max()
            .unwrap_or_default();
        u64::try_from(longest.as_millis()).unwrap_or(u64::MAX)
    }

    fn env_source(
        &self,
        source: &fleet_api::config::EnvSource,
    ) -> Observation<BTreeMap<String, String>> {
        crate::resolve_env_source(source)
    }

    /// A session on another host than this machine reports to a file
    /// there, read through the runtime's `exec`.
    fn signals(&self, session: &SessionRef) -> Option<Observation<Reported>> {
        (session.host != LOCAL_HOST).then(|| self.reported(session))
    }

    /// A session on another host than this machine publishes to an outbox
    /// there, read through the runtime's `exec`.
    fn outbox(&self, session: &SessionRef, from: u64) -> Option<Observation<Option<OutboxRead>>> {
        (session.host != LOCAL_HOST).then(|| self.place_outbox(session, from))
    }

    fn host(&self, host: &str) -> Observation<bool> {
        let rt = match self.program(host) {
            Ok(rt) => rt,
            Err(_) => return Observation::Known(false),
        };
        bind(rt.manifest(), |m| {
            if !PROTOCOLS.contains(&m.protocol) {
                return Observation::unknown(format!(
                    "runtime {} speaks protocol {}; Fleet speaks {PROTOCOLS:?}",
                    rt.name, m.protocol
                ));
            }
            if let Some(why) = rt.unplaced(m.protocol, &rt.place) {
                return Observation::unknown(why);
            }
            Observation::Known(true)
        })
    }

    fn create_dir(&self, host: &str, dir: &str) -> Observation<Result<bool, String>> {
        if !fleet_api::validate::is_absolute_path(dir) {
            return Observation::Known(Err(format!(
                "{dir:?} must be an absolute path on the host"
            )));
        }
        let script = "if [ -d \"$1\" ]; then echo kept; else mkdir -p -- \"$1\" && echo made; fi";
        self.exec_on(host, &["sh", "-c", script, "sh", dir])
            .map(|ran| match ran {
                Executed::Exited {
                    code: Some(0),
                    stdout,
                    ..
                } => Ok(stdout.trim() == "made"),
                Executed::Exited { stderr, .. } => Err(format!(
                    "cannot create {dir}: {}",
                    stderr.lines().next().unwrap_or("mkdir failed")
                )),
                Executed::Failed { why } => Err(format!("cannot create {dir}: {why}")),
                Executed::TimedOut { .. } => Err(format!("creating {dir} timed out")),
            })
    }

    fn remove_dir(&self, host: &str, dir: &str) -> Observation<Result<(), String>> {
        if !fleet_api::validate::is_absolute_path(dir) {
            return Observation::Known(Err(format!(
                "{dir:?} must be an absolute path on the host"
            )));
        }
        bind(
            self.exec_on(host, &["rm", "-rf", "--", dir]),
            |ran| match ran {
                Executed::Exited { code: Some(0), .. } => Observation::Known(Ok(())),
                Executed::Exited { stderr, .. } => Observation::Known(Err(format!(
                    "cannot remove {dir}: {}",
                    stderr.lines().next().unwrap_or("rm failed")
                ))),
                Executed::Failed { why } => {
                    Observation::Known(Err(format!("cannot remove {dir}: {why}")))
                }
                // Part of it may be gone: only the next call can tell.
                Executed::TimedOut { .. } => {
                    Observation::unknown(format!("removing {dir} timed out"))
                }
            },
        )
    }

    /// The runtime's `workspace` verb, asked only of a URL list its
    /// manifest lists every scheme of.
    fn workspace(&self, host: &str, key: &str, urls: &[String]) -> Observation<EffectOutcome> {
        let rt = match self.program(host) {
            Ok(rt) => rt,
            Err(why) => return Observation::unknown(why),
        };
        bind(rt.manifest(), |manifest| {
            if let Some(url) = manifest.unsupported(urls) {
                return Observation::Known(EffectOutcome::Unsupported { url: url.clone() });
            }
            let request = json!({ "key": rt.key(key), "urls": urls });
            rt.send::<WorkspaceAnswer>("workspace", request)
                .map(|answer| match answer {
                    WorkspaceAnswer::Made { dirs, adopted } => EffectOutcome::Materialized {
                        workspace: Box::new(WorkspaceRef {
                            key: key.to_string(),
                            host: host.to_string(),
                            dirs,
                        }),
                        adopted,
                    },
                    WorkspaceAnswer::Unsupported { url } => EffectOutcome::Unsupported { url },
                    WorkspaceAnswer::Failed { why } => EffectOutcome::Failed { why },
                })
        })
    }

    /// The runtime's `stop` with `release`: the process goes, and the
    /// workspace with it, its work kept as the runtime's rules say.
    fn release_workspace(&self, workspace: &WorkspaceRef) -> Observation<EffectOutcome> {
        let rt = match self.program(&workspace.host) {
            Ok(rt) => rt,
            Err(why) => return Observation::unknown(why),
        };
        let request = json!({ "key": rt.key(&workspace.key), "release": true });
        self.forget_inspected();
        rt.send::<Stopped>("stop", request)
            .map(|Stopped::Stopped| EffectOutcome::Deleted)
    }

    fn spawn(&self, spec: &SpawnSpec) -> Observation<EffectOutcome> {
        let argv = match spec.launch_argv() {
            Ok(argv) => argv,
            Err(why) => return Observation::Known(EffectOutcome::Failed { why }),
        };
        self.start(spec, &argv).map(|started| match started {
            Started::Started { adopted, .. } => EffectOutcome::Spawned {
                session: self.session(&spec.host, &spec.key),
                adopted,
            },
            Started::Failed { why } => EffectOutcome::Failed { why },
        })
    }

    fn find(&self, host: &str, key: &str) -> Observation<Option<SessionRef>> {
        self.inspect(host, key).map(|found| match found {
            Inspected::None => None,
            Inspected::Running { .. } | Inspected::Exited { .. } | Inspected::Paused => {
                Some(self.session(host, key))
            }
        })
    }

    /// Ready once the agent reported anything but a wait for a person: its
    /// start hook reports it before any prompt. Codex, Kimi Code and
    /// GitHub Copilot CLI report nothing before their first message, so
    /// before that their empty composer makes them ready.
    fn ready(&self, session: &SessionRef) -> Observation<bool> {
        bind(self.reported(session), |r| match r.state {
            Some(State::Blocked) => Observation::Known(false),
            Some(State::Started) | None => match self.first_screen(session) {
                Observation::Known(
                    FirstScreen::CodexComposer
                    | FirstScreen::KimiComposer
                    | FirstScreen::CopilotComposer,
                ) => Observation::Known(true),
                Observation::Known(_) => {
                    Observation::unknown("the agent reported no state since it started")
                }
                Observation::Unknown { why } => Observation::Unknown { why },
            },
            Some(_) => Observation::Known(true),
        })
    }

    /// An agent that reported anything runs past any workspace-trust
    /// dialog: hooks run only in a trusted workspace. Before that, a trust
    /// dialog on its screen is Untrusted, and Fleet never answers it.
    fn workspace_trust(
        &self,
        session: &SessionRef,
        _workspace: &Workspace,
        _ready: Option<&WorkspaceRef>,
    ) -> Observation<WorkspaceTrust> {
        match self.reported(session) {
            Observation::Known(r) if r.reported() => {
                return Observation::Known(WorkspaceTrust::Trusted {
                    how: "the agent reported its state, past any workspace-trust dialog".into(),
                });
            }
            Observation::Known(_) => {}
            Observation::Unknown { why } => return Observation::Unknown { why },
        }
        match self.first_screen(session) {
            Observation::Known(FirstScreen::TrustDialog) => {
                Observation::Known(WorkspaceTrust::Untrusted {
                    gap: TrustGap::Unsupported,
                    why: format!(
                        "the agent asks whether to trust its workspace on host {}, and Fleet never answers it: trust the directory once in the session. The brief follows.",
                        session.host
                    ),
                })
            }
            // The exec kind starts Codex with its launch override trusting
            // the workspace, so its composer is a trusted session's.
            Observation::Known(FirstScreen::CodexComposer) => {
                Observation::Known(WorkspaceTrust::Trusted {
                    how: "Codex runs with its launch override trusting the workspace".into(),
                })
            }
            // Kimi asks its folder trust before it shows its composer.
            Observation::Known(FirstScreen::KimiComposer) => {
                Observation::Known(WorkspaceTrust::Trusted {
                    how: "Kimi Code shows its composer only in a folder it trusts".into(),
                })
            }
            // Copilot asks its folder trust before it shows its composer.
            Observation::Known(FirstScreen::CopilotComposer) => {
                Observation::Known(WorkspaceTrust::Trusted {
                    how: "GitHub Copilot CLI shows its composer only in a folder it trusts".into(),
                })
            }
            Observation::Known(FirstScreen::Other) => Observation::unknown(
                "the agent has not reported yet and shows neither a trust dialog nor its composer",
            ),
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    fn brief(
        &self,
        session: &SessionRef,
        text: &str,
        _intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let rt = match self.program(&session.host) {
            Ok(rt) => rt,
            Err(why) => return Observation::unknown(why),
        };
        let request = json!({
            "key": rt.key(&session.key),
            "text": text,
            "submit": true,
        });
        rt.send::<Sent>("input", request).map(|sent| match sent {
            Sent::Sent => EffectOutcome::Delivered {
                already: false,
                via: Some("input".into()),
                may_hold: true,
            },
            Sent::None => EffectOutcome::Failed {
                why: "no process holds the session's key".into(),
            },
        })
    }

    /// A runtime cannot read a typed text back; the agent's prompt hook
    /// proves it, which the manager reads from the signal port.
    fn brief_delivered(&self, _session: &SessionRef, _intent_key: &str) -> Observation<bool> {
        Observation::unknown(
            "an exec runtime cannot read a brief back; the agent's prompt hook can",
        )
    }

    /// An agent that exited, or is gone, does no more work. A running one is
    /// at rest once its hooks last reported it idle.
    fn at_rest(&self, session: &SessionRef) -> Observation<bool> {
        bind(
            self.inspect(&session.host, &session.key),
            |found| match found {
                Inspected::Running { .. } => bind(self.reported(session), |r| match r.state {
                    Some(State::Idle) => Observation::Known(true),
                    Some(State::Started) | None => {
                        Observation::unknown("the running agent reported no state since it started")
                    }
                    Some(_) => Observation::Known(false),
                }),
                Inspected::Exited { .. } | Inspected::Paused | Inspected::None => {
                    Observation::Known(true)
                }
            },
        )
    }

    fn release(&self, session: &SessionRef) -> Observation<EffectOutcome> {
        let rt = match self.program(&session.host) {
            Ok(rt) => rt,
            Err(why) => return Observation::unknown(why),
        };
        let request = json!({ "key": rt.key(&session.key), "release": true });
        self.forget_inspected();
        rt.send::<Stopped>("stop", request)
            .map(|Stopped::Stopped| EffectOutcome::Released)
    }

    /// The runtime's `capture`, reused for [`SCREEN_REUSE`] by this process.
    fn screen(&self, session: &SessionRef) -> Observation<Option<fleet_api::screen::Screen>> {
        let screens = || {
            self.screens
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        let now = Instant::now();
        if let Some((at, screen)) = screens().get(&session.id)
            && now.duration_since(*at) < SCREEN_REUSE
        {
            return screen.clone();
        }
        // Not under the lock: a capture runs a process.
        let screen = self.capture(session).map(|captured| match captured {
            Captured::Screen { text, title, .. } => Some(fleet_api::screen::Screen {
                text,
                title: title.unwrap_or_default(),
            }),
            Captured::None => None,
        });
        let mut screens = screens();
        screens.retain(|_, (at, _)| now.duration_since(*at) < SCREEN_REUSE);
        screens.insert(session.id.clone(), (now, screen.clone()));
        screen
    }

    /// The runtime's `attach`, asked only when its manifest declares it: a
    /// runtime without the verb offers no live view.
    fn attach(&self, session: &SessionRef) -> Observation<Option<Attach>> {
        self.opener("attach", session)
    }

    /// The runtime's `shell`, asked only when its manifest declares it: a
    /// runtime without the verb offers no shell.
    fn shell(&self, session: &SessionRef) -> Observation<Option<Attach>> {
        self.opener("shell", session)
    }

    /// Lost when the runtime holds the key but its process exited: a `stop`
    /// removes the key, so only the agent's own exit, or a crash, leaves
    /// one. A key the runtime no longer holds proves nothing either way.
    fn lost(&self, session: &SessionRef) -> Observation<bool> {
        bind(
            self.inspect(&session.host, &session.key),
            |found| match found {
                Inspected::Running { .. } | Inspected::Paused => Observation::Known(false),
                Inspected::Exited { .. } => Observation::Known(true),
                Inspected::None => Observation::unknown(
                    "the runtime holds nothing under the key, which a stop also leaves",
                ),
            },
        )
    }

    fn revive(
        &self,
        session: &SessionRef,
        conversation: Option<&str>,
        launch: &SpawnSpec,
    ) -> Observation<EffectOutcome> {
        self.restart(session, conversation, launch, EffectOutcome::Revived)
    }

    /// Stopped under its key, as `inspect` answers it, on a runtime whose
    /// manifest declares `pause`; Unknown on any other, so Fleet never
    /// pauses there.
    fn paused(&self, session: &SessionRef) -> Observation<bool> {
        bind(self.pausing(&session.host), |declared| {
            if let Err(why) = declared {
                return Observation::unknown(why);
            }
            bind(
                self.inspect(&session.host, &session.key),
                |found| match found {
                    Inspected::Paused => Observation::Known(true),
                    Inspected::Running { .. } | Inspected::Exited { .. } => {
                        Observation::Known(false)
                    }
                    Inspected::None => {
                        Observation::unknown("the runtime holds nothing under the key")
                    }
                },
            )
        })
    }

    /// The runtime's `pause`: the process stops and the key stays.
    fn pause(&self, session: &SessionRef) -> Observation<EffectOutcome> {
        bind(self.pausing(&session.host), |declared| {
            let rt = match declared {
                Ok(rt) => rt,
                Err(why) => return Observation::Known(EffectOutcome::Failed { why }),
            };
            self.forget_inspected();
            rt.send::<PauseAnswer>("pause", json!({ "key": rt.key(&session.key) }))
                .map(|answer| match answer {
                    PauseAnswer::Paused => EffectOutcome::Paused,
                    PauseAnswer::None => EffectOutcome::Failed {
                        why: "no process holds the session's key".into(),
                    },
                })
        })
    }

    fn resume(
        &self,
        session: &SessionRef,
        conversation: Option<&str>,
        launch: &SpawnSpec,
    ) -> Observation<EffectOutcome> {
        self.restart(session, conversation, launch, EffectOutcome::Resumed)
    }
}

/// One case of the conformance suite, and what went wrong if it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    pub name: &'static str,
    pub result: Result<(), String>,
}

/// Whether `program` answers its `manifest` as Fleet speaks the protocol,
/// and its place is ready as the manifest reports it: what `fleet doctor`
/// asks of each place's runtime. [`check`]'s first case is the protocol half.
pub fn reach(program: &ExecProgram) -> Result<(), String> {
    let manifest = program.manifest();
    speaks(&manifest)?;
    match readiness(program, &program.place, &manifest) {
        Readiness {
            ready: Some(true), ..
        } => Ok(()),
        Readiness { why, .. } => {
            Err(why.unwrap_or_else(|| format!("place {} is not ready", program.place.name)))
        }
    }
}

/// What `manifest` says of `place`, one of `program`'s places: the one rule
/// `fleet doctor` and `fleet runtime places` read readiness by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    /// `None` when it is unknown: the runtime did not answer, or did not
    /// report the place.
    pub ready: Option<bool>,
    pub why: Option<String>,
    /// The runtime's own report, from protocol version 2.
    pub report: Option<PlaceReport>,
}

/// [`Readiness`] of `place` by `manifest`. A runtime of protocol version 1
/// reports no place: it is ready at `local`, with no config, and serves no
/// other.
pub fn readiness(
    program: &ExecProgram,
    place: &Place,
    manifest: &Observation<Manifest>,
) -> Readiness {
    let unknown = |why: String| Readiness {
        ready: None,
        why: Some(why),
        report: None,
    };
    let m = match manifest {
        Observation::Known(m) => m,
        Observation::Unknown { why } => return unknown(why.clone()),
    };
    if let Some(why) = program.unplaced(m.protocol, place) {
        return Readiness {
            ready: Some(false),
            why: Some(why),
            report: None,
        };
    }
    if m.protocol < 2 {
        return Readiness {
            ready: Some(true),
            why: None,
            report: None,
        };
    }
    match m.places.iter().find(|r| r.name == place.name) {
        Some(r) => Readiness {
            ready: Some(r.ready),
            why: r.why.clone(),
            report: Some(r.clone()),
        },
        None => unknown(format!(
            "runtime {} did not report its place {}",
            program.name, place.name
        )),
    }
}

fn speaks(manifest: &Observation<Manifest>) -> Result<(), String> {
    match manifest {
        Observation::Known(m) if !PROTOCOLS.contains(&m.protocol) => Err(format!(
            "protocol {}; Fleet speaks {PROTOCOLS:?}",
            m.protocol
        )),
        Observation::Known(m) => match REQUIRED_VERBS
            .iter()
            .find(|v| !m.verbs.iter().any(|have| have == *v))
        {
            Some(missing) => Err(format!("the verbs lack {missing}")),
            None => Ok(()),
        },
        Observation::Unknown { why } => Err(why.clone()),
    }
}

/// `fleet runtime check`: the conformance suite, run against `program`.
/// Each case stands alone; the keys it uses are its own and stopped after.
pub fn check(program: &ExecProgram) -> Vec<Case> {
    let run = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let key = |n: &str| format!("fleet-check-{run}-{nanos:x}.{n}");
    let mut cases = Vec::new();
    let manifest = program.manifest();
    cases.push(Case {
        name: "manifest names the protocol version and the required verbs",
        result: speaks(&manifest),
    });
    let (protocol, verbs) = match &manifest {
        Observation::Known(m) if PROTOCOLS.contains(&m.protocol) => (m.protocol, m.verbs.clone()),
        Observation::Known(m) => (PROTOCOL, m.verbs.clone()),
        Observation::Unknown { .. } => (PROTOCOL, Vec::new()),
    };
    let request = |fields: Value| program.request(protocol, fields);
    let start = |k: &str, argv: &[&str]| {
        program.ask::<Started>(
            "start",
            request(json!({ "key": k, "argv": argv, "env": {} })),
        )
    };
    let inspect = |k: &str| program.ask::<Inspected>("inspect", request(json!({ "key": k })));
    let stop = |k: &str| program.ask::<Stopped>("stop", request(json!({ "key": k })));
    let unknown = |verb: &str, request: Vec<u8>| match program.call_raw(verb, request) {
        Answer::Unknown(_) => Ok(()),
        Answer::Known(v) => Err(format!("answered Known {v}")),
    };

    cases.push(Case {
        name: "a protocol mismatch answers Unknown",
        result: unknown(
            "inspect",
            json!({ "protocol": PROTOCOL + 1000, "key": key("mismatch") })
                .to_string()
                .into_bytes(),
        ),
    });
    cases.push(Case {
        name: "an error answers Unknown, never None",
        result: unknown("inspect", b"{ this is not json".to_vec())
            .and_then(|()| {
                unknown(
                    "inspect",
                    request(json!({ "key": "not a key / at all" }))
                        .to_string()
                        .into_bytes(),
                )
            })
            .and_then(|()| unknown("no-such-verb", request(json!({})).to_string().into_bytes())),
    });
    if protocol >= 2 {
        cases.push(Case {
            name: "manifest reports each place it was asked about",
            result: match &manifest {
                Observation::Known(m) if m.places.iter().any(|p| p.name == Place::LOCAL) => Ok(()),
                Observation::Known(m) => {
                    Err(format!("places {:?} lack {}", m.places, Place::LOCAL))
                }
                Observation::Unknown { why } => Err(why.clone()),
            },
        });
        cases.push(Case {
            name: "a version 2 request without a place answers Unknown",
            result: unknown(
                "inspect",
                json!({ "protocol": protocol, "key": key("placeless") })
                    .to_string()
                    .into_bytes(),
            ),
        });
    }
    cases.push(Case {
        name: "inspect of an absent key answers None",
        result: match inspect(&key("absent")) {
            Observation::Known(Inspected::None) => Ok(()),
            other => Err(format!("answered {other:?}")),
        },
    });
    cases.push(Case {
        name: "a stop of an absent process succeeds",
        result: match stop(&key("absent")) {
            Observation::Known(Stopped::Stopped) => Ok(()),
            Observation::Unknown { why } => Err(why),
        },
    });

    let one = key("one");
    let twice = (|| {
        let first = match start(&one, &["sleep", "300"]) {
            Observation::Known(Started::Started { id, adopted: false }) => id,
            other => return Err(format!("the first start answered {other:?}")),
        };
        match start(&one, &["sleep", "300"]) {
            Observation::Known(Started::Started { id, adopted: true }) if id == first => {}
            other => {
                return Err(format!(
                    "the second start answered {other:?}, not {first} adopted"
                ));
            }
        }
        match inspect(&one) {
            Observation::Known(Inspected::Running { id }) if id == first => {}
            other => return Err(format!("inspect answered {other:?}, not {first} running")),
        }
        if verbs.iter().any(|v| v == "list") {
            match program.ask::<Keys>("list", request(json!({}))) {
                Observation::Known(Keys { keys })
                    if keys.iter().filter(|k| **k == one).count() == 1 => {}
                other => return Err(format!("list answered {other:?}, not the key once")),
            }
        }
        Ok(())
    })();
    cases.push(Case {
        name: "two starts with one key leave one process",
        result: twice,
    });

    // Every verb is idempotent by key even when calls overlap: a start looks
    // the key up, then makes its process, and two at once must not both make
    // one.
    let racing = key("race");
    let raced = (|| {
        let answers: Vec<Observation<Started>> = std::thread::scope(|s| {
            let calls: Vec<_> = (0..4)
                .map(|_| s.spawn(|| start(&racing, &["sleep", "300"])))
                .collect();
            calls
                .into_iter()
                .map(|c| {
                    c.join()
                        .unwrap_or_else(|_| Observation::unknown("the call panicked"))
                })
                .collect()
        });
        let mut ids = std::collections::BTreeSet::new();
        let mut made = 0;
        for answer in &answers {
            match answer {
                Observation::Known(Started::Started { id, adopted }) => {
                    ids.insert(id.clone());
                    made += usize::from(!adopted);
                }
                other => return Err(format!("a start answered {other:?}")),
            }
        }
        if ids.len() != 1 || made != 1 {
            return Err(format!("four starts at once answered {answers:?}"));
        }
        Ok(())
    })();
    let _ = stop(&racing);
    cases.push(Case {
        name: "concurrent starts with one key leave one process",
        result: raced,
    });

    cases.push(Case {
        name: "a stop ends the process",
        result: match (stop(&one), inspect(&one)) {
            (Observation::Known(Stopped::Stopped), Observation::Known(Inspected::None)) => Ok(()),
            (stopped, inspected) => Err(format!("stop {stopped:?}, then inspect {inspected:?}")),
        },
    });

    let exits = key("exits");
    let exited = (|| {
        match start(&exits, &["sh", "-c", "exit 3"]) {
            Observation::Known(Started::Started { .. }) => {}
            other => return Err(format!("start answered {other:?}")),
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match inspect(&exits) {
                Observation::Known(Inspected::Exited {
                    code: Some(3) | None,
                    ..
                })
                | Observation::Known(Inspected::None) => return Ok(()),
                Observation::Known(Inspected::Exited { code, .. }) => {
                    return Err(format!("exited with {code:?}, not 3"));
                }
                other if std::time::Instant::now() >= deadline => {
                    return Err(format!("still {other:?} after ten seconds"));
                }
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    })();
    cases.push(Case {
        name: "an exited process answers Exited with its code, or None",
        result: exited,
    });
    let _ = stop(&exits);

    if verbs.iter().any(|v| v == "workspace") {
        cases.extend(check_workspace(program, protocol, &key("workspace")));
    }
    if verbs.iter().any(|v| v == "browse") {
        cases.extend(check_browse(program, protocol, &key("browse")));
    }
    if verbs.iter().any(|v| v == "branches") {
        cases.extend(check_branches(program, protocol, &key("branches")));
    }
    if verbs.iter().any(|v| v == "shell") {
        cases.extend(check_shell(program, protocol, &key("shell")));
    }
    if verbs.iter().any(|v| v == "pause") {
        let listed = verbs.iter().any(|v| v == "list");
        cases.extend(check_pause(program, protocol, &key("pause"), listed));
    }
    cases
}

/// The pause rules a runtime that declares `pause` holds to: a pause stops
/// the key's process and keeps the key, `list` names it, a second pause
/// answers the same, a start runs the key again and a stop forgets it; a
/// key nothing holds answers `none`.
fn check_pause(program: &ExecProgram, protocol: u64, key: &str, listed: bool) -> Vec<Case> {
    let request = |fields: Value| program.request(protocol, fields);
    let pause = |k: &str| program.ask::<PauseAnswer>("pause", request(json!({ "key": k })));
    let inspect = |k: &str| program.ask::<Inspected>("inspect", request(json!({ "key": k })));
    let start = |k: &str| {
        program.ask::<Started>(
            "start",
            request(json!({ "key": k, "argv": ["sleep", "300"], "env": {} })),
        )
    };
    let stop = |k: &str| program.ask::<Stopped>("stop", request(json!({ "key": k })));
    let absent = match pause(&format!("{key}-absent")) {
        Observation::Known(PauseAnswer::None) => Ok(()),
        other => Err(format!("answered {other:?}, not none")),
    };
    let kept = (|| {
        match start(key) {
            Observation::Known(Started::Started { .. }) => {}
            other => return Err(format!("start answered {other:?}")),
        }
        for n in ["the first", "a second"] {
            match pause(key) {
                Observation::Known(PauseAnswer::Paused) => {}
                other => return Err(format!("{n} pause answered {other:?}")),
            }
            match inspect(key) {
                Observation::Known(Inspected::Paused) => {}
                other => return Err(format!("inspect after {n} pause answered {other:?}")),
            }
        }
        if listed {
            match program.ask::<Keys>("list", request(json!({}))) {
                Observation::Known(Keys { keys }) if keys.iter().any(|k| k == key) => {}
                other => return Err(format!("list answered {other:?}, without the paused key")),
            }
        }
        match (start(key), inspect(key)) {
            (
                Observation::Known(Started::Started { adopted: false, .. }),
                Observation::Known(Inspected::Running { .. }),
            ) => {}
            (started, inspected) => {
                return Err(format!(
                    "a start of the paused key answered {started:?}, then inspect {inspected:?}"
                ));
            }
        }
        match (pause(key), stop(key), inspect(key)) {
            (
                Observation::Known(PauseAnswer::Paused),
                Observation::Known(Stopped::Stopped),
                Observation::Known(Inspected::None),
            ) => Ok(()),
            (paused, stopped, inspected) => Err(format!(
                "pause {paused:?}, stop {stopped:?}, then inspect {inspected:?}"
            )),
        }
    })();
    let _ = stop(key);
    vec![
        Case {
            name: "pause: a key no process holds answers none",
            result: absent,
        },
        Case {
            name: "pause: stops the process and keeps the key until a start or a stop",
            result: kept,
        },
    ]
}

/// The shell rules a runtime that declares `shell` holds to: how to open a
/// shell for a key a process holds, on each call; `none` for a key nothing
/// holds, as `attach` answers, and after a stop. That a second call reaches
/// the same shell is not checked here: it needs a view typing into the
/// answer, and a check runs processes only bounded, through `fleet-exec`.
fn check_shell(program: &ExecProgram, protocol: u64, key: &str) -> Vec<Case> {
    let shell =
        |k: &str| program.ask::<Attached>("shell", program.request(protocol, json!({ "key": k })));
    let stop =
        |k: &str| program.ask::<Stopped>("stop", program.request(protocol, json!({ "key": k })));
    let absent = match shell(&format!("{key}-absent")) {
        Observation::Known(Attached::None) => Ok(()),
        other => Err(format!("answered {other:?}, not none")),
    };
    let opened = (|| {
        match program.ask::<Started>(
            "start",
            program.request(
                protocol,
                json!({ "key": key, "argv": ["sleep", "300"], "env": {} }),
            ),
        ) {
            Observation::Known(Started::Started { .. }) => {}
            other => return Err(format!("start answered {other:?}")),
        }
        let first = match shell(key) {
            Observation::Known(Attached::Stream { argv } | Attached::Tty { argv })
                if !argv.is_empty() =>
            {
                argv
            }
            other => return Err(format!("answered {other:?}, not a stream or a tty argv")),
        };
        match shell(key) {
            Observation::Known(Attached::Stream { argv } | Attached::Tty { argv })
                if !argv.is_empty() =>
            {
                Ok(())
            }
            other => Err(format!(
                "the second call answered {other:?} after {first:?}"
            )),
        }
    })();
    let stopped = match (stop(key), shell(key)) {
        (Observation::Known(Stopped::Stopped), Observation::Known(Attached::None)) => Ok(()),
        (stopped, shell) => Err(format!("stop {stopped:?}, then shell {shell:?}")),
    };
    vec![
        Case {
            name: "shell: a key no process holds answers none",
            result: absent,
        },
        Case {
            name: "shell: a running key answers how to open its shell, on each call",
            result: opened,
        },
        Case {
            name: "shell: a stop leaves nothing to open",
            result: stopped,
        },
    ]
}

/// A directory of the check's own under the temporary directory, made anew.
fn scratch(key: &str, what: &str) -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("{}-{what}", key.replace('.', "-")));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .map(|()| dir.clone())
        .map_err(|e| format!("cannot make {}: {e}", dir.display()))
}

/// The browse rules a runtime that declares `browse` holds to: the
/// subdirectories of a directory, by name, each marked when it is a git
/// work tree (made with this machine's `git`), files left out; and a failed
/// browse of one that is not there.
fn check_browse(program: &ExecProgram, protocol: u64, key: &str) -> Vec<Case> {
    let browse = |dir: &std::path::Path| {
        program.ask::<Value>(
            "browse",
            program.request(protocol, json!({ "dir": dir.display().to_string() })),
        )
    };
    let listed = scratch(key, "browse").and_then(|dir| {
        let made = std::fs::create_dir_all(dir.join("plain"))
            .and_then(|()| std::fs::create_dir_all(dir.join("repo")))
            .and_then(|()| std::fs::write(dir.join("notes.txt"), "a file"))
            .map_err(|e| format!("cannot fill {}: {e}", dir.display()))
            .and_then(|()| git_in(&dir.join("repo"), &["init", "-q"]));
        let answer = made.map(|()| browse(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        let want = json!({"listing": {
            "dir": dir.display().to_string(),
            "entries": [{"name": "plain", "repo": false}, {"name": "repo", "repo": true}],
        }});
        match answer? {
            Observation::Known(got) if got == want => Ok(()),
            other => Err(format!("answered {other:?}, not {want}")),
        }
    });
    let missing = std::env::temp_dir().join(format!("{}-absent", key.replace('.', "-")));
    let refused = match browse(&missing) {
        Observation::Known(got) if got["failed"]["why"].is_string() => Ok(()),
        other => Err(format!("answered {other:?}, not failed")),
    };
    vec![
        Case {
            name: "browse: lists the subdirectories and marks a repository",
            result: listed,
        },
        Case {
            name: "browse: a missing directory answers failed",
            result: refused,
        },
    ]
}

/// The branch rules a runtime that declares `branches` holds to: a
/// repository's branches with its default, and a failed answer for a
/// directory that is no repository, even inside one. The repository is
/// made with this machine's `git`.
fn check_branches(program: &ExecProgram, protocol: u64, key: &str) -> Vec<Case> {
    let branches = |repo: &std::path::Path| {
        program.ask::<Value>(
            "branches",
            program.request(protocol, json!({ "repo": repo.display().to_string() })),
        )
    };
    let listed = scratch(key, "branches").and_then(|repo| {
        let made = [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "first"][..],
            &["branch", "feat"][..],
        ]
        .iter()
        .try_for_each(|args| git_in(&repo, args));
        let answer = made.map(|()| branches(&repo));
        let _ = std::fs::remove_dir_all(&repo);
        // The names are compared as a set: their order is the runtime's.
        let names = |got: &Value| {
            let mut names: Vec<String> = got["branches"]["names"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect();
            names.sort();
            names
        };
        match answer? {
            Observation::Known(got)
                if names(&got) == ["feat", "main"] && got["branches"]["default"] == "main" =>
            {
                Ok(())
            }
            other => Err(format!(
                "answered {other:?}, not the names feat and main with main as default"
            )),
        }
    });
    let refused = scratch(key, "plain").and_then(|outer| {
        let plain = outer.join("plain");
        let answer = git_in(&outer, &["init", "-q"])
            .and_then(|()| {
                std::fs::create_dir_all(&plain)
                    .map_err(|e| format!("cannot make {}: {e}", plain.display()))
            })
            .map(|()| branches(&plain));
        let _ = std::fs::remove_dir_all(&outer);
        let answer = answer?;
        match answer {
            Observation::Known(got) if got["failed"]["why"].is_string() => Ok(()),
            other => Err(format!("answered {other:?}, not failed")),
        }
    });
    vec![
        Case {
            name: "branches: lists a repository's branches and its default",
            result: listed,
        },
        Case {
            name: "branches: a directory that is no repository answers failed",
            result: refused,
        },
    ]
}

/// `git <args>` in `dir` for a check's fixture, through `fleet-exec`, with
/// an identity of its own and the variables that would point git at another
/// repository cleared.
fn git_in(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let mut spec = Spec::new("git", Duration::from_secs(30))
        .args([
            "-c",
            "user.name=fleet",
            "-c",
            "user.email=check@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args.iter().copied());
    spec.cwd = Some(dir.to_path_buf());
    spec.env_remove = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ]
    .map(String::from)
    .to_vec();
    let out = fleet_exec::run(&spec);
    match out.success_text() {
        Some(_) => Ok(()),
        None => Err(format!("git {}: {}", args.join(" "), out.failure())),
    }
}

#[derive(Debug, Deserialize)]
struct Keys {
    keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum WorkspaceAnswer {
    Made { dirs: Vec<String>, adopted: bool },
    Unsupported { url: String },
    Failed { why: String },
}

/// The workspace rules a runtime that declares `workspace` holds to.
fn check_workspace(program: &ExecProgram, protocol: u64, key: &str) -> Vec<Case> {
    let dir = std::env::temp_dir().join(format!("{}-file", key.replace('.', "-")));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return vec![Case {
            name: "workspace: a file:// directory is used as it is",
            result: Err(format!("cannot make {}: {e}", dir.display())),
        }];
    }
    let url = format!("file://{}", dir.display());
    let make = || {
        program.ask::<WorkspaceAnswer>(
            "workspace",
            program.request(protocol, json!({ "key": key, "urls": [url] })),
        )
    };
    let idempotent = match (make(), make()) {
        (
            Observation::Known(WorkspaceAnswer::Made { dirs: a, .. }),
            Observation::Known(WorkspaceAnswer::Made {
                dirs: b,
                adopted: true,
            }),
        ) if a == b => Ok(()),
        (Observation::Known(WorkspaceAnswer::Unsupported { url }), _) => {
            let _ = std::fs::remove_dir_all(&dir);
            return vec![Case {
                name: "workspace: idempotent by key",
                result: Err(format!(
                    "{url}: file:// is unsupported, so no rule could be checked"
                )),
            }];
        }
        (Observation::Known(WorkspaceAnswer::Failed { why }), _) => {
            Err(format!("the first call failed: {why}"))
        }
        (first, second) => Err(format!("answered {first:?}, then {second:?}")),
    };
    let released = program.ask::<Stopped>(
        "stop",
        program.request(protocol, json!({ "key": key, "release": true })),
    );
    let kept = match released {
        Observation::Known(Stopped::Stopped) if dir.is_dir() => Ok(()),
        Observation::Known(Stopped::Stopped) => Err(format!("release removed {}", dir.display())),
        Observation::Unknown { why } => Err(why),
    };
    let _ = std::fs::remove_dir_all(&dir);
    vec![
        Case {
            name: "workspace: idempotent by key",
            result: idempotent,
        },
        Case {
            name: "workspace: release never removes what it did not make",
            result: kept,
        },
    ]
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A runtime that answers each verb with what the file of the verb's
    /// name beside it holds, and fails when there is none.
    fn runtime(dir: &std::path::Path) -> ExecProgram {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("runtime.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\nexec cat '{}/'\"$1\"\n",
                dir.display()
            ),
        )
        .expect("script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        ExecProgram::new("test", script.to_string_lossy())
    }

    /// A restart on another host is three calls: the manifest, the file's
    /// length through `exec`, then `start`. The lease covers all three.
    #[test]
    fn the_operation_bound_covers_a_remote_restart() {
        let mut rt = ExecProgram::new("mux", "runtime-mux");
        rt.call_timeout = Duration::from_secs(10);
        rt.workspace_timeout = Duration::from_millis(1);
        let runtime = ExecRuntime::new(BTreeMap::from([("mux/box".to_string(), rt)]));
        let three = fleet_exec::worst_case(Duration::from_secs(10)) * 3;
        assert!(
            u128::from(runtime.operation_bound()) >= three.as_millis(),
            "{} ms",
            runtime.operation_bound()
        );
    }

    #[test]
    fn an_environment_source_resolves_on_the_manager() {
        let dir = std::env::temp_dir().join(format!("fleet-exec-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("tokens.json");
        std::fs::write(&path, r#"{"TOKEN":"t"}"#).expect("source");
        let runtime = ExecRuntime::new(BTreeMap::from([(
            "mux/box".to_string(),
            ExecProgram::new("mux", "runtime-mux"),
        )]));
        let resolved = runtime.env_source(&fleet_api::config::EnvSource::File {
            path: path.to_string_lossy().into_owned(),
        });
        assert_eq!(
            resolved,
            Observation::Known(BTreeMap::from([("TOKEN".to_string(), "t".to_string())]))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_that_answers_unknown_forgets_the_version() {
        let dir = std::env::temp_dir().join(format!("fleet-exec-spoken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let rt = runtime(&dir);
        std::fs::write(dir.join("manifest"), MANIFEST_2).expect("answer");
        assert!(!rt.manifest().is_unknown());
        assert_eq!(rt.spoken(), Some(2));
        std::fs::remove_file(dir.join("manifest")).expect("remove");
        assert!(rt.manifest().is_unknown());
        assert_eq!(rt.spoken(), None, "an Unknown manifest proves no version");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const MANIFEST_2: &str =
        r#"{"known":{"protocol":2,"verbs":["manifest","start","inspect","stop"]}}"#;

    /// A runtime replaced by one of another version answers Unknown to the
    /// version Fleet learned: the next request asks the manifest again.
    #[test]
    fn an_unknown_answer_forgets_the_version() {
        let dir = std::env::temp_dir().join(format!("fleet-exec-send-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let rt = runtime(&dir);
        std::fs::write(dir.join("manifest"), MANIFEST_2).expect("answer");
        std::fs::write(dir.join("inspect"), r#"{"known":"none"}"#).expect("answer");
        let found: Observation<Inspected> = rt.send("inspect", json!({ "key": "k" }));
        assert_eq!(found, Observation::Known(Inspected::None));
        assert_eq!(rt.spoken(), Some(2));
        std::fs::write(
            dir.join("inspect"),
            r#"{"unknown":{"why":"protocol 2 is not served"}}"#,
        )
        .expect("answer");
        let found: Observation<Inspected> = rt.send("inspect", json!({ "key": "k" }));
        assert!(found.is_unknown(), "{found:?}");
        assert_eq!(
            rt.spoken(),
            None,
            "the next request asks the manifest again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_probes_of_one_session_share_one_inspect_until_a_verb_changes_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fleet-exec-inspect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let script = dir.join("runtime.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\necho \"$1\" >> '{0}/calls'\nexec cat '{0}/'\"$1\"\n",
                dir.display()
            ),
        )
        .expect("script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(
            dir.join("manifest"),
            r#"{"known":{"protocol":2,"verbs":["manifest","start","inspect","stop","pause"]}}"#,
        )
        .expect("answer");
        std::fs::write(dir.join("inspect"), r#"{"known":"paused"}"#).expect("answer");
        std::fs::write(dir.join("stop"), r#"{"known":"stopped"}"#).expect("answer");
        std::fs::write(dir.join("pause"), r#"{"known":"paused"}"#).expect("answer");
        std::fs::write(dir.join("start"), r#"{"known":"started"}"#).expect("answer");
        let runtime = ExecRuntime::new(BTreeMap::from([(
            "local".to_string(),
            ExecProgram::new("test", script.to_string_lossy()),
        )]));
        let inspects = || {
            std::fs::read_to_string(dir.join("calls"))
                .unwrap_or_default()
                .lines()
                .filter(|l| *l == "inspect")
                .count()
        };
        let session = runtime.session("local", "k");
        assert_eq!(
            runtime.find("local", "k"),
            Observation::Known(Some(session.clone()))
        );
        assert_eq!(runtime.lost(&session), Observation::Known(false));
        assert_eq!(runtime.paused(&session), Observation::Known(true));
        assert_eq!(inspects(), 1, "one inspect served the three probes");
        runtime.find("local", "other");
        assert_eq!(inspects(), 2, "another key is asked");
        runtime.find("local", "other");
        runtime.release(&runtime.session("local", "other"));
        runtime.find("local", "other");
        assert_eq!(inspects(), 3, "a stop changed the process");
        runtime.pause(&runtime.session("local", "other"));
        runtime.find("local", "other");
        assert_eq!(inspects(), 4, "a pause changed the process");
        runtime.start(
            &SpawnSpec {
                host: "local".into(),
                key: "other".into(),
                harness: "claude-code".into(),
                workspace: Workspace::default(),
                ready: None,
                model: None,
                env: BTreeMap::new(),
                environment_frozen: false,
                telemetry_args: Vec::new(),
            },
            &["agent".to_string()],
        );
        runtime.find("local", "other");
        assert_eq!(inspects(), 5, "a start changed the process");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
