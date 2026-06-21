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

## True in-place re-encode (temp file + atomic rename)

The output extension is always forced to `.mp4` (`format!("{}{}.mp4", stem,
suffix)`, `src-tauri/src/commands/queue.rs:199`) and HandBrake writes straight to
the final path — there is no temp+rename (`src-tauri/src/converter.rs:274`). With
an empty suffix: non-mp4 sources convert to a distinct `.mp4`, then
`decide_cleanup` (`src-tauri/src/converter.rs:56`) trashes/deletes the larger of
the two; but an `.mp4` source would produce output==source, which the pre-existing
`if output_path.exists() { continue; }` guard (`queue.rs:202`) silently drops from
the queue — no error and no corruption, but also no user feedback.

So self-overwrite is structurally impossible, yet a real in-place re-encode
(same name + same extension) is **not** supported, and the silent skip is a UX
rough edge.

**Proposed fix:** add an explicit temp-file → atomic-rename in-place path, and/or
surface a warning when a file is silently skipped because its output already
exists.

**Next step:** brainstorm → spec → implementation plan (not started).

## Skip queued files by source codec + resolution

Skip a file when its source codec and resolution already match or exceed the
target preset (e.g. target h265 1080p, source h265 720p → skip). Rough estimate
~1–2 days.

The app does **not** probe source files today — no ffprobe, no HandBrake `--scan`.
It only knows the *target preset's* codec/resolution via `classify_preset`
(`src-tauri/src/handbrake.rs:89`), used for filename suffixes — so this is mostly
new source-introspection work. Reusable: the per-file skip loop in
`add_files_to_db` (`queue.rs:174`) next to the existing `skip_already_converted`
toggle (`queue.rs:109`); the async/lock split (slow shelling-out belongs in
`add_files_inner` outside the DB lock, mirroring suffix resolution at
`queue.rs:122`); a pure, table-testable comparison fn like `decide_cleanup`. New:
a `HandBrakeCLI --scan --json -i <file>` probe + parse, the comparison policy, and
a settings toggle + UI checkbox.

**AV1 / open product decision:** a naive same-codec rule would re-encode AV1→h265
(likely larger, lower quality); `decide_cleanup` would then keep the original AV1
and mark the job "skipped" — correct result, but wasted CPU, which this feature
exists to avoid. A better rule compares codec *efficiency rank* (av1 ≈ h265 ≈ vp9
> h264 > mpeg). But skipping equal-or-better codecs conflicts with compatibility-
driven transcodes (e.g. an old device that can't decode AV1), so a default must be
chosen and made a toggle.

**Next step:** brainstorm → spec → implementation plan (not started). Settle the
codec-ranking-vs-compatibility default first.
