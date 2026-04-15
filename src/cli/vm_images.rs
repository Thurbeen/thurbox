//! VM image management subcommands.

use clap::Subcommand;
use serde_json::{json, Value};

use super::validate_safe_name;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List downloaded VM images.
    List,
    /// Download a VM image from an HTTPS URL.
    Download {
        /// HTTPS URL to download from.
        url: String,
        /// Filename to save as (default: derived from URL).
        #[arg(long)]
        filename: Option<String>,
    },
    /// Delete a cached VM image.
    Delete {
        /// Filename of the image to delete.
        filename: String,
    },
}

pub fn run(action: Action) -> Result<Value, String> {
    match action {
        Action::List => {
            let dir = crate::paths::images_directory()
                .ok_or_else(|| "Could not resolve images directory".to_string())?;
            if !dir.exists() {
                return Ok(Value::Array(Vec::new()));
            }
            let entries = std::fs::read_dir(&dir)
                .map_err(|e| format!("Failed to read images directory: {e}"))?;
            let mut images = Vec::new();
            for entry in entries.flatten() {
                if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    let filename = entry.file_name().to_string_lossy().to_string();
                    if filename.ends_with(".partial") {
                        continue;
                    }
                    let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    images.push(json!({ "filename": filename, "size_bytes": size_bytes }));
                }
            }
            images.sort_by_key(|v| v["filename"].as_str().unwrap_or("").to_string());
            Ok(Value::Array(images))
        }
        Action::Download { url, filename } => {
            if !url.starts_with("https://") {
                return Err("Only HTTPS URLs are allowed".into());
            }
            let filename = match filename {
                Some(f) => {
                    validate_safe_name(&f)?;
                    f
                }
                None => {
                    let f = url
                        .rsplit('/')
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.split('?').next().unwrap_or(s).to_string())
                        .ok_or_else(|| {
                            "Cannot derive filename from URL; provide --filename".to_string()
                        })?;
                    validate_safe_name(&f)?;
                    f
                }
            };
            let dir = crate::paths::images_directory()
                .ok_or_else(|| "Could not resolve images directory".to_string())?;
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create images directory: {e}"))?;
            let partial = dir.join(format!("{filename}.partial"));
            let final_path = dir.join(&filename);
            let result = std::process::Command::new("curl")
                .args(["-fSL", "--output", &partial.to_string_lossy(), &url])
                .output()
                .map_err(|e| format!("Failed to run curl: {e}"))?;
            if !result.status.success() {
                let _ = std::fs::remove_file(&partial);
                return Err(format!(
                    "Download failed: {}",
                    String::from_utf8_lossy(&result.stderr)
                ));
            }
            std::fs::rename(&partial, &final_path).map_err(|e| {
                let _ = std::fs::remove_file(&partial);
                format!("Failed to finalize download: {e}")
            })?;
            let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
            Ok(json!({ "filename": filename, "size_bytes": size }))
        }
        Action::Delete { filename } => {
            validate_safe_name(&filename)?;
            let dir = crate::paths::images_directory()
                .ok_or_else(|| "Could not resolve images directory".to_string())?;
            let path = dir.join(&filename);
            if !path.is_file() {
                return Err(format!("Image not found: {filename}"));
            }
            std::fs::remove_file(&path).map_err(|e| format!("Failed to delete image: {e}"))?;
            Ok(json!({ "deleted": true, "filename": filename }))
        }
    }
}
