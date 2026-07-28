# Open Issues

## Docker-based web-UI version for server use

*(Premises re-verified 2026-07-28 against v1.0.0; corrections inline.)*

A headless, server-deployable build of ConvertBar with a browser UI instead of
the menu-bar app. The Rust core (`converter.rs`, `commands/queue.rs`, `db.rs`/SQLite,
`watcher.rs`, `handbrake.rs`) is portable and is already structured for reuse
(`run_queue` is generic over `tauri::Runtime` — so it is *parameterised over* Tauri
rather than free of it, but that is what already makes it mock-testable and is the
natural seam for a second head; `add_files_inner` / `add_files_to_db` split logic
from IPC) — so this is a new "head" on the existing core, not a rewrite.

Scope estimate: the original ~1–2 week MVP figure predates watched folders, the probe
cache, low-disk pause, and bad-source handling. A server head must now carry all of
those, so treat it as materially larger and re-estimate during the spec.

What changes: replace Tauri command handlers with an HTTP server (e.g. axum) plus
WebSocket/SSE for the ~25 `app.emit` progress/status events (`converter.rs`,
`add_progress.rs`, `watcher.rs`, `commands/*`); swap the frontend
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

**Status:** shipped — Plan 1 (workspace split, PR #124) + Plan 2 (server head). Remaining follow-ups tracked in the Plan 2 doc.
