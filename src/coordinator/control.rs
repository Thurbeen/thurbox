//! Apply local control requests on the event loop that owns App and Lua.

use serde_json::{json, Value};
use thurbox::ui_control::{Reply, Request};

use crate::App;

impl App {
    fn control_event(&mut self, kind: &str, value: Value) {
        self.control_revision += 1;
        self.control_events
            .push_back(json!({"revision": self.control_revision, "kind": kind, "value": value}));
        if self.control_events.len() > 256 {
            self.control_events.pop_front();
        }
    }

    pub(crate) fn refresh_control_state(&mut self, force: bool) {
        if self.control.is_none() {
            return;
        }
        let state_version = self.host.ui_state_version();
        let modal_marker = (
            self.modals.kind(),
            self.modals.selection(),
            self.modals.palette_query().map(str::to_owned),
        );
        if self.control_observed.is_some()
            && !force
            && !self.input_dirty
            && self.control_state_version == state_version
            && self.control_registry_version == self.registry.version()
            && self.control_placed == self.last_placed
            && self.control_floats == self.drawn_floats
            && self.control_focus == self.focus
            && self.control_modal == modal_marker
        {
            return;
        }
        self.control_state_version = state_version;
        self.control_registry_version = self.registry.version();
        self.control_placed.clone_from(&self.last_placed);
        self.control_floats.clone_from(&self.drawn_floats);
        self.control_focus = self.focus;
        self.control_modal = modal_marker;
        let focused_plugin = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index));
        let focused = focused_plugin.map(|plugin| plugin.name.clone());
        let focused_id = focused_plugin.map(|plugin| plugin.path.clone());
        let plugin_states = self.host.ui_states();
        let slots: Vec<Value> = self
            .last_placed
            .iter()
            .map(|placed| {
                let members = self.host.in_slot(&placed.slot);
                let panes: Vec<String> = members
                    .iter()
                    .map(|index| self.host.plugins[*index].name.clone())
                    .collect();
                let pane_ids: Vec<String> = members
                    .iter()
                    .map(|index| self.host.plugins[*index].path.clone())
                    .collect();
                let visible_indices: Vec<usize> = match self.host.slot_mode(&placed.slot) {
                    thurbox::kernel::layout::SlotMode::Stack => (0..members.len()).collect(),
                    thurbox::kernel::layout::SlotMode::Switch => {
                        let selection = members
                            .iter()
                            .position(|index| self.host.focusable().get(self.focus) == Some(index))
                            .unwrap_or_else(|| {
                                self.slot_selection
                                    .get(&placed.slot)
                                    .copied()
                                    .unwrap_or(0)
                                    .min(members.len().saturating_sub(1))
                            });
                        (selection < members.len())
                            .then_some(selection)
                            .into_iter()
                            .collect()
                    }
                };
                let visible: Vec<String> = visible_indices
                    .iter()
                    .map(|index| panes[*index].clone())
                    .collect();
                let visible_ids: Vec<String> = visible_indices
                    .iter()
                    .map(|index| pane_ids[*index].clone())
                    .collect();
                json!({
                    "slot": placed.slot,
                    "panes": panes,
                    "pane_ids": pane_ids,
                    "visible_panes": visible,
                    "visible_pane_ids": visible_ids,
                    "rect": {
                        "x": placed.rect.x, "y": placed.rect.y,
                        "width": placed.rect.width, "height": placed.rect.height,
                    },
                    "shown": true,
                })
            })
            .collect();
        let modal = self.modals.kind().map(|kind| {
            json!({
                "kind": kind.action().split('.').next().unwrap_or(""),
                "selection": self.modals.selection(),
                "query": self.modals.palette_query(),
            })
        });
        let mut overlays: Vec<Value> = self
            .drawn_floats
            .iter()
            .filter_map(|index| {
                self.host.plugins.get(*index).map(|plugin| {
                    json!({
                        "name": plugin.name,
                        "id": plugin.path,
                        "state": plugin_states.get(&plugin.path),
                    })
                })
            })
            .collect();
        overlays.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let search_query = self.host.shared_string("search.query");
        let search_state = self
            .host
            .plugins
            .iter()
            .find(|plugin| plugin.name == "search")
            .and_then(|plugin| plugin_states.get(&plugin.path));
        let search = json!({
            "query": search_query,
            "selected_result": search_state.and_then(|s| s.get("selected_result")),
        });
        let current = json!({
            "focused_pane": focused,
            "focused_pane_id": focused_id,
            "selected_session": self.host.shared_string("selected"),
            "slots": slots,
            "panels": {
                "sessions": self.host.shared_bool("panels.sessions").unwrap_or(true),
                "search": self.host.shared_bool("panels.search").unwrap_or(false),
            },
            "modal": modal,
            "overlays": overlays,
            "plugin_state": plugin_states,
            "search": search,
            "search_query": search_query,
            "catalog_revision": self.registry.version(),
        });
        if self.control_observed.as_ref() == Some(&current) {
            return;
        }
        if let Some(previous) = self.control_observed.take() {
            for (field, kind) in [
                ("focused_pane_id", "focus.changed"),
                ("selected_session", "selection.changed"),
                ("slots", "layout.changed"),
                ("panels", "layout.changed"),
                ("modal", "overlay.changed"),
                ("overlays", "overlay.changed"),
                ("plugin_state", "selection.changed"),
                ("search", "search.changed"),
                ("catalog_revision", "catalog.changed"),
            ] {
                if previous[field] != current[field] {
                    let kind = if field == "modal" && previous[field].is_null() {
                        "overlay.opened"
                    } else if field == "modal" && current[field].is_null() {
                        "overlay.closed"
                    } else {
                        kind
                    };
                    self.control_event(kind, json!({"field": field, "value": current[field]}));
                }
            }
        } else {
            self.control_revision += 1;
        }
        self.control_observed = Some(current);
    }

    fn control_watch(&mut self, since: Option<u64>) -> Value {
        self.refresh_control_state(true);
        let Some(since) = since else {
            return json!({"kind": "snapshot", "revision": self.control_revision, "state": self.control_state(), "events": []});
        };
        let oldest = self
            .control_events
            .front()
            .and_then(|e| e["revision"].as_u64())
            .unwrap_or(self.control_revision + 1);
        if since < oldest.saturating_sub(1) || since > self.control_revision {
            return json!({"kind": "resync_required", "revision": self.control_revision, "state": self.control_state(), "events": []});
        }
        let mut events = Vec::new();
        let mut bytes = 0;
        for event in self
            .control_events
            .iter()
            .filter(|e| e["revision"].as_u64().unwrap_or(0) > since)
        {
            let size = serde_json::to_vec(event).map_or(0, |data| data.len());
            if events.len() == 16 || bytes + size > 12 * 1024 {
                break;
            }
            bytes += size;
            events.push(event.clone());
        }
        if events.is_empty() && self.control_revision > since {
            return json!({"kind": "resync_required", "revision": self.control_revision, "state": self.control_state(), "events": []});
        }
        let revision = events
            .last()
            .and_then(|event| event["revision"].as_u64())
            .unwrap_or(self.control_revision);
        json!({"kind": "delta", "revision": revision, "events": events})
    }

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
                Request::Watch { since } => self.control_watch(since),
                Request::Action { name, args } => {
                    match self.control_action(&name, &args) {
                        Ok(()) => {
                            self.note_input();
                            self.refresh_control_state(true);
                            self.control_event("action.completed", json!({"action": name, "request_id": &pending.request_id, "ok": true}));
                            json!({"ok": true, "state": self.control_state()})
                        }
                        Err((code, message)) => {
                            self.control_event(
                            "action.refused",
                            json!({"action": name, "request_id": &pending.request_id, "ok": false, "code": code}),
                        );
                            json!({"ok": false, "error": {"code": code, "message": message}})
                        }
                    }
                }
            };
            let revision = result["revision"].as_u64().unwrap_or(self.control_revision);
            let mut reply = Reply {
                instance_id: self
                    .control
                    .as_ref()
                    .expect("control server")
                    .instance
                    .id
                    .clone(),
                request_id: pending.request_id,
                revision,
                result,
            };
            if reply.exceeds_limit() {
                reply.result = json!({
                    "ok": false,
                    "error": {
                        "code": "state_too_large",
                        "message": "UI state exceeds local reply limit",
                    }
                });
            }
            let _ = pending.reply.send(reply);
        }
    }

    fn control_state(&mut self) -> Value {
        self.refresh_control_state(true);
        let mut state = self.control_observed.clone().unwrap_or_else(|| json!({}));
        state["instance_id"] = json!(self.control.as_ref().expect("control server").instance.id);
        state["revision"] = json!(self.control_revision);
        state
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
