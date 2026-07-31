fn main() {
    // `has_credential_store` marks the targets where `keyring` is a real
    // dependency with a native backend enabled (see Cargo.toml). Everything
    // else — Android today — falls back to a sealed file, because keyring
    // would otherwise resolve to its in-memory mock and protect nothing.
    println!("cargo::rustc-check-cfg=cfg(has_credential_store)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "macos" | "ios" | "windows" | "linux") {
        println!("cargo::rustc-cfg=has_credential_store");
    }

    tauri_build::build();
}
