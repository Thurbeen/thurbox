//! Containerfile template subcommands.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use super::validate_safe_name;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List templates with their files.
    List,
    /// Print a template's Containerfile content + list support files.
    Get {
        /// Template name.
        name: String,
    },
    /// Create or update a template.
    Set {
        /// Template name.
        name: String,
        /// Path to the Containerfile content.
        #[arg(long)]
        file: PathBuf,
        /// Support file as `filename=path` (repeatable).
        #[command(flatten)]
        support: SupportFiles,
    },
    /// Delete a template (refuses to delete "default").
    Delete {
        /// Template name.
        name: String,
    },
}

#[derive(Args, Debug)]
pub struct SupportFiles {
    /// Support file spec `filename=path`, repeatable.
    ///
    /// On the CLI we can't accept arbitrary file *contents* as a single
    /// argument, so each flag gives a `filename=host_path` pair.
    #[arg(long = "support-file")]
    pub files: Vec<String>,
}

pub fn run(action: Action) -> Result<Value, String> {
    match action {
        Action::List => {
            let templates: Vec<Value> = crate::paths::list_containerfile_templates()?
                .into_iter()
                .map(|(name, files)| json!({ "name": name, "files": files }))
                .collect();
            Ok(Value::Array(templates))
        }
        Action::Get { name } => {
            validate_safe_name(&name)?;
            let dir = crate::paths::containerfiles_directory()
                .ok_or_else(|| "Could not resolve containerfiles directory".to_string())?;
            let tdir = dir.join(&name);
            if !tdir.is_dir() {
                return Err(format!("Template not found: {name}"));
            }
            let content = std::fs::read_to_string(tdir.join("Containerfile"))
                .map_err(|_| format!("Template '{name}' has no Containerfile"))?;
            let mut support = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&tdir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                        let fname = entry.file_name().to_string_lossy().to_string();
                        if fname != "Containerfile" {
                            support.push(fname);
                        }
                    }
                }
            }
            support.sort();
            Ok(json!({
                "name": name,
                "containerfile_content": content,
                "support_files": support,
            }))
        }
        Action::Set {
            name,
            file,
            support,
        } => {
            validate_safe_name(&name)?;
            let dir = crate::paths::containerfiles_directory()
                .ok_or_else(|| "Could not resolve containerfiles directory".to_string())?;
            let tdir = dir.join(&name);
            std::fs::create_dir_all(&tdir)
                .map_err(|e| format!("Failed to create template directory: {e}"))?;
            let content = super::read_file(&file)?;
            std::fs::write(tdir.join("Containerfile"), &content)
                .map_err(|e| format!("Failed to write Containerfile: {e}"))?;
            let mut all_files = vec!["Containerfile".to_string()];
            for spec in &support.files {
                let (fname, src) = spec
                    .split_once('=')
                    .ok_or_else(|| format!("Invalid --support-file {spec} (expected name=path)"))?;
                validate_safe_name(fname)?;
                let body = super::read_file(&PathBuf::from(src))?;
                std::fs::write(tdir.join(fname), body)
                    .map_err(|e| format!("Failed to write {fname}: {e}"))?;
                all_files.push(fname.to_string());
            }
            all_files.sort();
            Ok(json!({ "name": name, "files": all_files }))
        }
        Action::Delete { name } => {
            validate_safe_name(&name)?;
            if name == "default" {
                return Err("Cannot delete the 'default' template".into());
            }
            let dir = crate::paths::containerfiles_directory()
                .ok_or_else(|| "Could not resolve containerfiles directory".to_string())?;
            let tdir = dir.join(&name);
            if !tdir.is_dir() {
                return Err(format!("Template not found: {name}"));
            }
            std::fs::remove_dir_all(&tdir)
                .map_err(|e| format!("Failed to delete template: {e}"))?;
            Ok(json!({ "deleted": true, "name": name }))
        }
    }
}
