//! Fleet's file-backed store: one JSON file per object in a data directory
//! (Q9), written through [`durable`]'s primitives under one store lock.
//!
//! Layout of a data directory:
//!
//! ```text
//! store.json                  format marker: {"format": "fleet-store", "version": 1}
//! lock                        the store lock (an OS file lock)
//! seq                         the last resourceVersion handed out
//! lease.json                  the manager lease, if one is held
//! objects/<Kind>/<name>.json  one object, spec and status together (Q6)
//! tmp/                        temp files; anything here at open is a crashed write
//! ```
//!
//! Reads take no lock: every record is replaced atomically, so a read sees a
//! whole old or whole new file. Every write takes the store lock, re-reads,
//! checks, then writes; that is what makes create exclusive and update a
//! compare-and-swap across processes.

pub mod durable;
mod plugin;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fleet_api::conversation::{self, ConversationStatus, Spooled};
use fleet_api::follow::ForgeStatus;
use fleet_api::gateway::Delivery;
use fleet_api::inbox::{DeliveryReceipt, InboxEntry};
use fleet_api::kinds;
use fleet_api::listener::{self, Beat, ListenerStatus};
use fleet_api::talk::{self, GatewayStatus};
use fleet_api::task::{self, TaskStatus};
use fleet_api::validate::{
    validate_kind, validate_name, validate_object, validate_spec_update, validate_status,
};
use fleet_api::{
    Clock, DeleteOutcome, FieldManager, LeaseToken, Millis, Object, Observation, Store, StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::durable::LockGuard;

const FORMAT: &str = "fleet-store";
const FORMAT_VERSION: u32 = 1;
const LAUNCH_CAP: usize = 1024 * 1024;

/// Wall-clock milliseconds: the store stamps records and expires leases.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Millis {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as Millis)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    format: String,
    version: u32,
}

pub struct FileStore {
    root: PathBuf,
    clock: Arc<dyn Clock + Send + Sync>,
    plugins: BTreeMap<String, (String, fleet_api::plugin::PluginKind)>,
    plugin_rights: BTreeMap<String, fleet_api::plugin::TaskAccess>,
}

impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl FileStore {
    /// Open the store at `root`, creating it when the directory is absent or
    /// empty. A non-empty directory that is not a fleet store is refused, so
    /// a mistyped path never turns someone's files into a store.
    pub fn open(root: &Path) -> Result<FileStore, StoreError> {
        FileStore::open_with_clock(root, Arc::new(SystemClock))
    }

    pub fn open_with_clock(
        root: &Path,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Result<FileStore, StoreError> {
        let store = FileStore {
            root: root.to_path_buf(),
            clock,
            plugins: BTreeMap::new(),
            plugin_rights: BTreeMap::new(),
        };
        let marker = root.join("store.json");
        match fs::read(&marker) {
            Ok(bytes) => store.check_marker(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => store.init()?,
            Err(e) => return Err(io_unknown(&marker, e)),
        }
        store.clean_temp()?;
        Ok(store)
    }

    fn check_marker(&self, bytes: &[u8]) -> Result<(), StoreError> {
        let m: Marker = serde_json::from_slice(bytes).map_err(|e| {
            StoreError::Unknown(format!(
                "{}: unreadable store marker: {e}",
                self.root.display()
            ))
        })?;
        if m.format != FORMAT || m.version != FORMAT_VERSION {
            return Err(StoreError::Unsupported(format!(
                "{} holds {} version {}; this build reads {FORMAT} version {FORMAT_VERSION}",
                self.root.display(),
                m.format,
                m.version
            )));
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create the store under its lock, so two processes opening the same
    /// fresh directory at once agree on one marker.
    fn init(&self) -> Result<(), StoreError> {
        match fs::read_dir(&self.root) {
            Ok(entries) => {
                // Only a store's own entries may be present: a second process
                // can be initialising the same empty directory right now.
                let own = ["store.json", "lock", "seq", "lease.json", "objects", "tmp"];
                let foreign = entries
                    .filter_map(Result::ok)
                    .any(|e| !own.iter().any(|o| e.file_name() == *o));
                if foreign {
                    return Err(StoreError::Invalid(format!(
                        "{} is not empty and is not a fleet store; refusing to use it",
                        self.root.display()
                    )));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.root).map_err(|e| io_unknown(&self.root, e))?;
            }
            Err(e) => return Err(io_unknown(&self.root, e)),
        }
        let _guard = self.lock()?;
        let path = self.root.join("store.json");
        if path.exists() {
            let bytes = fs::read(&path).map_err(|e| io_unknown(&path, e))?;
            return self.check_marker(&bytes);
        }
        for dir in [self.tmp_dir(), self.root.join("objects")] {
            fs::create_dir_all(&dir).map_err(|e| io_unknown(&dir, e))?;
        }
        let marker = serde_json::to_vec_pretty(&Marker {
            format: FORMAT.into(),
            version: FORMAT_VERSION,
        })
        .map_err(|e| StoreError::Invalid(e.to_string()))?;
        durable::create_exclusive(&self.tmp_dir(), &path, &marker)
            .map_err(|e| io_unknown(&path, e))?;
        Ok(())
    }

    /// A temp file present while nobody holds the lock belongs to a writer
    /// that died before linking it; it was never visible, so it goes.
    fn clean_temp(&self) -> Result<(), StoreError> {
        let _guard = self.lock()?;
        let dir = self.tmp_dir();
        let entries = fs::read_dir(&dir).map_err(|e| io_unknown(&dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_unknown(&dir, e))?;
            durable::remove(&entry.path()).map_err(|e| io_unknown(&entry.path(), e))?;
        }
        let private = self.root.join("launches");
        if let Ok(meta) = fs::symlink_metadata(&private) {
            if !meta.is_dir() {
                return Err(StoreError::Unknown(
                    "private launch directory is not a directory".into(),
                ));
            }
            for entry in fs::read_dir(&private)
                .map_err(|_| StoreError::Unknown("private launch cleanup unavailable".into()))?
            {
                let path = entry
                    .map_err(|_| StoreError::Unknown("private launch cleanup unavailable".into()))?
                    .path();
                if path.extension().is_some_and(|s| s == "tmp") {
                    durable::remove(&path)
                        .map_err(|_| StoreError::Unknown("private launch cleanup failed".into()))?;
                }
            }
        }
        Ok(())
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    fn kind_dir(&self, kind: &str) -> PathBuf {
        self.root.join("objects").join(kind)
    }

    fn object_path(&self, kind: &str, name: &str) -> PathBuf {
        self.kind_dir(kind).join(format!("{name}.json"))
    }

    fn lock(&self) -> Result<LockGuard, StoreError> {
        let path = self.root.join("lock");
        LockGuard::acquire(&path).map_err(|e| io_unknown(&path, e))
    }

    fn now(&self) -> Millis {
        self.clock.now()
    }

    fn next_rv(&self) -> Result<u64, StoreError> {
        let path = self.root.join("seq");
        let current = match fs::read_to_string(&path) {
            Ok(s) => s
                .trim()
                .parse::<u64>()
                .map_err(|e| StoreError::Unknown(format!("{}: corrupt: {e}", path.display())))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(io_unknown(&path, e)),
        };
        let next = current
            .checked_add(1)
            .ok_or_else(|| StoreError::Invalid("resourceVersion exhausted".into()))?;
        durable::replace(&self.tmp_dir(), &path, next.to_string().as_bytes())
            .map_err(|e| io_unknown(&path, e))?;
        Ok(next)
    }

    fn read(&self, kind: &str, name: &str) -> Observation<Option<Object>> {
        if let Err(e) = self.read_kind(kind).and_then(|()| validate_name(name)) {
            return Observation::unknown(e.to_string());
        }
        read_object(self, &self.object_path(kind, name), kind, name)
    }

    /// Read under the lock for a write: Unknown is an error, absence NotFound.
    fn read_existing(&self, kind: &str, name: &str) -> Result<Object, StoreError> {
        match self.read(kind, name) {
            Observation::Known(Some(o)) => Ok(o),
            Observation::Known(None) => Err(StoreError::NotFound(format!("{kind}/{name}"))),
            Observation::Unknown { why } => Err(StoreError::Unknown(why)),
        }
    }

    fn write(&self, obj: &Object) -> Result<(), StoreError> {
        let path = self.object_path(&obj.kind, &obj.metadata.name);
        let bytes = encode(obj)?;
        durable::replace(&self.tmp_dir(), &path, &bytes).map_err(|e| io_unknown(&path, e))
    }

    fn check_lease(&self, lease: &LeaseToken) -> Result<(), StoreError> {
        match self.read_lease()? {
            Some(l)
                if l.holder == lease.holder
                    && l.token == lease.token
                    && self.now() < l.expires_at =>
            {
                Ok(())
            }
            Some(l) => Err(StoreError::LeaseLost(format!(
                "{} token {} no longer holds the lease (now {} token {})",
                lease.holder, lease.token, l.holder, l.token
            ))),
            None => Err(StoreError::LeaseLost(format!(
                "{} token {}: no lease is held",
                lease.holder, lease.token
            ))),
        }
    }

    fn read_lease(&self) -> Result<Option<LeaseToken>, StoreError> {
        let path = self.root.join("lease.json");
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                StoreError::Unknown(format!("{}: corrupt lease: {e}", path.display()))
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_unknown(&path, e)),
        }
    }

    fn write_lease(&self, lease: &LeaseToken) -> Result<(), StoreError> {
        let path = self.root.join("lease.json");
        let bytes =
            serde_json::to_vec_pretty(lease).map_err(|e| StoreError::Invalid(e.to_string()))?;
        durable::replace(&self.tmp_dir(), &path, &bytes).map_err(|e| io_unknown(&path, e))
    }

    /// Mark `obj` deleted; it stays while a finalizer holds it.
    fn mark_deleted(&self, mut obj: Object) -> Result<DeleteOutcome, StoreError> {
        if obj.metadata.deletion_timestamp.is_none() {
            obj.metadata.deletion_timestamp = Some(self.now());
            obj.metadata.resource_version = self.next_rv()?;
            self.write(&obj)?;
        }
        self.remove_if_done(&obj)
    }

    /// An object marked for deletion goes once it has no finalizer.
    fn remove_if_done(&self, obj: &Object) -> Result<DeleteOutcome, StoreError> {
        if !obj.is_deleting() || !obj.metadata.finalizers.is_empty() {
            return Ok(DeleteOutcome::Marked);
        }
        let path = self.object_path(&obj.kind, &obj.metadata.name);
        if obj.kind == task::KIND {
            let launch = self
                .root
                .join("launches")
                .join(format!("{}.json", obj.metadata.uid));
            durable::remove(&launch)
                .map_err(|_| StoreError::Unknown("private launch removal failed".into()))?;
        }
        durable::remove(&path).map_err(|e| io_unknown(&path, e))?;
        Ok(DeleteOutcome::Removed)
    }
}

fn read_object(
    store: &FileStore,
    path: &Path,
    kind: &str,
    name: &str,
) -> Observation<Option<Object>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Observation::Known(None),
        Err(e) => return Observation::unknown(format!("inaccessible {}: {e}", path.display())),
    };
    match serde_json::from_slice::<Object>(&bytes) {
        Ok(o) if o.kind == kind && o.metadata.name == name => {
            if o.metadata.uid.is_empty()
                || o.metadata.resource_version == 0
                || o.metadata.generation == 0
                || o.metadata.creation_timestamp.is_none()
                || (kind == task::KIND
                    && !o.is_deleting()
                    && !o
                        .metadata
                        .finalizers
                        .iter()
                        .any(|f| f == task::RELEASE_FINALIZER))
                || store.validate_read(&o).is_err()
                || store.validate_plugin_status(kind, &o.status).is_err()
            {
                Observation::unknown(format!(
                    "corrupt {}: invalid persisted resource metadata or status",
                    path.display()
                ))
            } else {
                Observation::Known(Some(o))
            }
        }
        Ok(o) => Observation::unknown(format!("corrupt {}: holds {}", path.display(), o.key())),
        Err(e) => Observation::unknown(format!("corrupt {}: {e}", path.display())),
    }
}

fn encode(obj: &Object) -> Result<Vec<u8>, StoreError> {
    let mut bytes =
        serde_json::to_vec_pretty(obj).map_err(|e| StoreError::Invalid(e.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn io_unknown(path: &Path, e: io::Error) -> StoreError {
    StoreError::Unknown(format!("{}: {e}", path.display()))
}

/// `by` may change only the status fields it owns; a field nobody owns
/// (`Task.status.inbox`) may not change through a status write at all.
fn check_owned(kind: &str, old: &Value, new: &Value, by: FieldManager) -> Result<(), StoreError> {
    let empty = serde_json::Map::new();
    let old = old.as_object().unwrap_or(&empty);
    let new = new.as_object().unwrap_or(&empty);
    for field in old.keys().chain(new.keys()) {
        if old.get(field) != new.get(field) && kinds::status_owner(kind, field) != Some(by.clone())
        {
            return Err(StoreError::Forbidden(format!(
                "{} may not write status.{field} of {kind}",
                by.name()
            )));
        }
    }
    Ok(())
}

fn task_status(obj: &Object) -> Result<TaskStatus, StoreError> {
    if obj.status.is_null() {
        return Ok(TaskStatus::default());
    }
    serde_json::from_value(obj.status.clone())
        .map_err(|e| StoreError::Invalid(format!("{}: status: {e}", obj.key())))
}

fn encode_status(st: &TaskStatus) -> Result<Value, StoreError> {
    serde_json::to_value(st).map_err(|e| StoreError::Invalid(format!("Task status: {e}")))
}

fn conversation_status(obj: &Object) -> Result<ConversationStatus, StoreError> {
    if obj.status.is_null() {
        return Ok(ConversationStatus::default());
    }
    serde_json::from_value(obj.status.clone())
        .map_err(|e| StoreError::Invalid(format!("{}: status: {e}", obj.key())))
}

fn encode_conversation(st: &ConversationStatus) -> Result<Value, StoreError> {
    serde_json::to_value(st).map_err(|e| StoreError::Invalid(format!("Conversation status: {e}")))
}

fn listener_status(obj: &Object) -> Result<ListenerStatus, StoreError> {
    if obj.status.is_null() {
        return Ok(ListenerStatus::default());
    }
    serde_json::from_value(obj.status.clone())
        .map_err(|e| StoreError::Invalid(format!("{}: status: {e}", obj.key())))
}

fn encode_listener(st: &ListenerStatus) -> Result<Value, StoreError> {
    serde_json::to_value(st).map_err(|e| StoreError::Invalid(format!("Listener status: {e}")))
}

/// Append one entry under the inbox's checks, and record what it answers.
/// Every append path lands through here, so `answers` is checked and
/// recorded the same way for `fleet send`, a relay and a plugin.
fn push_entry(obj: &Object, st: &mut TaskStatus, entry: InboxEntry) -> Result<(), StoreError> {
    if entry.amends.is_some() {
        return Err(StoreError::Forbidden(format!(
            "{}: inbox entry {}: only fleet amend queues an amendment",
            obj.key(),
            entry.key
        )));
    }
    push_checked(obj, st, entry)
}

/// [`push_entry`] without the amendment guard: the amend mutation's own.
fn push_checked(obj: &Object, st: &mut TaskStatus, entry: InboxEntry) -> Result<(), StoreError> {
    fleet_api::inbox::check_append(&st.inbox, &st.deliveries, &entry)
        .map_err(|why| inbox_refusal(obj, why))?;
    talk::check_answers(&st.declared, &entry.answers)
        .map_err(|why| StoreError::Invalid(format!("{}: {why}", obj.key())))?;
    st.record_answers(&entry);
    st.inbox.push(entry);
    Ok(())
}

/// Each inbox appender names its source with its own prefix: `cli/<user>`
/// for `fleet send`, `forge/<change>` for the Forge controller,
/// `gateway/<conversation>` or `gateway/child/<child>` for the Gateway
/// controller.
fn check_source(by: FieldManager, entry: &InboxEntry) -> Result<(), StoreError> {
    let Some(prefix) = kinds::inbox_source_prefix(by.clone()) else {
        return Ok(());
    };
    if !entry.from.starts_with(prefix) || entry.from.len() == prefix.len() {
        return Err(StoreError::Invalid(format!(
            "an entry {} appends names its source as {prefix}<name>, not {:?}",
            by.name(),
            entry.from
        )));
    }
    Ok(())
}

/// A refused append: a repeated key is `AlreadyExists`, the rest Forbidden.
fn inbox_refusal(obj: &Object, why: String) -> StoreError {
    if why.ends_with("already exists") {
        StoreError::AlreadyExists(format!("{}: {why}", obj.key()))
    } else {
        StoreError::Forbidden(format!("{}: {why}", obj.key()))
    }
}

/// The Task status field a relay write replaces, decoded for its writer.
enum RelayField {
    Forge(Option<ForgeStatus>),
    Gateway(Option<Box<GatewayStatus>>),
}

/// A Task is created with its brief alone: only the amend mutation adds an
/// amendment, each creating a later generation.
fn check_no_amendments(obj: &Object) -> Result<(), StoreError> {
    let carries = obj
        .spec
        .get("amendments")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    if obj.kind == task::KIND && carries {
        return Err(StoreError::Invalid(format!(
            "{}: a new Task carries no spec.amendments; fleet amend adds them once it exists",
            obj.key()
        )));
    }
    Ok(())
}

/// The amend mutation's one write, for `fleet amend` and a plugin's
/// `amends[]` alike: append the amendment to `spec.amendments`, bump the
/// generation and queue its inbox entry. Refused on a deleting or Stalled
/// Task, for an output name already declared, and on a full inbox.
fn apply_amendment(
    obj: &mut Object,
    amendment: task::Amendment,
    entry: InboxEntry,
) -> Result<(), StoreError> {
    if amendment.from != entry.from {
        return Err(StoreError::Invalid(
            "an amendment and its inbox entry name one sender".into(),
        ));
    }
    if obj.is_deleting() {
        return Err(StoreError::Forbidden(format!(
            "{} is being deleted; nothing would deliver the amendment",
            obj.key()
        )));
    }
    let mut st = task_status(obj)?;
    if st.phase == task::Phase::Stalled {
        return Err(StoreError::Forbidden(format!(
            "{} is Stalled; nothing would deliver the amendment, so create a new Task",
            obj.key()
        )));
    }
    let generation = obj
        .metadata
        .generation
        .checked_add(1)
        .ok_or_else(|| StoreError::Invalid("generation exhausted".into()))?;
    let key = fleet_api::inbox::amend_key(&obj.metadata.name, &obj.metadata.uid, generation);
    if amendment.generation != generation || entry.amends != Some(generation) || entry.key != key {
        return Err(StoreError::Invalid(format!(
            "{}: an amendment creates generation {generation} under {key}",
            obj.key()
        )));
    }
    let spec: task::TaskSpec = fleet_api::validate::typed(task::KIND, &obj.spec)?;
    let declared = spec.outputs_at(u64::MAX);
    if let Some(name) = amendment.outputs.keys().find(|n| declared.contains_key(*n)) {
        return Err(StoreError::Invalid(format!(
            "{}: output {name:?} is already declared; an amendment only adds outputs",
            obj.key()
        )));
    }
    // Appended to the stored JSON as is: which execution fields a spec
    // sets is itself meaningful, so the spec is never re-encoded.
    let amendment = serde_json::to_value(&amendment)
        .map_err(|e| StoreError::Invalid(format!("Task amendment: {e}")))?;
    let mut spec = obj.spec.clone();
    let Some(fields) = spec.as_object_mut() else {
        return Err(StoreError::Invalid(format!(
            "{}: spec is not an object",
            obj.key()
        )));
    };
    match fields
        .entry("amendments")
        .or_insert_with(|| Value::Array(Vec::new()))
    {
        Value::Array(list) => list.push(amendment),
        _ => {
            return Err(StoreError::Invalid(format!(
                "{}: spec.amendments",
                obj.key()
            )));
        }
    }
    fleet_api::validate::validate_spec(task::KIND, &spec)?;
    push_checked(obj, &mut st, entry)?;
    obj.spec = spec;
    obj.metadata.generation = generation;
    obj.status = encode_status(&st)?;
    Ok(())
}

fn expect_rv(obj: &Object, expected: u64) -> Result<(), StoreError> {
    if obj.metadata.resource_version != expected {
        return Err(StoreError::Conflict {
            key: obj.key(),
            expected,
            actual: obj.metadata.resource_version,
        });
    }
    Ok(())
}

impl Store for FileStore {
    fn plugin_mutate(
        &self,
        mutation: fleet_api::plugin::Mutation,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Value, StoreError> {
        self.mutate_plugin(mutation, by, lease)
    }

    fn served_kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = kinds::BUILT_IN.iter().map(|k| (*k).to_string()).collect();
        kinds.extend(self.plugins.keys().cloned());
        if let Ok(entries) = fs::read_dir(self.root.join("objects")) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && valid_plugin_kind(name)
                    && !kinds.iter().any(|k| k == name)
                {
                    kinds.push(name.into());
                }
            }
        }
        kinds
    }
    fn validate(&self, obj: &Object) -> Result<(), StoreError> {
        if !kinds::is_served(&obj.kind) && !self.plugins.contains_key(&obj.kind) {
            return Err(StoreError::Invalid(
                "unknown kind without configured plugin".into(),
            ));
        }

        if obj.metadata.labels.contains_key(fleet_api::plugin::CREATOR) {
            return Err(StoreError::Forbidden(
                "fleet.dev/plugin is assigned by the host".into(),
            ));
        }
        self.validate_read(obj)
    }
    fn validate_plugin_status(&self, kind: &str, status: &Value) -> Result<(), StoreError> {
        FileStore::validate_plugin_status(self, kind, status)
    }

    fn launch(&self, task: &Object) -> Result<Option<fleet_api::config::Execution>, StoreError> {
        validate_name(&task.metadata.uid)?;
        let path = self
            .root
            .join("launches")
            .join(format!("{}.json", task.metadata.uid));
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(StoreError::Unknown("private launch unavailable".into())),
            Ok(meta) if !meta.is_file() || meta.len() > LAUNCH_CAP as u64 => {
                return Err(StoreError::Unknown(
                    "private launch is not a bounded regular file".into(),
                ));
            }
            Ok(_) => {}
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in [&self.root.join("launches"), &path] {
                let meta = fs::symlink_metadata(p)
                    .map_err(|_| StoreError::Unknown("private launch unavailable".into()))?;
                if meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
                    return Err(StoreError::Forbidden(
                        "private launch permissions must restrict access to its owner".into(),
                    ));
                }
            }
        }
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| StoreError::Unknown("private launch is unreadable".into())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(StoreError::Unknown("private launch unavailable".into())),
        }
    }

    fn freeze_launch(
        &self,
        task: &Object,
        execution: &fleet_api::config::Execution,
        lease: &LeaseToken,
    ) -> Result<fleet_api::config::Execution, StoreError> {
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        let current = self.read_existing(task::KIND, &task.metadata.name)?;
        if current.metadata.uid != task.metadata.uid
            || current.metadata.resource_version != task.metadata.resource_version
            || current.is_deleting()
        {
            return Err(StoreError::Conflict {
                key: task.key(),
                expected: task.metadata.resource_version,
                actual: current.metadata.resource_version,
            });
        }
        if let Some(existing) = self.launch(task)? {
            return Ok(existing);
        }
        let dir = self.root.join("launches");
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let mut builder = builder;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(StoreError::Unknown(
                    "private launch directory unavailable".into(),
                ));
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::symlink_metadata(&dir)
                .map_err(|_| StoreError::Unknown("private launch directory unavailable".into()))?;
            if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
                return Err(StoreError::Forbidden(
                    "private launch directory must have mode 0700".into(),
                ));
            }
        }
        let path = dir.join(format!("{}.json", task.metadata.uid));
        let bytes = serde_json::to_vec(execution)
            .map_err(|_| StoreError::Invalid("private launch encoding failed".into()))?;
        if bytes.len() > LAUNCH_CAP {
            return Err(StoreError::Invalid("private launch exceeds 1 MiB".into()));
        }
        durable::create_exclusive(&dir, &path, &bytes)
            .map_err(|_| StoreError::Unknown("private launch write failed".into()))?;
        Ok(execution.clone())
    }

    fn get(&self, kind: &str, name: &str) -> Observation<Option<Object>> {
        self.read(kind, name)
    }

    fn list(&self, kind: &str) -> Observation<Vec<Object>> {
        if let Err(e) = self.read_kind(kind) {
            return Observation::unknown(e.to_string());
        }
        let dir = self.kind_dir(kind);
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Observation::Known(Vec::new()),
            Err(e) => return Observation::unknown(format!("inaccessible {}: {e}", dir.display())),
        };
        let mut objs = Vec::new();
        for entry in entries {
            let path = match entry {
                Ok(e) => e.path(),
                Err(e) => {
                    return Observation::unknown(format!("inaccessible {}: {e}", dir.display()));
                }
            };
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
            else {
                return Observation::unknown(format!(
                    "unexpected file {} in the store",
                    path.display()
                ));
            };
            match read_object(self, &path, kind, name) {
                Observation::Known(Some(o)) => objs.push(o),
                // Removed between read_dir and read: it is gone, which is a fact.
                Observation::Known(None) => {}
                Observation::Unknown { why } => return Observation::Unknown { why },
            }
        }
        objs.sort_by_key(|o| o.metadata.name.clone());
        Observation::Known(objs)
    }

    fn create(&self, obj: Object, by: FieldManager) -> Result<Object, StoreError> {
        if by.is_controller() || !kinds::may_create(&obj.kind, &by) {
            return Err(StoreError::Forbidden(format!(
                "{} does not create {} objects",
                by.name(),
                obj.kind
            )));
        }
        check_conversation_labels(&obj, &by)?;
        check_no_amendments(&obj)?;
        let _guard = self.lock()?;
        self.check_parent(&obj, &by)?;
        self.insert(obj, None)
    }

    fn create_fenced(
        &self,
        obj: Object,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        if !by.is_controller() || !kinds::may_create(&obj.kind, &by) {
            return Err(StoreError::Forbidden(format!(
                "{} does not create {} objects",
                by.name(),
                obj.kind
            )));
        }
        check_conversation_labels(&obj, &by)?;
        check_no_amendments(&obj)?;
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        self.insert(obj, None)
    }

    fn update_spec(
        &self,
        kind: &str,
        name: &str,
        spec: Value,
        labels: Option<BTreeMap<String, String>>,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        if by != FieldManager::Apply {
            return Err(StoreError::Forbidden(format!(
                "{} does not write spec",
                by.name()
            )));
        }
        let _guard = self.lock()?;
        let mut obj = self.read_existing(kind, name)?;
        expect_rv(&obj, expected)?;
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted",
                obj.key()
            )));
        }
        if let Some((_, def)) = self.plugins.get(kind) {
            schema(&def.spec, &spec)?;
            if def.immutable_spec && obj.spec != spec {
                return Err(StoreError::Invalid("plugin spec is immutable".into()));
            }
        } else {
            validate_spec_update(kind, &obj.spec, &spec)?;
        }
        let labels = if let Some(mut labels) = labels {
            if labels.contains_key(fleet_api::plugin::CREATOR) {
                return Err(StoreError::Forbidden(
                    "plugin creator label is assigned by the host".into(),
                ));
            }
            // The conversation labels are set by the one create that may,
            // and never change afterwards.
            for label in [
                conversation::CONVERSATION_LABEL,
                conversation::CONVERSATION_UID_LABEL,
            ] {
                if labels.get(label) != obj.metadata.labels.get(label) {
                    return Err(StoreError::Forbidden(format!(
                        "the label {label} is set only by the Conversation's controller when it creates the Task, and never changes"
                    )));
                }
            }
            if kind == task::KIND {
                talk::check_parent_unchanged(&obj.metadata.labels, &labels)
                    .map_err(|why| StoreError::Forbidden(format!("{}: {why}", obj.key())))?;
            }
            if let Some(creator) = obj.metadata.labels.get(fleet_api::plugin::CREATOR) {
                labels.insert(fleet_api::plugin::CREATOR.into(), creator.clone());
            }
            Some(labels)
        } else {
            None
        };
        let mut changed = false;
        if obj.spec != spec {
            obj.spec = spec;
            obj.metadata.generation = obj
                .metadata
                .generation
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid("generation exhausted".into()))?;
            changed = true;
        }
        if let Some(labels) = labels
            && labels != obj.metadata.labels
        {
            obj.metadata.labels = labels;
            changed = true;
        }
        if changed {
            obj.metadata.resource_version = self.next_rv()?;
            self.write(&obj)?;
        }
        Ok(obj)
    }

    fn update_status(
        &self,
        kind: &str,
        name: &str,
        status: Value,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        if self.controller_of(kind).is_none() {
            return Err(StoreError::Forbidden(format!("kind {kind} has no status")));
        }
        if !(status.is_object() || status.is_null()) {
            return Err(StoreError::Invalid("status must be an object".into()));
        }
        self.validate_plugin_status(kind, &status)?;
        let _guard = self.lock()?;
        if by.is_controller() {
            match lease {
                Some(l) => self.check_lease(l)?,
                None => {
                    return Err(StoreError::LeaseLost(format!(
                        "{} writes only under the manager lease",
                        by.name()
                    )));
                }
            }
        }
        let mut obj = self.read_existing(kind, name)?;
        expect_rv(&obj, expected)?;
        if obj.is_deleting() && !by.is_controller() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted",
                obj.key()
            )));
        }
        if let Some((owner, _)) = self.plugins.get(kind) {
            if by.name() != format!("plugin/{owner}") {
                return Err(StoreError::Forbidden("plugin status owner mismatch".into()));
            }
        } else {
            check_owned(kind, &obj.status, &status, by)?;
        }
        if obj.status == status {
            return Ok(obj);
        }
        obj.status = status;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(obj)
    }

    fn append_inbox(
        &self,
        name: &str,
        entry: InboxEntry,
        expected: u64,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Object, StoreError> {
        if !kinds::may_append_inbox(by.clone()) {
            return Err(StoreError::Forbidden(format!(
                "{} does not append to status.inbox",
                by.name()
            )));
        }
        check_source(by.clone(), &entry)?;
        let _guard = self.lock()?;
        if by.is_controller() {
            match lease {
                Some(l) => self.check_lease(l)?,
                None => {
                    return Err(StoreError::LeaseLost(format!(
                        "{} writes only under the manager lease",
                        by.name()
                    )));
                }
            }
        }
        let mut obj = self.read_existing(task::KIND, name)?;
        expect_rv(&obj, expected)?;
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted; nothing would deliver the entry",
                obj.key()
            )));
        }
        let mut st = task_status(&obj)?;
        push_entry(&obj, &mut st, entry)?;
        obj.status = encode_status(&st)?;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(obj)
    }

    fn amend_task(
        &self,
        name: &str,
        amendment: task::Amendment,
        entry: InboxEntry,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        if by != FieldManager::Cli {
            return Err(StoreError::Forbidden(format!(
                "{} does not amend Tasks; only fleet amend does",
                by.name()
            )));
        }
        check_source(by.clone(), &entry)?;
        let _guard = self.lock()?;
        let mut obj = self.read_existing(task::KIND, name)?;
        expect_rv(&obj, expected)?;
        apply_amendment(&mut obj, amendment, entry)?;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(obj)
    }

    fn record_relay(
        &self,
        name: &str,
        status: Value,
        appends: Vec<InboxEntry>,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        let Some(field) = kinds::relay_field(&by) else {
            return Err(StoreError::Forbidden(format!(
                "{} does not record relays; only the Forge and Gateway controllers do",
                by.name()
            )));
        };
        // The writer's own field, decoded as its readers would.
        let own = match field {
            "forge" if status.is_null() => RelayField::Forge(None),
            "forge" => RelayField::Forge(Some(
                serde_json::from_value(status)
                    .map_err(|e| StoreError::Invalid(format!("Task status.forge: {e}")))?,
            )),
            _ if status.is_null() => RelayField::Gateway(None),
            _ => RelayField::Gateway(Some(Box::new(
                serde_json::from_value(status)
                    .map_err(|e| StoreError::Invalid(format!("Task status.gateway: {e}")))?,
            ))),
        };
        for entry in &appends {
            check_source(by.clone(), entry)?;
        }
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        let mut obj = self.read_existing(task::KIND, name)?;
        expect_rv(&obj, expected)?;
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted; nothing of the {}'s remains to write",
                obj.key(),
                by.name()
            )));
        }
        let mut st = task_status(&obj)?;
        match own {
            RelayField::Forge(forge) => st.forge = forge.filter(|f| !f.is_empty()),
            RelayField::Gateway(gateway) => {
                st.gateway = gateway.map(|g| *g).filter(|g| !g.is_empty())
            }
        }
        for entry in appends {
            push_entry(&obj, &mut st, entry)?;
        }
        let status = encode_status(&st)?;
        if obj.status == status {
            return Ok(obj);
        }
        obj.status = status;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(obj)
    }

    fn record_delivery(
        &self,
        name: &str,
        status: Value,
        receipt: DeliveryReceipt,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        if !kinds::may_record_delivery(by.clone()) {
            return Err(StoreError::Forbidden(format!(
                "{} does not record deliveries; only {}'s controller does",
                by.name(),
                task::KIND
            )));
        }
        if !status.is_object() {
            return Err(StoreError::Invalid("status must be an object".into()));
        }
        validate_status(task::KIND, &status)?;
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        let mut obj = self.read_existing(task::KIND, name)?;
        expect_rv(&obj, expected)?;
        // The controller's own fields change as in update_status; inbox,
        // having no owner, must be passed back unchanged.
        check_owned(task::KIND, &obj.status, &status, by)?;
        let mut st: TaskStatus = serde_json::from_value(status)
            .map_err(|e| StoreError::Invalid(format!("Task status: {e}")))?;
        let Some(at) = st.inbox.iter().position(|e| e.key == receipt.key) else {
            return Err(StoreError::NotFound(format!(
                "{}: inbox entry {}",
                obj.key(),
                receipt.key
            )));
        };
        let entry = st.inbox.remove(at);
        st.deliveries.record(&entry, receipt);
        obj.status = encode_status(&st)?;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(obj)
    }

    fn delete(
        &self,
        kind: &str,
        name: &str,
        by: FieldManager,
    ) -> Result<DeleteOutcome, StoreError> {
        if by != FieldManager::Apply {
            return Err(StoreError::Forbidden(format!(
                "{} does not delete objects",
                by.name()
            )));
        }
        let _guard = self.lock()?;
        let obj = self.read_existing(kind, name)?;
        self.mark_deleted(obj)
    }

    fn delete_fenced(
        &self,
        kind: &str,
        name: &str,
        uid: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        if self.controller_of(kind) != Some(by.clone()) {
            return Err(StoreError::Forbidden(format!(
                "only {kind}'s controller deletes its own objects"
            )));
        }
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        let obj = self.read_existing(kind, name)?;
        // A recreated object under the same name has a new uid: the plan that
        // named the old one removes nothing.
        if obj.metadata.uid != uid {
            return Err(StoreError::Conflict {
                key: obj.key(),
                expected,
                actual: obj.metadata.resource_version,
            });
        }
        expect_rv(&obj, expected)?;
        self.mark_deleted(obj)
    }

    fn retire_conversation_task(
        &self,
        name: &str,
        uid: &str,
        conversation_uid: &str,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        if !kinds::may_retire_conversation_task(&by) {
            return Err(StoreError::Forbidden(format!(
                "{} does not retire conversation Tasks",
                by.name()
            )));
        }
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        let obj = self.read_existing(task::KIND, name)?;
        // A recreated Task under the same name has a new uid: the plan that
        // named the old one removes nothing.
        if obj.metadata.uid != uid {
            return Err(StoreError::Conflict {
                key: obj.key(),
                expected: 0,
                actual: obj.metadata.resource_version,
            });
        }
        if obj
            .metadata
            .labels
            .get(conversation::CONVERSATION_UID_LABEL)
            .map(String::as_str)
            != Some(conversation_uid)
        {
            return Err(StoreError::Forbidden(format!(
                "{} was not created for this thread",
                obj.key()
            )));
        }
        if obj.is_deleting() {
            return Ok(DeleteOutcome::Marked);
        }
        if obj.status["phase"].as_str() != Some("Concluded") {
            return Err(StoreError::Forbidden(format!(
                "{} is not Concluded; a thread whose Task runs is not retired",
                obj.key()
            )));
        }
        self.mark_deleted(obj)
    }

    fn receive_event(
        &self,
        name: &str,
        delivery: Delivery,
        expected: u64,
        by: FieldManager,
    ) -> Result<(Object, Spooled), StoreError> {
        let _guard = self.lock()?;
        let mut obj = self.read_existing(conversation::KIND, name)?;
        let account = obj.spec["account"].as_str().unwrap_or_default();
        if !kinds::may_spool(&by, account, delivery.verified) {
            return Err(StoreError::Forbidden(format!(
                "{} does not write {}; only listener/{account} does",
                by.name(),
                obj.key()
            )));
        }
        expect_rv(&obj, expected)?;
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted; nothing would number the event",
                obj.key()
            )));
        }
        let mut st = conversation_status(&obj)?;
        if st.has_seen(&delivery.id) {
            return Ok((obj, Spooled::Duplicate));
        }
        if st.received.len() >= conversation::RECEIVED_CAP {
            return Err(StoreError::Forbidden(format!(
                "{} holds {} unread events; its controller has not numbered them yet",
                obj.key(),
                conversation::RECEIVED_CAP
            )));
        }
        st.received.push(delivery);
        obj.status = encode_conversation(&st)?;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok((obj, Spooled::Written))
    }

    fn record_received(
        &self,
        name: &str,
        status: Value,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<Object, StoreError> {
        if !kinds::may_record_received(&by) {
            return Err(StoreError::Forbidden(format!(
                "{} does not drain a Conversation's spool; only its controller does",
                by.name()
            )));
        }
        if !status.is_object() {
            return Err(StoreError::Invalid("status must be an object".into()));
        }
        validate_status(conversation::KIND, &status)?;
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        let mut obj = self.read_existing(conversation::KIND, name)?;
        expect_rv(&obj, expected)?;
        // The controller's own fields change as in update_status; the
        // spool, having no owner, may only have been drained.
        let before = conversation_status(&obj)?;
        let after: ConversationStatus = serde_json::from_value(status.clone())
            .map_err(|e| StoreError::Invalid(format!("Conversation status: {e}")))?;
        let mut old = obj.status.clone();
        let mut new = status;
        for v in [&mut old, &mut new] {
            if let Some(m) = v.as_object_mut() {
                m.remove("received");
            }
        }
        check_owned(conversation::KIND, &old, &new, by)?;
        conversation::check_drained(&before.received, &after.received)
            .map_err(|why| StoreError::Forbidden(format!("{}: {why}", obj.key())))?;
        let status = encode_conversation(&after)?;
        if obj.status == status {
            return Ok(obj);
        }
        obj.status = status;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(obj)
    }

    fn record_beat(
        &self,
        name: &str,
        instance: &str,
        beat: Beat,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        if instance.trim().is_empty() || instance.len() > 64 {
            return Err(StoreError::Invalid(
                "a beat names its instance in at most 64 characters".into(),
            ));
        }
        let _guard = self.lock()?;
        let mut obj = self.read_existing(listener::KIND, name)?;
        self.check_listener_of(&obj, &by)?;
        expect_rv(&obj, expected)?;
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted",
                obj.key()
            )));
        }
        let mut st = listener_status(&obj)?;
        st.record_beat(instance, beat);
        obj.status = encode_listener(&st)?;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(obj)
    }

    fn record_refusal(
        &self,
        name: &str,
        reason: &str,
        fault: bool,
        at: Millis,
        expected: u64,
        by: FieldManager,
    ) -> Result<Object, StoreError> {
        if reason.trim().is_empty() || reason.len() > 64 {
            return Err(StoreError::Invalid(
                "a refusal names its reason in at most 64 characters".into(),
            ));
        }
        let _guard = self.lock()?;
        let mut obj = self.read_existing(listener::KIND, name)?;
        self.check_listener_of(&obj, &by)?;
        expect_rv(&obj, expected)?;
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted",
                obj.key()
            )));
        }
        let mut st = listener_status(&obj)?;
        st.record_refusal(reason, fault, at);
        obj.status = encode_listener(&st)?;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(obj)
    }

    fn remove_finalizer(
        &self,
        kind: &str,
        name: &str,
        finalizer: &str,
        expected: u64,
        by: FieldManager,
        lease: &LeaseToken,
    ) -> Result<DeleteOutcome, StoreError> {
        if self.controller_of(kind) != Some(by.clone()) {
            return Err(StoreError::Forbidden(format!(
                "only {kind}'s controller removes its finalizers"
            )));
        }
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        let mut obj = self.read_existing(kind, name)?;
        expect_rv(&obj, expected)?;
        // A finalizer comes off only when its work is done, which is only
        // after the object was marked for deletion. Removed from a live
        // object it would let a later delete skip the release entirely.
        if !obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is not being deleted; its finalizers stay",
                obj.key()
            )));
        }
        if !obj.metadata.finalizers.iter().any(|f| f == finalizer) {
            // The finalizer write may have survived a crash before unlink.
            // Ownership, CAS, lease and deletion checks still fence cleanup.
            return self.remove_if_done(&obj);
        }
        obj.metadata.finalizers.retain(|f| f != finalizer);
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        self.remove_if_done(&obj)
    }

    fn acquire_lease(&self, holder: &str, ttl: Millis) -> Result<LeaseToken, StoreError> {
        let _guard = self.lock()?;
        let now = self.now();
        let current = self.read_lease()?;
        // Held until it expires, even against the same holder name: two
        // managers started with one --holder must not both hold it.
        if let Some(l) = &current
            && now < l.expires_at
        {
            return Err(StoreError::LeaseHeld {
                holder: l.holder.clone(),
                expires_at: l.expires_at,
            });
        }
        let token = current.map_or(1, |l| l.token + 1);
        let lease = LeaseToken {
            holder: holder.to_string(),
            token,
            expires_at: now.saturating_add(ttl),
        };
        self.write_lease(&lease)?;
        Ok(lease)
    }

    fn renew_lease(&self, lease: &LeaseToken, ttl: Millis) -> Result<LeaseToken, StoreError> {
        let _guard = self.lock()?;
        self.check_lease(lease)?;
        let renewed = LeaseToken {
            expires_at: self.now().saturating_add(ttl),
            ..lease.clone()
        };
        self.write_lease(&renewed)?;
        Ok(renewed)
    }

    fn release_lease(&self, lease: &LeaseToken) -> Result<(), StoreError> {
        let _guard = self.lock()?;
        if let Some(l) = self.read_lease()?
            && l.holder == lease.holder
            && l.token == lease.token
        {
            // Keep the token so the next holder's is larger: expire, never erase.
            let expired = LeaseToken { expires_at: 0, ..l };
            self.write_lease(&expired)?;
        }
        Ok(())
    }
}

impl FileStore {
    /// A Task being created may name a parent only with both labels, only
    /// from a writer that may already append to the parent's inbox, and
    /// only for a parent that exists with that `uid` and is not being
    /// deleted. Called under the store lock, before the insert: a parent
    /// the store cannot read is Unknown, to be retried, never a refusal.
    fn check_parent(&self, obj: &Object, by: &FieldManager) -> Result<(), StoreError> {
        if obj.kind != task::KIND {
            return Ok(());
        }
        let Some((name, uid)) = talk::parent_of(&obj.metadata.labels)
            .map_err(|why| StoreError::Invalid(format!("{}: {why}", obj.key())))?
        else {
            return Ok(());
        };
        let parent = match self.read(task::KIND, &name) {
            Observation::Known(Some(p)) => p,
            Observation::Known(None) => {
                return Err(StoreError::Invalid(format!(
                    "{}: the parent Task/{name} does not exist",
                    obj.key()
                )));
            }
            Observation::Unknown { why } => {
                return Err(StoreError::Unknown(format!(
                    "{}: the parent Task/{name} could not be read: {why}",
                    obj.key()
                )));
            }
        };
        if parent.metadata.uid != uid {
            return Err(StoreError::Invalid(format!(
                "{}: the parent Task/{name} has uid {}, not {uid}",
                obj.key(),
                parent.metadata.uid
            )));
        }
        if parent.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{}: the parent Task/{name} is being deleted; nothing would read its inbox",
                obj.key()
            )));
        }
        let access = by
            .name()
            .strip_prefix("plugin/")
            .and_then(|plugin| self.plugin_rights.get(plugin).copied());
        if !talk::may_name_parent(by, access, &parent) {
            return Err(StoreError::Forbidden(format!(
                "{}: {} may not name Task/{name} as a parent; only a writer that may append to its inbox does",
                obj.key(),
                by.name()
            )));
        }
        Ok(())
    }

    /// The writer is the listener of the account the object's spec names.
    fn check_listener_of(&self, obj: &Object, by: &FieldManager) -> Result<(), StoreError> {
        let account = obj.spec["account"].as_str().unwrap_or_default();
        if !kinds::is_listener_of(by, account) {
            return Err(StoreError::Forbidden(format!(
                "{} does not write {}; only listener/{account} does",
                by.name(),
                obj.key()
            )));
        }
        Ok(())
    }

    /// Create under the store lock the caller holds: validation, exclusive
    /// name, live owners, then the store's own metadata.
    fn insert(&self, mut obj: Object, creator: Option<&str>) -> Result<Object, StoreError> {
        self.validate(&obj)?;
        let m = &obj.metadata;
        if !m.uid.is_empty()
            || m.generation != 0
            || m.resource_version != 0
            || !m.finalizers.is_empty()
            || m.creation_timestamp.is_some()
            || m.deletion_timestamp.is_some()
        {
            return Err(StoreError::Invalid(
                "metadata.uid, generation, resourceVersion, finalizers and timestamps are assigned by the store".into(),
            ));
        }
        if !obj.status.is_null() {
            return Err(StoreError::Invalid(
                "status is written by controllers, not on create".into(),
            ));
        }
        if let Some(creator) = creator {
            obj.metadata
                .labels
                .insert(fleet_api::plugin::CREATOR.into(), creator.into());
        }
        let key = obj.key();
        match self.read(&obj.kind, &obj.metadata.name) {
            Observation::Known(None) => {}
            Observation::Known(Some(_)) => return Err(StoreError::AlreadyExists(key)),
            Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
        }
        // Nothing owns a Task in this build, so no object may name an owner.
        if !obj.metadata.owner_references.is_empty() {
            return Err(StoreError::Invalid(
                "metadata.ownerReferences is not served: no kind owns a Task".into(),
            ));
        }
        let rv = self.next_rv()?;
        obj.metadata.uid = new_uid(rv);
        obj.metadata.generation = 1;
        obj.metadata.resource_version = rv;
        obj.metadata.finalizers = self
            .plugins
            .get(&obj.kind)
            .filter(|(_, k)| k.finalizer)
            .map(|(p, _)| vec![format!("fleet.dev/plugin/{p}")])
            .unwrap_or_else(|| kinds::finalizers_on_create(&obj.kind));
        obj.metadata.creation_timestamp = Some(self.now());
        let dir = self.kind_dir(&obj.kind);
        fs::create_dir_all(&dir).map_err(|e| io_unknown(&dir, e))?;
        let path = self.object_path(&obj.kind, &obj.metadata.name);
        let created = durable::create_exclusive(&self.tmp_dir(), &path, &encode(&obj)?)
            .map_err(|e| io_unknown(&path, e))?;
        if !created {
            return Err(StoreError::AlreadyExists(key));
        }
        Ok(obj)
    }
}

/// The `gateway.fleet.dev/conversation` labels name the thread a Task was
/// created for, so only that thread's controller may set them.
fn check_conversation_labels(obj: &Object, by: &FieldManager) -> Result<(), StoreError> {
    for label in obj.metadata.labels.keys() {
        if conversation::is_controller_label(label) && !kinds::may_set_conversation_labels(by) {
            return Err(StoreError::Forbidden(format!(
                "{}: the label {label} is set only by the Conversation's controller when it creates the Task",
                obj.key()
            )));
        }
    }
    Ok(())
}

/// Unique beyond this store: the sequence number orders, the random part keeps
/// two stores (a re-initialised data directory, a trial) from ever handing
/// out the same uid, which runtimes key sessions by.
fn new_uid(rv: u64) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(rv);
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    h.write_u32(std::process::id());
    format!("u{rv}-{:016x}", h.finish())
}

fn valid_plugin_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind.starts_with(|c: char| c.is_ascii_uppercase())
        && kind.chars().all(|c| c.is_ascii_alphanumeric())
}
fn schema(schema: &Value, value: &Value) -> Result<(), StoreError> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|_| StoreError::Invalid("invalid plugin JSON Schema".into()))?;
    validator
        .validate(value)
        .map_err(|_| StoreError::Invalid("plugin value fails JSON Schema".into()))
}
impl FileStore {
    pub fn with_plugins(
        mut self,
        registrations: &[fleet_api::plugin::Registration],
    ) -> Result<Self, StoreError> {
        let mut seen = std::collections::BTreeSet::new();
        for registration in registrations {
            self.plugin_rights.insert(
                registration.manifest.name.clone(),
                registration.config.tasks,
            );
            fleet_api::plugin::validate_manifest(
                &registration.manifest,
                &registration.manifest.name,
            )?;
            for kind in &registration.manifest.kinds {
                if fleet_api::plugin::kind_aliases(&kind.kind)
                    .iter()
                    .any(|alias| seen.contains(alias))
                {
                    return Err(StoreError::Invalid("duplicate plugin kind".into()));
                }
                seen.extend(fleet_api::plugin::kind_aliases(&kind.kind));
                jsonschema::validator_for(&kind.spec)
                    .map_err(|_| StoreError::Invalid("invalid spec schema".into()))?;
                if let Some(status) = &kind.status {
                    jsonschema::validator_for(status)
                        .map_err(|_| StoreError::Invalid("invalid status schema".into()))?;
                }
                self.plugins.insert(
                    kind.kind.clone(),
                    (registration.manifest.name.clone(), kind.clone()),
                );
            }
        }
        Ok(self)
    }
    fn read_kind(&self, kind: &str) -> Result<(), StoreError> {
        if self.plugins.contains_key(kind)
            || (valid_plugin_kind(kind) && self.kind_dir(kind).is_dir())
        {
            Ok(())
        } else {
            validate_kind(kind)
        }
    }
    fn controller_of(&self, kind: &str) -> Option<FieldManager> {
        self.plugins
            .get(kind)
            .map(|(name, _)| plugin_writer(name))
            .or_else(|| kinds::controller_of(kind))
    }
    fn validate_read(&self, obj: &Object) -> Result<(), StoreError> {
        if kinds::is_served(&obj.kind) {
            return validate_object(obj);
        }
        self.read_kind(&obj.kind)?;
        validate_name(&obj.metadata.name)?;
        if obj.api_version != fleet_api::API_VERSION || !obj.metadata.owner_references.is_empty() {
            return Err(StoreError::Invalid("plugin envelope".into()));
        }
        if let Some((_, kind)) = self.plugins.get(&obj.kind) {
            schema(&kind.spec, &obj.spec)?;
        }
        Ok(())
    }
    fn validate_plugin_status(&self, kind: &str, status: &Value) -> Result<(), StoreError> {
        if kinds::is_served(kind) {
            return validate_status(kind, status);
        }
        if status.is_null() {
            return Ok(());
        }
        let mut own = status
            .as_object()
            .ok_or_else(|| StoreError::Invalid("plugin status must be object".into()))?
            .clone();
        let host_only = own
            .keys()
            .any(|k| fleet_api::plugin::RESERVED.contains(&k.as_str()));
        for reserved in fleet_api::plugin::RESERVED {
            own.remove(reserved);
        }
        if status.get("notBefore").is_some_and(|v| !v.is_u64())
            || status.get("host").is_some_and(|v| !v.is_object())
            || status.get("effects").is_some_and(|v| !v.is_object())
        {
            return Err(StoreError::Invalid(
                "malformed reserved plugin status".into(),
            ));
        }
        if host_only && own.is_empty() {
            return Ok(());
        }
        if let Some((_, def)) = self.plugins.get(kind)
            && let Some(s) = &def.status
        {
            schema(s, &Value::Object(own))?;
        }
        Ok(())
    }
}

fn plugin_writer(name: &str) -> FieldManager {
    fleet_api::plugin::writer_for_store(name)
}

#[cfg(test)]
mod initialization_tests {
    use super::*;

    #[test]
    fn initialization_rechecks_a_marker_created_by_another_writer() {
        // open() can observe no marker before another initializer gets the
        // lock. init() must recheck the winner's format while holding it.
        let root = std::env::temp_dir().join(format!("fleet-init-marker-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let marker = root.join("store.json");
        let legacy = br#"{"format":"fleet-store","version":2}"#;
        fs::write(&marker, legacy).unwrap();
        let store = FileStore {
            root: root.clone(),
            clock: Arc::new(SystemClock),
            plugins: BTreeMap::new(),
            plugin_rights: BTreeMap::new(),
        };
        assert!(matches!(store.init(), Err(StoreError::Unsupported(_))));
        assert_eq!(fs::read(&marker).unwrap(), legacy);
        fs::remove_dir_all(root).unwrap();
    }
}
