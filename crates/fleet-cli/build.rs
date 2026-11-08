//! Compile the explicitly allowlisted, pinned starter into the executable.
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

fn main() {
    let root = Path::new("bundle/vega");
    println!("cargo:rerun-if-changed=bundle/vega");
    let manifest = fs::read_to_string(root.join("starter.toml")).expect("bundled starter manifest");
    let doc: toml::Value = toml::from_str(&manifest).expect("starter TOML");
    let provenance = fs::read_to_string(root.join("provenance.json")).expect("pinned provenance");
    let pin: serde_json::Value = serde_json::from_str(&provenance).expect("provenance JSON");
    let commit = pin["commit"].as_str().expect("source commit");
    assert!(
        commit.len() == 40
            && commit.bytes().all(|b| b.is_ascii_hexdigit())
            && commit != "0000000000000000000000000000000000000000",
        "exact source commit required"
    );
    assert_eq!(pin["repository"], "https://github.com/Thurbeen/vega");
    assert_eq!(doc["version"].as_integer(), Some(1));
    assert_eq!(doc["id"].as_str(), Some("vega"));
    let mut code = String::from("const FILES: &[(&str, &[u8])] = &[\n");
    let mut seen = std::collections::BTreeSet::new();
    for item in doc["files"].as_array().expect("explicit file allowlist") {
        let name = item.as_str().expect("file path");
        assert!(seen.insert(name), "duplicate starter path");
        assert!(
            !name.contains('\\')
                && Path::new(name)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "unsafe starter path"
        );
        let path = root.join(name).canonicalize().expect("allowlisted file");
        assert!(
            path.starts_with(root.canonicalize().unwrap()),
            "starter symlink escapes bundle"
        );
        let bytes = fs::read(&path).expect("starter asset");
        let hash: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            pin["files"][name].as_str(),
            Some(hash.as_str()),
            "starter asset hash differs from provenance: {name}"
        );
        code.push_str(&format!(
            "({name:?}, include_bytes!({:?})),\n",
            path.to_str().unwrap()
        ));
    }
    code.push_str("];\n");
    code.push_str(&format!("const MANIFEST: &str = {manifest:?};\n"));
    code.push_str(&format!("const PROVENANCE: &str = {provenance:?};\n"));
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("starter.rs"),
        code,
    )
    .unwrap();
}
