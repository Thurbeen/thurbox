use rusqlite::params;

use crate::session::ProfileConfig;
use crate::sync::current_time_millis;

use super::Database;

/// Where a resolved profile came from in the merged view.
///
/// Shaped like [`RoleSource`](super::RoleSource) so a future
/// plugin-contribution path has a place to slot in without breaking
/// callers. Only `Registered` is produced today.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileSource {
    Registered,
    Plugin(String),
}

fn row_to_profile_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProfileConfig> {
    let name: String = row.get(0)?;
    let description: String = row.get(1)?;
    let role_csv: String = row.get(2)?;
    let mcp_csv: String = row.get(3)?;
    let skill_csv: String = row.get(4)?;

    Ok(ProfileConfig {
        name,
        description,
        roles: csv_to_vec(&role_csv),
        mcp_servers: csv_to_vec(&mcp_csv),
        skills: csv_to_vec(&skill_csv),
    })
}

impl Database {
    /// List all global profiles in alphabetical order.
    pub fn list_global_profiles(&self) -> rusqlite::Result<Vec<ProfileConfig>> {
        let mut stmt = self.conn.prepare(
            "SELECT profile_name, description, role_names, mcp_server_names, skill_names \
             FROM profiles ORDER BY profile_name",
        )?;

        let profiles = stmt
            .query_map([], row_to_profile_config)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(profiles)
    }

    /// Fetch a single global profile by name.
    pub fn get_global_profile(&self, name: &str) -> rusqlite::Result<Option<ProfileConfig>> {
        self.conn
            .query_row(
                "SELECT profile_name, description, role_names, mcp_server_names, skill_names \
                 FROM profiles WHERE profile_name = ?1",
                params![name],
                row_to_profile_config,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// Atomically replace all global profiles (delete existing + insert new).
    pub fn replace_global_profiles(&self, profiles: &[ProfileConfig]) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;

        self.conn.execute("DELETE FROM profiles", [])?;

        for profile in profiles {
            self.conn.execute(
                "INSERT INTO profiles \
                 (profile_name, description, role_names, mcp_server_names, skill_names, \
                  created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    profile.name,
                    profile.description,
                    vec_to_csv(&profile.roles),
                    vec_to_csv(&profile.mcp_servers),
                    vec_to_csv(&profile.skills),
                    now,
                    now,
                ],
            )?;
        }

        Ok(())
    }

    /// Insert or update a single global profile.
    pub fn upsert_global_profile(&self, profile: &ProfileConfig) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;

        self.conn.execute(
            "INSERT INTO profiles \
             (profile_name, description, role_names, mcp_server_names, skill_names, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(profile_name) DO UPDATE SET \
              description = excluded.description, \
              role_names = excluded.role_names, \
              mcp_server_names = excluded.mcp_server_names, \
              skill_names = excluded.skill_names, \
              updated_at = excluded.updated_at",
            params![
                profile.name,
                profile.description,
                vec_to_csv(&profile.roles),
                vec_to_csv(&profile.mcp_servers),
                vec_to_csv(&profile.skills),
                now,
                now,
            ],
        )?;

        Ok(())
    }

    /// Delete a single global profile by name. Returns whether a row was
    /// removed (false when the profile did not exist).
    pub fn delete_global_profile(&self, profile_name: &str) -> rusqlite::Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM profiles WHERE profile_name = ?1",
            params![profile_name],
        )?;
        Ok(rows > 0)
    }

    /// Effective profile list — registered profiles plus any future plugin
    /// contributions. Today every entry is `ProfileSource::Registered`.
    pub fn list_effective_profiles(&self) -> rusqlite::Result<Vec<(ProfileConfig, ProfileSource)>> {
        Ok(self
            .list_global_profiles()?
            .into_iter()
            .map(|p| (p, ProfileSource::Registered))
            .collect())
    }
}

fn csv_to_vec(csv: &str) -> Vec<String> {
    if csv.is_empty() {
        Vec::new()
    } else {
        csv.split(',').map(|s| s.to_string()).collect()
    }
}

fn vec_to_csv(v: &[String]) -> String {
    v.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> ProfileConfig {
        ProfileConfig {
            name: name.to_string(),
            description: format!("{name} profile"),
            roles: vec!["developer".to_string(), "reviewer".to_string()],
            mcp_servers: vec!["github".to_string()],
            skills: vec!["refactor".to_string()],
        }
    }

    #[test]
    fn list_global_profiles_empty_after_wipe() {
        let db = Database::open_in_memory().unwrap();
        db.replace_global_profiles(&[]).unwrap();
        assert!(db.list_global_profiles().unwrap().is_empty());
    }

    #[test]
    fn replace_and_list_global_profiles_roundtrip() {
        let db = Database::open_in_memory().unwrap();

        let profiles = vec![sample("alpha"), sample("beta")];
        db.replace_global_profiles(&profiles).unwrap();
        let loaded = db.list_global_profiles().unwrap();

        assert_eq!(loaded, profiles);
    }

    #[test]
    fn replace_global_profiles_overwrites_previous() {
        let db = Database::open_in_memory().unwrap();

        db.replace_global_profiles(&[sample("old")]).unwrap();
        db.replace_global_profiles(&[sample("new")]).unwrap();

        let loaded = db.list_global_profiles().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "new");
    }

    #[test]
    fn upsert_global_profile_insert_then_update() {
        let db = Database::open_in_memory().unwrap();

        let mut p = sample("dev");
        p.description = "original".to_string();
        db.upsert_global_profile(&p).unwrap();

        let loaded = db.get_global_profile("dev").unwrap().unwrap();
        assert_eq!(loaded.description, "original");

        p.description = "updated".to_string();
        p.roles = vec!["developer".to_string()];
        db.upsert_global_profile(&p).unwrap();

        let loaded = db.get_global_profile("dev").unwrap().unwrap();
        assert_eq!(loaded.description, "updated");
        assert_eq!(loaded.roles, vec!["developer".to_string()]);
    }

    #[test]
    fn delete_global_profile_returns_removal() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_global_profile(&sample("dev")).unwrap();
        assert!(db.delete_global_profile("dev").unwrap());
        assert!(!db.delete_global_profile("dev").unwrap());

        assert!(db.get_global_profile("dev").unwrap().is_none());
    }

    #[test]
    fn get_global_profile_returns_none_when_missing() {
        let db = Database::open_in_memory().unwrap();
        db.replace_global_profiles(&[]).unwrap();
        assert!(db.get_global_profile("ghost").unwrap().is_none());
    }

    #[test]
    fn list_effective_profiles_tags_registered() {
        let db = Database::open_in_memory().unwrap();
        db.replace_global_profiles(&[sample("dev")]).unwrap();

        let effective = db.list_effective_profiles().unwrap();
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].1, ProfileSource::Registered);
        assert_eq!(effective[0].0.name, "dev");
    }

    #[test]
    fn empty_list_fields_roundtrip_as_empty_vecs() {
        let db = Database::open_in_memory().unwrap();
        let profile = ProfileConfig {
            name: "bare".to_string(),
            description: String::new(),
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            skills: Vec::new(),
        };
        db.upsert_global_profile(&profile).unwrap();
        let loaded = db.get_global_profile("bare").unwrap().unwrap();
        assert!(loaded.roles.is_empty());
        assert!(loaded.mcp_servers.is_empty());
        assert!(loaded.skills.is_empty());
    }

    #[test]
    fn csv_roundtrip() {
        assert_eq!(csv_to_vec(""), Vec::<String>::new());
        assert_eq!(csv_to_vec("a"), vec!["a".to_string()]);
        assert_eq!(
            csv_to_vec("a,b,c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(vec_to_csv(&[]), "");
        assert_eq!(vec_to_csv(&["a".to_string()]), "a");
        assert_eq!(vec_to_csv(&["a".to_string(), "b".to_string()]), "a,b");
    }
}
