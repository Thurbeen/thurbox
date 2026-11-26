fn main() {
    println!("cargo:rerun-if-env-changed=THURBOX_RELEASE_VERSION");
    println!("cargo:rustc-check-cfg=cfg(dev_build)");
    let version = std::env::var("THURBOX_RELEASE_VERSION")
        .map(|v| v.strip_prefix('v').unwrap_or(&v).to_string())
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=RUNTIME_MUX_VERSION={version}");
    if version.contains("-dev") {
        println!("cargo:rustc-cfg=dev_build");
    }
}
