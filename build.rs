fn main() {
    let version = get_version();
    println!("cargo:rustc-env=THURBOX_VERSION={}", version);

    // Declare dev_build as a valid cfg so the compiler doesn't warn about it.
    println!("cargo:rustc-check-cfg=cfg(dev_build)");

    if version.contains("-dev") {
        println!("cargo:rustc-cfg=dev_build");
    }
}

fn get_version() -> String {
    // Use THURBOX_RELEASE_VERSION if set (from CI release workflow)
    if let Ok(release_version) = std::env::var("THURBOX_RELEASE_VERSION") {
        return release_version;
    }

    // Fallback to Cargo.toml version for local development
    env!("CARGO_PKG_VERSION").to_string()
}
