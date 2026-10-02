//! Apply local control requests on the event loop that owns App and Lua.

use serde_json::{json, Value};
use thurbox::ui_control::{Reply, Request};

use crate::App;

impl App {
    pub(crate) fn serve_ui_control(&mut self) {
        while let Some(pending) = self
            .control
            .as_ref()
            .and_then(|control| control.pending.try_recv().ok())
        {
            if std::time::Instant::now() > pending.deadline {
                continue;
            }
            let result = match pending.request {
                Request::Ping => json!({"ok": true}),
                Request::State => self.control_state(),
                Request::Action { name, args } => match self.control_action(&name, &args) {
                    Ok(()) => {
                        self.note_input();
                        json!({"ok": true, "state": self.control_state()})
                    }
                    Err((code, message)) => {
                        json!({"ok": false, "error": {"code": code, "message": message}})
                    }
                },
            };
            let _ = pending.reply.send(Reply {
                instance_id: self
                    .control
                    .as_ref()
                    .expect("control server")
                    .instance
                    .id
                    .clone(),
                request_id: pending.request_id,
                revision: self.control_revision,
                result,
            });
        }
    }

    fn control_state(&mut self) -> Value {
        let pane = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .map(|plugin| plugin.name.clone());
        let selected = self.host.shared_string("selected");
        let query = self.host.shared_string("search.query");
        let observed = (pane.clone(), selected.clone(), query.clone());
        if self.control_observed.as_ref() != Some(&observed) {
            self.control_revision = self.control_revision.wrapping_add(1);
            self.control_observed = Some(observed);
        }
        json!({
            "instance_id": self.control.as_ref().expect("control server").instance.id,
            "revision": self.control_revision,
            "focused_pane": pane,
            "selected_session": selected,
            "search_query": query,
        })
    }

    fn control_action(&mut self, name: &str, args: &Value) -> Result<(), (&'static str, String)> {
        let object = args
            .as_object()
            .ok_or(("invalid_arguments", "arguments must be an object".into()))?;
        match name {
            "session.focus" => {
                if object.len() != 1 {
                    return Err((
                        "invalid_arguments",
                        "session.focus needs only session_id".into(),
                    ));
                }
                let id = object
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or(("invalid_arguments", "session_id must be a string".into()))?;
                if uuid::Uuid::parse_str(id).is_err() {
                    return Err(("invalid_arguments", "session_id must be a UUID".into()));
                }
                if self.snapshots.current().session(id).is_none() {
                    return Err((
                        "session_not_found",
                        "session is not in this interface".into(),
                    ));
                }
                let agent = self
                    .host
                    .index_of("agent")
                    .ok_or(("unavailable", "agent pane is not loaded".into()))?;
                if !self.host.focusable().contains(&agent) {
                    return Err(("unavailable", "agent pane cannot take focus".into()));
                }
                self.host.set_shared_string("selected", id);
                self.focus_on_session(id);
                Ok(())
            }
            "search.open" => {
                if object.len() != 1 {
                    return Err(("invalid_arguments", "search.open needs only query".into()));
                }
                let query = object
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or(("invalid_arguments", "query must be a string".into()))?;
                if query.len() > 4096 {
                    return Err(("invalid_arguments", "query is too long".into()));
                }
                let index = self
                    .host
                    .index_of("search")
                    .ok_or(("unavailable", "search plugin is not loaded".into()))?;
                let handled = self
                    .host
                    .on_action_with_args(index, name, &[("query", query)])
                    .map_err(|e| ("action_failed", e.to_string()))?;
                if !handled {
                    return Err(("unavailable", "search plugin declined the action".into()));
                }
                if let Some(position) = self
                    .host
                    .focusable()
                    .iter()
                    .position(|candidate| *candidate == index)
                {
                    self.focus = position;
                }
                Ok(())
            }
            _ => Err(("unknown_action", "unknown UI action".into())),
        }
    }
}
