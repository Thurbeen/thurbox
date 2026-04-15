//! VM read-only subcommands.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::storage::vms::VmRecord;
use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List all VMs.
    List,
    /// Get a VM by UUID.
    Get {
        /// VM UUID.
        uuid: String,
    },
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::List => {
            let vms = db.list_vms().map_err(|e| format!("list_vms: {e}"))?;
            Ok(Value::Array(vms.iter().map(vm_to_json).collect()))
        }
        Action::Get { uuid } => match db.get_vm(&uuid).map_err(|e| format!("get_vm: {e}"))? {
            Some(vm) => Ok(vm_to_json(&vm)),
            None => Err(format!("VM not found: {uuid}")),
        },
    }
}

fn vm_to_json(r: &VmRecord) -> Value {
    json!({
        "id": r.id,
        "session_id": r.session_id,
        "state": r.state.to_string(),
        "ssh_port": r.ssh_port,
        "base_image": r.base_image,
        "cpus": r.cpus,
        "memory_mb": r.memory_mb,
        "disk_gb": r.disk_gb,
        "error_msg": r.error_msg,
    })
}
