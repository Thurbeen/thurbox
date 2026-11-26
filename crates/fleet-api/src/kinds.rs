use crate::store::FieldManager;
use crate::{board, change_request, conversation, cron_task, listener, task};

pub const BUILT_IN: &[&str] = &[
    task::KIND,
    crate::config::TASK_CONFIG,
    crate::config::ENV_PROVIDER,
    change_request::KIND,
    conversation::KIND,
    listener::KIND,
    cron_task::KIND,
    board::KIND,
];

pub fn is_served(kind: &str) -> bool {
    BUILT_IN.contains(&kind)
}

pub fn controller_of(kind: &str) -> Option<FieldManager> {
    match kind {
        task::KIND => Some(FieldManager::TaskController),
        change_request::KIND => Some(FieldManager::ChangeRequestController),
        conversation::KIND => Some(FieldManager::ConversationController),
        listener::KIND => Some(FieldManager::ListenerController),
        cron_task::KIND => Some(FieldManager::CronTaskController),
        _ => None,
    }
}

pub fn status_owner(kind: &str, field: &str) -> Option<FieldManager> {
    match kind {
        task::KIND if task::UNOWNED_FIELDS.contains(&field) => None,
        task::KIND if task::WORKER_FIELDS.contains(&field) => Some(FieldManager::Worker),
        task::KIND if task::FORGE_FIELDS.contains(&field) => Some(FieldManager::ForgeController),
        task::KIND if task::GATEWAY_FIELDS.contains(&field) => {
            Some(FieldManager::GatewayController)
        }
        task::KIND => Some(FieldManager::TaskController),
        change_request::KIND => Some(FieldManager::ChangeRequestController),
        conversation::KIND if conversation::UNOWNED_FIELDS.contains(&field) => None,
        conversation::KIND => Some(FieldManager::ConversationController),
        listener::KIND if listener::UNOWNED_FIELDS.contains(&field) => None,
        listener::KIND => Some(FieldManager::ListenerController),
        cron_task::KIND => Some(FieldManager::CronTaskController),
        _ => None,
    }
}

pub fn may_create(kind: &str, by: &FieldManager) -> bool {
    match by {
        FieldManager::Apply => true,
        FieldManager::Listener(_) => matches!(kind, conversation::KIND | listener::KIND),
        FieldManager::ConversationController | FieldManager::CronTaskController => {
            kind == task::KIND
        }
        FieldManager::GatewayController => kind == conversation::KIND,
        _ => false,
    }
}

pub fn may_set_conversation_labels(by: &FieldManager) -> bool {
    *by == FieldManager::ConversationController
}

pub fn may_set_cron_task_labels(by: &FieldManager) -> bool {
    *by == FieldManager::CronTaskController
}

pub fn may_retire_scheduled_task(by: &FieldManager) -> bool {
    *by == FieldManager::CronTaskController
}

pub fn is_listener_of(by: &FieldManager, account: &str) -> bool {
    matches!(by, FieldManager::Listener(a) if a == account)
}

pub fn may_spool(by: &FieldManager, account: &str, verified: crate::gateway::Verified) -> bool {
    is_listener_of(by, account)
        || (*by == FieldManager::GatewayController && verified == crate::gateway::Verified::Fleet)
}

pub fn may_record_received(by: &FieldManager) -> bool {
    *by == FieldManager::ConversationController
}

pub fn may_retire_conversation_task(by: &FieldManager) -> bool {
    *by == FieldManager::ConversationController
}

pub fn may_append_inbox(by: FieldManager) -> bool {
    matches!(
        by,
        FieldManager::Cli | FieldManager::ForgeController | FieldManager::GatewayController
    )
}

pub fn may_request_wake(by: &FieldManager) -> bool {
    *by == FieldManager::Cli
}

pub fn may_request_pause(by: &FieldManager) -> bool {
    *by == FieldManager::Cli
}

pub fn may_request_restart(by: &FieldManager) -> bool {
    *by == FieldManager::Cli
}

pub fn may_record_relay(by: FieldManager) -> bool {
    relay_field(&by).is_some()
}

pub fn relay_field(by: &FieldManager) -> Option<&'static str> {
    match by {
        FieldManager::ForgeController => Some("forge"),
        FieldManager::GatewayController => Some("gateway"),
        FieldManager::TaskController => Some("subscriptions"),
        _ => None,
    }
}

pub fn inbox_source_prefix(by: FieldManager) -> Option<&'static str> {
    match by {
        FieldManager::Cli => Some(crate::inbox::CLI_SOURCE_PREFIX),
        FieldManager::ForgeController => Some(crate::inbox::FORGE_SOURCE_PREFIX),
        FieldManager::GatewayController => Some(crate::talk::GATEWAY_SOURCE_PREFIX),
        FieldManager::TaskController => Some(crate::subscription::BOARD_SOURCE_PREFIX),
        _ => None,
    }
}

pub fn may_append_events(by: &FieldManager, source: &crate::event::Source) -> bool {
    use crate::event::{ControllerSource, Source};
    match (by, source) {
        (FieldManager::Cli, Source::Cli(_)) => true,
        (FieldManager::TaskController, Source::Task(_)) => true,
        (FieldManager::TaskController, Source::Controller(c)) => *c == ControllerSource::of(by),
        _ => false,
    }
}

pub fn may_record_delivery(by: FieldManager) -> bool {
    controller_of(task::KIND) == Some(by)
}

pub fn finalizers_on_create(kind: &str) -> Vec<String> {
    match kind {
        task::KIND => vec![task::RELEASE_FINALIZER.to_string()],
        _ => Vec::new(),
    }
}
