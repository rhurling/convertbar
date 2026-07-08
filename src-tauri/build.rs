fn main() {
    // The mock-runtime tests pull in comctl32-v6-only imports (TaskDialogIndirect via
    // tauri's dialog code). tauri-build embeds the Common-Controls v6 manifest into the
    // app binary only, so Windows test binaries die at load with
    // STATUS_ENTRYPOINT_NOT_FOUND (tauri-apps/tauri#11028; tauri's own env-var
    // workaround resolves a workspace-relative path and doesn't work downstream).
    // Embed the same manifest into test binaries.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS");
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV");
    if target_os.as_deref() == Ok("windows") && target_env.as_deref() == Ok("msvc") {
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-test-manifest.xml");
        // Double-colon syntax: tauri_build emits `cargo::` directives, and cargo
        // rejects mixing the legacy `cargo:` form into the same build-script output.
        println!("cargo::rerun-if-changed={}", manifest.display());
        println!("cargo::rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo::rustc-link-arg-tests=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
    tauri_build::build()
}
