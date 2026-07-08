---
name: add-tauri-plugin
description: Use when adding a Tauri plugin to ConvertBar (autostart, updater, notification, dialog, etc.) — runs the official add command and wires up the matching per-call ACL permission so the plugin's frontend API actually works at runtime.
disable-model-invocation: true
---

# Add a Tauri Plugin

## Steps

1. **Add the plugin** with the official command — it handles `Cargo.toml`, `lib.rs` registration, the npm dependency, and capabilities in one step:
   ```
   npm run tauri add {plugin}
   ```
2. **Add per-call ACL permissions.** ConvertBar uses explicit per-call permissions in `src-tauri/capabilities/default.json` (no `:default` bundles). For every frontend API call you use, add the specific permission (e.g. `notification:allow-notify`, `autostart:allow-enable`) — one per call, so removing one never silently breaks another.
3. **Backend-only APIs need no permission** — anything called only from Rust (tray, window management, notifications sent from Rust, window-state persistence) does not require an ACL entry. App-defined `#[tauri::command]` functions are likewise ACL-exempt; only `core:`/`plugin:` APIs invoked from the frontend need a grant.
4. **Verify:** run the app and exercise the new call. An `ACL`/`not allowed` error at runtime means a permission is missing from `default.json`. Consider running the `acl-auditor` agent to confirm coverage.

## Reference

See `CLAUDE.md` → "Adding Tauri Plugins" and "Permissions (ACL)".
