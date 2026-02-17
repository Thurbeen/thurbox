fn main() {
    println!("cargo:rerun-if-env-changed=THURBOX_RELEASE_VERSION");

    let version = get_version();
    println!("cargo:rustc-env=THURBOX_VERSION={}", version);

    // Declare dev_build as a valid cfg so the compiler doesn't warn about it.
    println!("cargo:rustc-check-cfg=cfg(dev_build)");

    if version.contains("-dev") {
        println!("cargo:rustc-cfg=dev_build");
    }
}

fn get_version() -> String {
    // CI sets THURBOX_RELEASE_VERSION (e.g. "v0.7.0"); strip the "v" prefix
    if let Ok(v) = std::env::var("THURBOX_RELEASE_VERSION") {
        return v.strip_prefix('v').unwrap_or(&v).to_string();
    }

    // Fallback to Cargo.toml version for local development
    env!("CARGO_PKG_VERSION").to_string()
}
