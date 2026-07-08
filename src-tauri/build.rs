fn main() {
    // NOTE (Windows tests): the mock-runtime tests pull in comctl32-v6-only imports
    // (TaskDialogIndirect via tauri's dialog code), and without a Common-Controls v6
    // manifest the lib-test binary dies at load with STATUS_ENTRYPOINT_NOT_FOUND
    // (tauri-apps/tauri#11028). It can't be fixed here: `rustc-link-arg-tests` only
    // reaches [[test]] targets (this package has none) and a blanket `rustc-link-arg`
    // would double-manifest the app binary against tauri-build's bins-only resource.
    // Instead the Windows CI jobs run `cargo test --lib` with RUSTFLAGS embedding
    // windows-test-manifest.xml — see test.yml / test-windows.yml.
    tauri_build::build()
}
