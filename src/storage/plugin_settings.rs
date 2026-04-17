//! Per-plugin user-overridden settings (`plugin_settings` table).
//!
//! Only rows for *user-overridden* keys exist here. Any unset key falls back to
//! the manifest default at read time via [`Database::list_plugin_settings_with_defaults`].
//!
//! The `value_json` column stores a `toml::Value` round-tripped through JSON —
//! plugin configuration types (string/int/bool/duration/path/enum) are all
//! representable in both formats, so the conversion is lossless and lets us
//! reuse `ConfigurationSchema::validate_value` for schema enforcement.
//!
//! FK cascade on `plugins.plugin_name` deletes settings when a plugin is
//! uninstalled — see the `plugin_settings_cascades_on_plugin_delete` test.

use rusqlite::params;

use crate::session::{ConfigurationSchema, ConfigurationType};
use crate::sync::current_time_millis;

use super::Database;

/// One key in the effective view: the schema, the user override (if any), and
/// the resolved effective value (override or default).
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveSetting {
    pub key: String,
    pub ty: ConfigurationType,
    pub default: toml::Value,
    pub description: String,
    pub user_value: Option<toml::Value>,
    pub effective_value: toml::Value,
}

fn value_to_json_string(value: &toml::Value) -> rusqlite::Result<String> {
    serde_json::to_string(value).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
    })
}

fn json_string_to_value(s: &str) -> rusqlite::Result<toml::Value> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e.to_string())),
        )
    })
}

impl Database {
    /// Read the user-overridden value for one key, if any.
    pub fn get_plugin_setting(
        &self,
        plugin_name: &str,
        key: &str,
    ) -> rusqlite::Result<Option<toml::Value>> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM plugin_settings \
                 WHERE plugin_name = ?1 AND key = ?2",
                params![plugin_name, key],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        row.map(|s| json_string_to_value(&s)).transpose()
    }

    /// Insert or update a user override for one key. Caller is responsible for
    /// validating the value against the manifest schema *before* calling this.
    ///
    /// If the plugin isn't yet in the `plugins` table (disk-only), this creates
    /// a shadow row first to satisfy the FK — same pattern as
    /// `set_plugin_enabled`. Errors with `QueryReturnedNoRows` if the plugin
    /// can't be found on disk *or* in the registry.
    pub fn upsert_plugin_setting(
        &self,
        plugin_name: &str,
        key: &str,
        value: &toml::Value,
    ) -> rusqlite::Result<()> {
        self.ensure_plugin_row(plugin_name)?;
        let now = current_time_millis() as i64;
        let json = value_to_json_string(value)?;
        self.conn.execute(
            "INSERT INTO plugin_settings (plugin_name, key, value_json, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(plugin_name, key) DO UPDATE SET \
                 value_json = excluded.value_json, \
                 updated_at = excluded.updated_at",
            params![plugin_name, key, json, now],
        )?;
        Ok(())
    }

    /// Ensure a row in `plugins` exists for `plugin_name`. If the plugin is
    /// disk-only, creates a shadow row pointing at the disk path. Used before
    /// insertions into FK-bound child tables (`plugin_settings`).
    fn ensure_plugin_row(&self, plugin_name: &str) -> rusqlite::Result<()> {
        let exists: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM plugins WHERE plugin_name = ?1",
            params![plugin_name],
            |r| r.get(0),
        )?;
        if exists > 0 {
            return Ok(());
        }
        let disk = crate::paths::list_disk_plugins()
            .into_iter()
            .find(|p| p.name == plugin_name);
        let Some(d) = disk else {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        };
        self.upsert_global_plugin(&d)
    }

    /// Delete a user override, restoring the manifest default. Returns true if
    /// a row was deleted.
    pub fn reset_plugin_setting(&self, plugin_name: &str, key: &str) -> rusqlite::Result<bool> {
        let count = self.conn.execute(
            "DELETE FROM plugin_settings WHERE plugin_name = ?1 AND key = ?2",
            params![plugin_name, key],
        )?;
        Ok(count > 0)
    }

    /// Project the manifest schema with any user overrides folded in.
    ///
    /// The returned vector has one entry per `schema` row, in the same order.
    /// Keys not declared in `schema` are not surfaced even if a stale row
    /// exists in the table — the manifest is the source of truth.
    pub fn list_plugin_settings_with_defaults(
        &self,
        plugin_name: &str,
        schema: &[ConfigurationSchema],
    ) -> rusqlite::Result<Vec<EffectiveSetting>> {
        schema
            .iter()
            .map(|cfg| {
                let user_value = self.get_plugin_setting(plugin_name, &cfg.key)?;
                let effective = user_value.clone().unwrap_or_else(|| cfg.default.clone());
                Ok(EffectiveSetting {
                    key: cfg.key.clone(),
                    ty: cfg.ty,
                    default: cfg.default.clone(),
                    description: cfg.description.clone(),
                    user_value,
                    effective_value: effective,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::session::PluginConfig;

    fn schema_int(
        key: &str,
        default: i64,
        min: Option<i64>,
        max: Option<i64>,
    ) -> ConfigurationSchema {
        ConfigurationSchema {
            key: key.into(),
            ty: ConfigurationType::Int,
            default: toml::Value::Integer(default),
            description: String::new(),
            min,
            max,
            values: vec![],
        }
    }

    fn schema_string(key: &str, default: &str) -> ConfigurationSchema {
        ConfigurationSchema {
            key: key.into(),
            ty: ConfigurationType::String,
            default: toml::Value::String(default.into()),
            description: String::new(),
            min: None,
            max: None,
            values: vec![],
        }
    }

    fn seed_plugin(db: &Database, name: &str) {
        db.upsert_global_plugin(&PluginConfig {
            name: name.into(),
            path: PathBuf::from(format!("/tmp/{name}")),
            version: "0.1.0".into(),
            enabled: true,
        })
        .unwrap();
    }

    #[test]
    fn get_returns_none_when_unset() {
        let db = Database::open_in_memory().unwrap();
        seed_plugin(&db, "p");
        assert!(db.get_plugin_setting("p", "missing").unwrap().is_none());
    }

    #[test]
    fn upsert_then_get_roundtrips() {
        let db = Database::open_in_memory().unwrap();
        seed_plugin(&db, "p");
        let v = toml::Value::Integer(7);
        db.upsert_plugin_setting("p", "workers", &v).unwrap();
        assert_eq!(db.get_plugin_setting("p", "workers").unwrap(), Some(v));
    }

    #[test]
    fn upsert_replaces_existing_value() {
        let db = Database::open_in_memory().unwrap();
        seed_plugin(&db, "p");
        db.upsert_plugin_setting("p", "k", &toml::Value::Integer(1))
            .unwrap();
        db.upsert_plugin_setting("p", "k", &toml::Value::Integer(2))
            .unwrap();
        assert_eq!(
            db.get_plugin_setting("p", "k").unwrap(),
            Some(toml::Value::Integer(2))
        );
    }

    #[test]
    fn reset_returns_existence_and_clears() {
        let db = Database::open_in_memory().unwrap();
        seed_plugin(&db, "p");
        db.upsert_plugin_setting("p", "k", &toml::Value::Boolean(true))
            .unwrap();
        assert!(db.reset_plugin_setting("p", "k").unwrap());
        assert!(!db.reset_plugin_setting("p", "k").unwrap());
        assert!(db.get_plugin_setting("p", "k").unwrap().is_none());
    }

    #[test]
    fn effective_falls_back_to_default_when_no_override() {
        let db = Database::open_in_memory().unwrap();
        seed_plugin(&db, "p");
        let schema = vec![
            schema_int("workers", 3, Some(1), Some(16)),
            schema_string("greeting", "hello"),
        ];
        let eff = db.list_plugin_settings_with_defaults("p", &schema).unwrap();
        assert_eq!(eff.len(), 2);
        assert!(eff[0].user_value.is_none());
        assert_eq!(eff[0].effective_value, toml::Value::Integer(3));
        assert_eq!(eff[1].effective_value, toml::Value::String("hello".into()));
    }

    #[test]
    fn effective_uses_user_override_when_present() {
        let db = Database::open_in_memory().unwrap();
        seed_plugin(&db, "p");
        db.upsert_plugin_setting("p", "workers", &toml::Value::Integer(8))
            .unwrap();
        let schema = vec![schema_int("workers", 3, Some(1), Some(16))];
        let eff = db.list_plugin_settings_with_defaults("p", &schema).unwrap();
        assert_eq!(eff[0].user_value, Some(toml::Value::Integer(8)));
        assert_eq!(eff[0].effective_value, toml::Value::Integer(8));
    }

    #[test]
    fn effective_drops_stale_keys_not_in_schema() {
        let db = Database::open_in_memory().unwrap();
        seed_plugin(&db, "p");
        db.upsert_plugin_setting("p", "removed_key", &toml::Value::Integer(99))
            .unwrap();
        let schema = vec![schema_int("workers", 3, None, None)];
        let eff = db.list_plugin_settings_with_defaults("p", &schema).unwrap();
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].key, "workers");
    }

    #[test]
    fn settings_cascade_on_plugin_delete() {
        let db = Database::open_in_memory().unwrap();
        // FK enforcement is enabled by schema initialization; verify by relying
        // on the cascade behavior.
        seed_plugin(&db, "p");
        db.upsert_plugin_setting("p", "k", &toml::Value::Integer(1))
            .unwrap();
        assert!(db.delete_global_plugin("p").unwrap());
        // Re-seed under same name; row should be gone, not lingering.
        seed_plugin(&db, "p");
        assert!(db.get_plugin_setting("p", "k").unwrap().is_none());
    }
}
