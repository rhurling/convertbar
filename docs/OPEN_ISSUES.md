# Open Issues

## Docker-based web-UI version for server use

A headless, server-deployable build of ConvertBar with a browser UI instead of
the menu-bar app. The Rust core (`converter.rs`, `queue.rs`, `db.rs`/SQLite,
`watcher.rs`, `handbrake.rs`) is portable and nearly Tauri-free, and is already
structured for reuse (`run_queue` takes plain args; `add_files_inner` /
`add_files_to_db` split logic from IPC) — so this is a new "head" on the existing
core, not a rewrite. Rough MVP estimate ~1–2 weeks.

What changes: replace Tauri command handlers with an HTTP server (e.g. axum) plus
WebSocket/SSE for the ~dozen `app.emit` progress/status events; swap the frontend
transport in `src/hooks` (invoke→fetch, listen→WS); drop tray/window/dialog/
updater UI. The watched-folders feature is the natural primary input on a server
(mounted volume). Gotchas: no user filesystem → need a web file browser over a
mounted volume (upload impractical for video); `trash::delete` won't work headless
(use `cleanup_mode=delete`); HandBrake HW accel needs container device passthrough
(QSV/VAAPI/NVENC) or falls back to slow software x265; no auth today.

**Proposed approach:** Cargo workspace split — `convertbar-core` lib +
`convertbar-tauri` + `convertbar-server` — to avoid forking the logic. Settle the
input model (watched-folders + web file browser vs. upload) and auth posture
during the spec.

**Next step:** brainstorm → spec → implementation plan (not started).
