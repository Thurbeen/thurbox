//! Read-side evidence, separate from state: neither output nor a transcript
//! declares completion. No transcript content is opened or parsed here.
use crate::{
    session::{AgentRegistry, SessionId},
    storage::Database,
    sync::SharedSession,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default)]
pub struct ReadFields {
    output: HashMap<String, HashMap<String, u64>>,
    transcripts: HashMap<SessionId, Option<PathBuf>>,
    rows: HashMap<SessionId, Option<SharedSession>>,
}
impl ReadFields {
    /// At most one metadata listing per local server per invocation/batch,
    /// including failures. In particular this never attaches or captures pane
    /// output, and a row on a host answers `null` rather than costing a round
    /// trip to it: every listing and watch batch would otherwise wait on it.
    pub fn annotate(
        &mut self,
        db: &Database,
        backends: Option<&crate::backend::BackendRegistry>,
        registry: &AgentRegistry,
        row: &SharedSession,
        value: &mut Value,
    ) {
        let local = !crate::session::Route::is_remote_key(&row.backend_type);
        let output = backends.filter(|_| local).and_then(|backends| {
            let server = super::server_key(&row.backend_type);
            let times = self.output.entry(server).or_insert_with(|| {
                super::windows::backend_for(backends, &row.backend_type)
                    .ok()
                    .and_then(|b| b.output_times().ok())
                    .unwrap_or_default()
            });
            times.get(&row.id.to_string()).copied()
        });
        value["last_output_at"] = json!(output);
        value["transcript_path"] = json!(self
            .transcripts
            .entry(row.id)
            .or_insert_with(|| transcript(db, registry, row)));
    }

    pub fn annotate_id(
        &mut self,
        db: &Database,
        backends: Option<&crate::backend::BackendRegistry>,
        registry: &AgentRegistry,
        id: SessionId,
        value: &mut Value,
    ) {
        value["last_output_at"] = Value::Null;
        value["transcript_path"] = Value::Null;
        // A batch holds up to 512 events, mostly about the same few sessions.
        let row = self
            .rows
            .entry(id)
            .or_insert_with(|| db.get_session_by_id(id).ok().flatten())
            .clone();
        if let Some(row) = row {
            self.annotate(db, backends, registry, &row, value);
        }
    }
}

fn transcript(db: &Database, registry: &AgentRegistry, row: &SharedSession) -> Option<PathBuf> {
    if crate::session::Route::is_remote_key(&row.backend_type) {
        return None;
    }
    let def = registry.get(&row.agent)?;
    if def.hook_schema.as_deref().unwrap_or(&def.name) != "claude" {
        return None;
    }
    let env = db.load_launch_env(row.id).ok()?;
    let dir = env.get("CLAUDE_CONFIG_DIR").map(PathBuf::from);
    crate::paths::claude_transcript_path(row.agent_session_id.as_deref()?, dir.as_deref())
}
