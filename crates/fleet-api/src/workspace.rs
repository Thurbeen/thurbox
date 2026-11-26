use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRef {
    pub key: String,
    pub host: String,
    pub dirs: Vec<String>,
}

impl WorkspaceRef {
    pub fn dir(&self) -> Option<&str> {
        self.dirs.first().map(String::as_str)
    }

    pub fn extra_dirs(&self) -> &[String] {
        self.dirs.get(1..).unwrap_or_default()
    }
}
