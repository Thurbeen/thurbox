//! VM CRUD operations for the SQLite database.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;

use crate::session::{VmConfig, VmState};

use super::Database;

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// A VM record from the database.
#[derive(Debug, Clone)]
pub struct VmRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub state: VmState,
    pub ssh_port: u16,
    pub base_image: String,
    pub cpus: u32,
    pub memory_mb: u32,
    pub disk_gb: u32,
    pub qemu_pid: Option<u32>,
    pub error_msg: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Database {
    /// Insert a new VM record.
    pub fn insert_vm(
        &self,
        vm_id: &str,
        session_id: Option<&str>,
        state: &VmState,
        ssh_port: u16,
        config: &VmConfig,
    ) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO vms (id, session_id, state, ssh_port, base_image, cpus, memory_mb, disk_gb, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                vm_id,
                session_id,
                state.to_db_str(),
                ssh_port as i64,
                config.base_image,
                config.cpus,
                config.memory_mb,
                config.disk_gb,
                now,
                now,
            ],
        )?;
        Ok(())
    }

    /// Update VM state.
    pub fn update_vm_state(
        &self,
        vm_id: &str,
        state: &VmState,
        qemu_pid: Option<u32>,
        error_msg: Option<&str>,
    ) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "UPDATE vms SET state = ?1, qemu_pid = ?2, error_msg = ?3, updated_at = ?4 WHERE id = ?5",
            params![
                state.to_db_str(),
                qemu_pid.map(|p| p as i64),
                error_msg,
                now,
                vm_id,
            ],
        )?;
        Ok(())
    }

    /// Get a VM record by ID.
    pub fn get_vm(&self, vm_id: &str) -> rusqlite::Result<Option<VmRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, state, ssh_port, base_image, cpus, memory_mb, disk_gb, qemu_pid, error_msg, created_at, updated_at
             FROM vms WHERE id = ?1 AND deleted_at IS NULL",
        )?;

        let mut rows = stmt.query(params![vm_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_vm_record(row)?)),
            None => Ok(None),
        }
    }

    /// Get VM record by session ID.
    pub fn get_vm_by_session(&self, session_id: &str) -> rusqlite::Result<Option<VmRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, state, ssh_port, base_image, cpus, memory_mb, disk_gb, qemu_pid, error_msg, created_at, updated_at
             FROM vms WHERE session_id = ?1 AND deleted_at IS NULL",
        )?;

        let mut rows = stmt.query(params![session_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_vm_record(row)?)),
            None => Ok(None),
        }
    }

    /// List all active VMs.
    pub fn list_vms(&self) -> rusqlite::Result<Vec<VmRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, state, ssh_port, base_image, cpus, memory_mb, disk_gb, qemu_pid, error_msg, created_at, updated_at
             FROM vms WHERE deleted_at IS NULL
             ORDER BY created_at",
        )?;
        let mut records = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            records.push(row_to_vm_record(row)?);
        }
        Ok(records)
    }

    /// Update the SSH port on a VM record.
    pub fn update_vm_ssh_port(&self, vm_id: &str, ssh_port: u16) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "UPDATE vms SET ssh_port = ?1, updated_at = ?2 WHERE id = ?3",
            params![ssh_port as i64, now, vm_id],
        )?;
        Ok(())
    }

    /// Update the session ID on a VM record.
    pub fn update_vm_session(&self, vm_id: &str, session_id: &str) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "UPDATE vms SET session_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![session_id, now, vm_id],
        )?;
        Ok(())
    }

    /// Soft-delete a VM record.
    pub fn soft_delete_vm(&self, vm_id: &str) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "UPDATE vms SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            params![now, vm_id],
        )?;
        Ok(())
    }
}

fn row_to_vm_record(row: &rusqlite::Row) -> rusqlite::Result<VmRecord> {
    let state_str: String = row.get(2)?;
    Ok(VmRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        state: VmState::from_db_str(&state_str),
        ssh_port: row.get::<_, i64>(3)? as u16,
        base_image: row.get(4)?,
        cpus: row.get::<_, i64>(5)? as u32,
        memory_mb: row.get::<_, i64>(6)? as u32,
        disk_gb: row.get::<_, i64>(7)? as u32,
        qemu_pid: row.get::<_, Option<i64>>(8)?.map(|v| v as u32),
        error_msg: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn insert_and_get_vm() {
        let db = test_db();
        let config = VmConfig::default();

        db.insert_vm("vm-1", None, &VmState::Provisioning, 22200, &config)
            .unwrap();

        let vm = db.get_vm("vm-1").unwrap().unwrap();
        assert_eq!(vm.id, "vm-1");
        assert_eq!(vm.state, VmState::Provisioning);
        assert_eq!(vm.ssh_port, 22200);
        assert_eq!(vm.cpus, 2);
        assert_eq!(vm.memory_mb, 2048);
    }

    #[test]
    fn get_nonexistent_vm() {
        let db = test_db();
        let vm = db.get_vm("nonexistent").unwrap();
        assert!(vm.is_none());
    }

    #[test]
    fn update_vm_state() {
        let db = test_db();
        let config = VmConfig::default();

        db.insert_vm("vm-1", None, &VmState::Provisioning, 22200, &config)
            .unwrap();
        db.update_vm_state("vm-1", &VmState::Ready, Some(12345), None)
            .unwrap();

        let vm = db.get_vm("vm-1").unwrap().unwrap();
        assert_eq!(vm.state, VmState::Ready);
        assert_eq!(vm.qemu_pid, Some(12345));
    }

    #[test]
    fn list_vms_all() {
        let db = test_db();
        let config = VmConfig::default();

        db.insert_vm("vm-1", None, &VmState::Ready, 22200, &config)
            .unwrap();
        db.insert_vm("vm-2", None, &VmState::Stopped, 22201, &config)
            .unwrap();

        let vms = db.list_vms().unwrap();
        assert_eq!(vms.len(), 2);
    }

    #[test]
    fn soft_delete_vm() {
        let db = test_db();
        let config = VmConfig::default();

        db.insert_vm("vm-1", None, &VmState::Stopped, 22200, &config)
            .unwrap();
        db.soft_delete_vm("vm-1").unwrap();

        let vm = db.get_vm("vm-1").unwrap();
        assert!(vm.is_none());

        let vms = db.list_vms().unwrap();
        assert!(vms.is_empty());
    }

    #[test]
    fn get_vm_by_session() {
        let db = test_db();
        let config = VmConfig::default();

        let session_id = uuid::Uuid::new_v4().to_string();
        db.conn
            .execute(
                "INSERT INTO sessions (id, name, created_at, updated_at) VALUES (?1, 'test', 0, 0)",
                rusqlite::params![session_id],
            )
            .unwrap();

        db.insert_vm("vm-1", Some(&session_id), &VmState::Ready, 22200, &config)
            .unwrap();

        let vm = db.get_vm_by_session(&session_id).unwrap().unwrap();
        assert_eq!(vm.id, "vm-1");
        assert_eq!(vm.session_id, Some(session_id));
    }

    #[test]
    fn update_vm_ssh_port() {
        let db = test_db();
        let config = VmConfig::default();

        db.insert_vm("vm-1", None, &VmState::Provisioning, 0, &config)
            .unwrap();

        db.update_vm_ssh_port("vm-1", 22200).unwrap();

        let vm = db.get_vm("vm-1").unwrap().unwrap();
        assert_eq!(vm.ssh_port, 22200);
    }

    #[test]
    fn update_vm_session_links_session() {
        let db = test_db();
        let config = VmConfig::default();

        let session_id = uuid::Uuid::new_v4().to_string();
        db.conn
            .execute(
                "INSERT INTO sessions (id, name, created_at, updated_at) VALUES (?1, 'test', 0, 0)",
                rusqlite::params![session_id],
            )
            .unwrap();

        db.insert_vm("vm-1", None, &VmState::Ready, 22200, &config)
            .unwrap();
        db.update_vm_session("vm-1", &session_id).unwrap();
        let vm = db.get_vm("vm-1").unwrap().unwrap();
        assert_eq!(vm.session_id, Some(session_id.clone()));
    }

    #[test]
    fn update_vm_state_with_error() {
        let db = test_db();
        let config = VmConfig::default();

        db.insert_vm("vm-1", None, &VmState::Starting, 22200, &config)
            .unwrap();
        db.update_vm_state(
            "vm-1",
            &VmState::Failed("disk full".to_string()),
            None,
            Some("disk full"),
        )
        .unwrap();

        let vm = db.get_vm("vm-1").unwrap().unwrap();
        assert_eq!(vm.state, VmState::Failed("disk full".to_string()));
        assert_eq!(vm.error_msg, Some("disk full".to_string()));
    }
}
