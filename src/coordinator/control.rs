//! Apply local control requests on the event loop that owns App and Lua.

use serde_json::{json, Value};
use thurbox::kernel::command::Command;
use thurbox::kernel::registry::ActionDescriptor;
use thurbox::ui_control::{InputOperation, Reply, Request};

use crate::{App, PendingConfirmation};

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
                Request::Actions => {
                    json!({"schema_version": 1, "actions": self.live_catalog()})
                }
                Request::Action { name, args } => {
                    let descriptor = self
                        .live_catalog()
                        .into_iter()
                        .find(|entry| entry.name == name);
                    let destructive = descriptor.as_ref().is_some_and(|entry| entry.destructive);
                    let audit_name = descriptor
                        .as_ref()
                        .map_or("unknown_action", |_| name.as_str());
                    let target = args
                        .get("session_id")
                        .and_then(Value::as_str)
                        .and_then(|id| uuid::Uuid::parse_str(id).ok())
                        .map(|id| id.to_string());
                    let instance = self
                        .control
                        .as_ref()
                        .expect("control server")
                        .instance
                        .id
                        .clone();
                    let audited = thurbox::ui_control::audit(
                        &instance,
                        pending.peer,
                        audit_name,
                        target.as_deref(),
                        "attempted",
                        &pending.request_id,
                    );
                    let attempt = if destructive && audited.is_err() {
                        Err((
                            "audit_unavailable",
                            "cannot record destructive action".into(),
                        ))
                    } else if destructive {
                        self.request_confirmation(&name, &args, pending.peer, &pending.request_id)
                    } else {
                        self.control_action(&name, &args)
                            .map(|()| json!({"ok": true, "state": self.control_state()}))
                    };
                    match attempt {
                        Ok(result) => {
                            let outcome = if result["ok"] == true {
                                "completed"
                            } else {
                                "confirmation_required"
                            };
                            let _ = thurbox::ui_control::audit(
                                &instance,
                                pending.peer,
                                audit_name,
                                target.as_deref(),
                                outcome,
                                &pending.request_id,
                            );
                            self.note_input();
                            self.refresh_control_state(true);
                            if result["ok"] == true {
                                self.control_event("action.completed", json!({"action": name, "request_id": &pending.request_id, "ok": true}));
                            }
                            result
                        }
                        Err((code, message)) => {
                            let _ = thurbox::ui_control::audit(
                                &instance,
                                pending.peer,
                                audit_name,
                                target.as_deref(),
                                code,
                                &pending.request_id,
                            );
                            self.control_event(
                            "action.refused",
                            json!({"action": audit_name, "request_id": &pending.request_id, "ok": false, "code": code}),
                        );
                            json!({"ok": false, "error": {"code": code, "message": message}})
                        }
                    }
                }
                Request::Confirm { ticket } => {
                    self.confirm_control_action(&ticket, pending.peer, &pending.request_id)
                }
                Request::Input { target, input } => match self.control_input(&target, input) {
                    Ok(()) => {
                        self.note_input();
                        json!({"ok": true, "state": self.control_state()})
                    }
                    Err((code, message)) => {
                        json!({"ok": false, "error": {"code": code, "message": message}})
                    }
                },
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

    fn request_confirmation(
        &mut self,
        name: &str,
        args: &Value,
        peer: u32,
        request_id: &str,
    ) -> Result<Value, (&'static str, String)> {
        let descriptor = self
            .live_catalog()
            .into_iter()
            .find(|entry| entry.name == name)
            .ok_or(("unknown_action", "unknown UI action".into()))?;
        if !matches!(
            name,
            "sessions.delete" | "sessions.force_delete" | "sessions.restart" | "sessions.sync"
        ) {
            return Err((
                "unavailable",
                "this destructive action has no guarded external executor".into(),
            ));
        }
        if !descriptor.available {
            return Err(("unavailable", "action is unavailable".into()));
        }
        let object = args
            .as_object()
            .ok_or(("invalid_arguments", "arguments must be an object".into()))?;
        if object.len() != 1 || !object.contains_key("session_id") {
            return Err((
                "invalid_arguments",
                "destructive action needs only session_id".into(),
            ));
        }
        let target = object["session_id"]
            .as_str()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or(("invalid_arguments", "session_id must be a UUID".into()))?
            .to_string();
        let row = self.snapshots.current().session(&target).ok_or((
            "stale_target",
            "session is no longer in this interface".into(),
        ))?;
        let instance = &self.control.as_ref().expect("control server").instance.id;
        thurbox::ui_control::audit(
            instance,
            peer,
            name,
            Some(&target),
            "confirmation_required",
            request_id,
        )
        .map_err(|_| {
            (
                "audit_unavailable",
                "cannot record destructive action".into(),
            )
        })?;
        self.control_tickets
            .retain(|_, pending| pending.expires > std::time::Instant::now());
        if self.control_tickets.len() >= 32 {
            return Err(("busy", "too many pending confirmations".into()));
        }
        let ticket = uuid::Uuid::new_v4().to_string();
        self.control_tickets.insert(
            ticket.clone(),
            PendingConfirmation {
                action: name.into(),
                args: json!({"session_id": target}),
                target,
                backend_id: row.backend_id.clone(),
                cwd: row.cwd.clone(),
                member_dirs: row.member_dirs.clone(),
                registry_version: self.registry.version(),
                peer,
                expires: std::time::Instant::now() + std::time::Duration::from_secs(30),
            },
        );
        Ok(
            json!({"ok": false, "error": {"code": "confirmation_required", "message": "confirm this destructive action", "ticket": ticket}}),
        )
    }

    fn confirm_control_action(&mut self, ticket: &str, peer: u32, request_id: &str) -> Value {
        let instance = &self.control.as_ref().expect("control server").instance.id;
        if thurbox::ui_control::audit(instance, peer, "confirm", None, "attempted", request_id)
            .is_err()
        {
            return json!({"ok": false, "error": {"code": "audit_unavailable", "message": "cannot record destructive action"}});
        }
        let Some(pending) = self.control_tickets.remove(ticket) else {
            let _ = thurbox::ui_control::audit(
                instance,
                peer,
                "confirm",
                None,
                "invalid_ticket",
                request_id,
            );
            return json!({"ok": false, "error": {"code": "invalid_ticket", "message": "confirmation ticket is invalid"}});
        };
        let valid = pending.peer == peer
            && pending.expires > std::time::Instant::now()
            && pending.registry_version == self.registry.version()
            && pending.args == json!({"session_id": pending.target})
            && self
                .snapshots
                .current()
                .session(&pending.target)
                .is_some_and(|row| {
                    row.backend_id == pending.backend_id
                        && row.cwd == pending.cwd
                        && row.member_dirs == pending.member_dirs
                });
        if !valid {
            let _ = thurbox::ui_control::audit(
                instance,
                peer,
                &pending.action,
                Some(&pending.target),
                "stale_target",
                request_id,
            );
            return json!({"ok": false, "error": {"code": "stale_target", "message": "confirmation target changed"}});
        }
        if thurbox::ui_control::audit(
            instance,
            peer,
            &pending.action,
            Some(&pending.target),
            "confirmed",
            request_id,
        )
        .is_err()
        {
            return json!({"ok": false, "error": {"code": "audit_unavailable", "message": "cannot record destructive action"}});
        }
        let result: Result<(), (&'static str, String)> = match pending.action.as_str() {
            "sessions.force_delete" | "sessions.delete" | "sessions.restart" | "sessions.sync" => {
                let inner = match pending.action.as_str() {
                    "sessions.force_delete" => Command::Delete {
                        session: pending.target.clone(),
                        force: true,
                    },
                    "sessions.delete" => Command::Delete {
                        session: pending.target.clone(),
                        force: false,
                    },
                    "sessions.restart" => Command::Restart {
                        session: pending.target.clone(),
                        if_missing: false,
                    },
                    _ => Command::Sync {
                        session: pending.target.clone(),
                    },
                };
                self.dispatch_tracked(Command::Guarded {
                    inner: Box::new(inner),
                    session: pending.target,
                    backend_id: pending.backend_id,
                    cwd: pending.cwd,
                    member_dirs: pending.member_dirs,
                });
                Ok(())
            }
            _ => Err(("unavailable", "unknown confirmed action".into())),
        };
        match result {
            Ok(()) => {
                self.note_input();
                self.refresh_control_state(true);
                self.control_event(
                    "action.completed",
                    json!({"action": pending.action, "request_id": request_id, "ok": true}),
                );
                json!({"ok": true, "state": self.control_state()})
            }
            Err((code, message)) => {
                json!({"ok": false, "error": {"code": code, "message": message}})
            }
        }
    }

    fn control_action(&mut self, name: &str, args: &Value) -> Result<(), (&'static str, String)> {
        let object = args
            .as_object()
            .ok_or(("invalid_arguments", "arguments must be an object".into()))?;
        let descriptor = self
            .live_catalog()
            .into_iter()
            .find(|entry| entry.name == name)
            .ok_or(("unknown_action", "unknown UI action".into()))?;
        if !descriptor.available {
            return Err(("unavailable", "action is unavailable".into()));
        }
        if descriptor.destructive {
            return Err((
                "confirmation_required",
                "this action requires on-screen confirmation".into(),
            ));
        }
        for key in object.keys() {
            if descriptor.argument(key).is_none() {
                return Err(("invalid_arguments", format!("unexpected argument {key}")));
            }
        }
        for argument in &descriptor.arguments {
            if argument.required && !object.contains_key(argument.name.as_str()) {
                return Err((
                    "invalid_arguments",
                    format!("{} is required", argument.name),
                ));
            }
            if let Some(value) = object.get(argument.name.as_str()) {
                if !value.is_string() {
                    return Err((
                        "invalid_arguments",
                        format!("{} must be a string", argument.name),
                    ));
                }
                if argument.kind == "uuid"
                    && uuid::Uuid::parse_str(value.as_str().unwrap_or_default()).is_err()
                {
                    return Err((
                        "invalid_arguments",
                        format!("{} must be a UUID", argument.name),
                    ));
                }
            }
        }
        match name {
            "session.focus" => {
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
            "search.open" if descriptor.owner == "search" => {
                let query = object.get("query").and_then(Value::as_str).unwrap_or("");
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
            _ if descriptor.owner == "kernel" => {
                if self.run_kernel_action(name) {
                    Ok(())
                } else {
                    Err(("unavailable", "kernel action is unavailable".into()))
                }
            }
            _ => {
                let index = self
                    .host
                    .index_of(&descriptor.owner)
                    .ok_or(("unavailable", "action owner is not loaded".into()))?;
                let handled = self
                    .host
                    .on_action_with_args(
                        index,
                        name,
                        &object
                            .iter()
                            .map(|(key, value)| (key.as_str(), value.as_str().unwrap_or_default()))
                            .collect::<Vec<_>>(),
                    )
                    .map_err(|error| ("action_failed", error.to_string()))?;
                if handled {
                    Ok(())
                } else {
                    Err(("unavailable", "plugin declined the action".into()))
                }
            }
        }
    }

    fn live_catalog(&self) -> Vec<ActionDescriptor> {
        self.registry
            .action_catalog()
            .into_iter()
            .map(|mut descriptor| {
                descriptor.available &= match descriptor.owner.as_str() {
                    "kernel" if descriptor.name == "kernel.perf_hud" => {
                        self.config.features().perf_hud
                    }
                    "kernel" if descriptor.name == "session.focus" => self
                        .host
                        .index_of("agent")
                        .is_some_and(|index| self.host.focusable().contains(&index)),
                    "kernel" => true,
                    owner => self
                        .host
                        .index_of(owner)
                        .is_some_and(|index| self.host.action_handler_present(index)),
                };
                if descriptor.destructive
                    && !matches!(
                        descriptor.name.as_str(),
                        "sessions.delete"
                            | "sessions.force_delete"
                            | "sessions.restart"
                            | "sessions.sync"
                    )
                {
                    descriptor.available = false;
                }
                if descriptor.destructive
                    && descriptor.owner == "sessions"
                    && !self.host.index_of("sessions").is_some_and(|index| {
                        self.host.plugins[index].path == "plugins/10_sessions.lua"
                    })
                {
                    descriptor.available = false;
                }
                descriptor
            })
            .collect()
    }

    fn control_input(
        &mut self,
        target: &str,
        input: InputOperation,
    ) -> Result<(), (&'static str, String)> {
        if target == "modal" {
            if !self.modals.is_open() {
                return Err(("unavailable", "no modal is open".into()));
            }
            match input {
                InputOperation::Key { chord } => {
                    let key = super::key_event_from_chord(&chord)
                        .ok_or(("invalid_arguments", "invalid key chord".into()))?;
                    self.dispatch_modal_key(&key);
                }
                InputOperation::Text { text } => {
                    if text.len() > 4096 {
                        return Err(("invalid_arguments", "text is too long".into()));
                    }
                    self.on_paste(text);
                }
                InputOperation::Scroll { up } => {
                    let key = crossterm::event::KeyEvent::new(
                        if up {
                            crossterm::event::KeyCode::Up
                        } else {
                            crossterm::event::KeyCode::Down
                        },
                        crossterm::event::KeyModifiers::NONE,
                    );
                    self.dispatch_modal_key(&key);
                }
            }
            return Ok(());
        }
        if self.modals.is_open() {
            return Err(("unavailable", "a modal owns input now".into()));
        }
        let index = self
            .host
            .index_of(target)
            .ok_or(("unavailable", "plugin is not loaded".into()))?;
        if self.host.plugins[index].session_input && !matches!(input, InputOperation::Scroll { .. })
        {
            return Err((
                "unavailable",
                "session terminals require an addressed session input operation".into(),
            ));
        }
        let active = if let Some(grabbed) = self.grabbed {
            grabbed == index
        } else {
            self.host.focusable().get(self.focus) == Some(&index)
        };
        if !active {
            return Err(("unavailable", "plugin does not own input now".into()));
        }
        match input {
            InputOperation::Key { chord } => {
                let key = super::key_event_from_chord(&chord)
                    .ok_or(("invalid_arguments", "invalid key chord".into()))?;
                if target == "confirm"
                    && matches!(
                        key.code,
                        crossterm::event::KeyCode::Enter | crossterm::event::KeyCode::Char('y')
                    )
                {
                    return Err((
                        "confirmation_required",
                        "use ui confirm for destructive actions".into(),
                    ));
                }
                if self
                    .registry
                    .resolve(&super::to_press(&key), Some(target))
                    .is_some_and(|binding| {
                        self.live_catalog()
                            .iter()
                            .any(|entry| entry.name == binding.action && entry.destructive)
                    })
                {
                    return Err((
                        "confirmation_required",
                        "use ui action and ui confirm for destructive actions".into(),
                    ));
                }
                self.dispatch_key_to(index, &key);
            }
            InputOperation::Text { text } => {
                if text.len() > 4096 {
                    return Err(("invalid_arguments", "text is too long".into()));
                }
                if self.grabbed != Some(index) && !self.focused_typing {
                    return Err(("unavailable", "plugin has no active text field".into()));
                }
                self.on_paste(text);
            }
            InputOperation::Scroll { up } => {
                let scroll = thurbox::kernel::host::Scroll { up, x: 0, y: 0 };
                let handled = self
                    .host
                    .on_scroll(index, &scroll)
                    .map_err(|e| ("action_failed", e.to_string()))?;
                if !handled {
                    if self.host.plugins[index].session_input {
                        return Err(("unavailable", "terminal pane declined the scroll".into()));
                    }
                    let key = crossterm::event::KeyEvent::new(
                        if up {
                            crossterm::event::KeyCode::Up
                        } else {
                            crossterm::event::KeyCode::Down
                        },
                        crossterm::event::KeyModifiers::NONE,
                    );
                    self.dispatch_key_to(index, &key);
                }
            }
        }
        Ok(())
    }
}
