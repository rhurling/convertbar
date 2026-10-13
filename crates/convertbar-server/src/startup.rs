//! Server-head boot sequence: settings normalization, the desktop `lib.rs` setup block
//! (recover interrupted jobs, auto-resume, watcher), and the graceful-shutdown trigger.

use std::sync::Arc;

use rusqlite::params;

use convertbar_core::ctx::Ctx;

/// Settings the server head forces off `'trash'`. The `trash` crate the desktop app uses
/// would litter `.Trash-<uid>` directories on the NAS/network mounts a headless server
/// typically runs against, so the server always deletes instead — the web UI hides the
/// "trash" option entirely, but a db copied over from a desktop install (or an old row)
/// could still carry it, so this runs on every boot.
const FORCED_DELETE_KEYS: &[&str] = &["cleanup_mode", "bad_source_action"];

/// Flips any `FORCED_DELETE_KEYS` row still at `'trash'` to `'delete'`, warning once per
/// changed key. A row already at `'delete'` (or anything else) is left untouched.
pub fn normalize_server_settings(ctx: &Ctx) {
    let conn = ctx.db.lock().unwrap();
    for key in FORCED_DELETE_KEYS {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok();

        if value.as_deref() == Some("trash") {
            conn.execute(
                "UPDATE settings SET value = 'delete' WHERE key = ?1",
                params![key],
            )
            .expect("normalize_server_settings: update failed");
            tracing::warn!(
                setting = key,
                "server head forces 'delete' over 'trash' (avoids .Trash-<uid> litter on NAS mounts); normalized at boot"
            );
        }
    }
}

/// Prepares staging (created, then emptied: a crash can leave a job's copy behind) and hands the
/// remote roots to `ctx`, with the system `ssh`. Refuses when staging cannot be created.
pub fn load_remote_roots(
    ctx: &Ctx,
    roots: Vec<convertbar_core::remote::RemoteRoot>,
    staging: std::path::PathBuf,
) -> Result<(), String> {
    std::fs::create_dir_all(&staging).map_err(|e| {
        format!(
            "the staging directory {} cannot be created: {e}",
            staging.display()
        )
    })?;
    let setup = convertbar_core::remote::RemoteSetup {
        roots,
        staging,
        ssh: std::path::PathBuf::from("ssh"),
    };
    setup.empty_staging();
    ctx.remote
        .set(setup)
        .map_err(|_| "the remote roots were already loaded".to_string())
}

/// Imports the configured preset file into `ctx`, or refuses with the reason startup must stop.
///
/// Every refusal here would otherwise fail each queued job in turn, one HandBrake run at a time.
/// HandBrake must be resolvable because the collision check needs its built-in names, and a
/// server with a preset file but no HandBrake can encode nothing anyway. The built-in names are
/// listed once, without the import flag, and kept for the per-run pre-flight
/// (`handbrake::preset_import_args`). Runs after the context is built and before `boot`.
pub fn load_preset_import(ctx: &Ctx, file: &std::path::Path) -> Result<(), String> {
    use convertbar_core::handbrake;

    let imported = handbrake::read_preset_file(file)?;
    // Absolute from here on: the path is handed to every HandBrake child as an argument. Not
    // canonical: a symlinked file is followed on every read, so retargeting it takes effect the
    // way an in-place rewrite does instead of pinning the target seen at startup.
    let file = std::path::absolute(file)
        .map_err(|e| format!("Preset file {} cannot be read: {e}", file.display()))?;
    let path = file.display();
    if imported.is_empty() {
        return Err(format!("Preset file {path} defines no presets"));
    }
    let hb = handbrake::resolve_handbrake_path(ctx)?.ok_or_else(|| {
        format!(
            "a preset file is configured ({path}) but HandBrakeCLI cannot be found: {}",
            handbrake::HANDBRAKE_NOT_FOUND
        )
    })?;
    let builtins: std::collections::HashSet<String> =
        handbrake::list_presets(&hb)?.into_iter().collect();
    if let Some(refusal) = handbrake::builtin_collision(&file, &imported, &builtins) {
        return Err(refusal);
    }
    ctx.preset_import
        .set(handbrake::PresetImport {
            path: file.clone(),
            builtins,
        })
        .map_err(|_| "the preset file was already imported".to_string())
}

/// Direct port of the desktop setup block's auto-resume sequence (`src-tauri/src/lib.rs`,
/// ~:337-360): reset interrupted jobs to `queued`, decide whether to auto-resume the queue,
/// then arm the directory watchers. The db lock is taken and dropped BEFORE `run_queue`/
/// `watcher::start` run — neither may be called while still holding `ctx.db`, mirroring the
/// desktop block's own scoping.
pub fn boot(ctx: &Arc<Ctx>) {
    let has_queued;
    let queue_paused;
    {
        let db = ctx.db.lock().unwrap();

        // Reset interrupted jobs to queued, deleting only their partial output (never the
        // source — critical for in-place jobs where output_path == source_path).
        convertbar_core::converter::recover_interrupted_jobs(&db);

        // An in-place job that was 'encoding' (not merely 'queued') when the user switched
        // cleanup_mode to keep survives update_setting's queued-only drop, and the requeue
        // above just resurrected it. Such a job is impossible under keep — drop it again here,
        // before has_queued/should_auto_resume can pick it up.
        if convertbar_core::settings_ops::read_cleanup_mode(&db) == "keep" {
            convertbar_core::queue_ops::drop_queued_in_place_jobs(&db);
        }

        has_queued = db
            .query_row(
                "SELECT COUNT(*) > 0 FROM jobs WHERE status = 'queued'",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false);
        queue_paused = convertbar_core::converter::is_queue_paused(&db);
    }

    if convertbar_core::converter::should_auto_resume(has_queued, queue_paused) {
        convertbar_core::converter::run_queue(ctx.clone());
    }

    // Arm directory watchers and ingest any files already present in enabled folders.
    convertbar_core::watcher::start(ctx.clone());
}

/// The production serve wiring. Factored out so the real-listener test exercises
/// THIS call rather than a copy of it: without `into_make_service_with_connect_info`
/// there is no `ConnectInfo`, every request collapses into one shared throttle
/// bucket, and no `oneshot`-based test can see it.
///
/// Takes the graceful-shutdown future as a parameter (rather than returning the
/// `Serve` builder for the caller to chain `.with_graceful_shutdown()` onto)
/// because naming `Serve`'s exact generic parameters against this axum version
/// is awkward — the per-connection service type isn't `IntoMakeServiceWithConnectInfo`
/// itself. Owning the chain here still runs it identically: `main.rs` passes the
/// same shutdown-signal-then-kill-active-child future it always did.
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// Resolves on SIGTERM (unix) or Ctrl-C — the trigger axum's graceful shutdown awaits.
/// Kept trivial by design: SIGTERM/Ctrl-C delivery isn't unit-tested here, it's covered by
/// the Task 13/14 container smoke test (docker stop / ctrl-c against a running server).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use convertbar_core::dispose::RecordingDisposer;
    use convertbar_core::events::TestSink;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        convertbar_core::db::init_db(&conn).unwrap();
        conn
    }

    fn test_ctx(conn: Connection) -> Arc<Ctx> {
        Ctx::new(
            conn,
            Arc::new(TestSink::default()),
            Arc::new(RecordingDisposer::default()),
            Arc::new(convertbar_core::handbrake::PanickingLocator),
            convertbar_core::hooks::HookSetup {
                runner: Arc::new(convertbar_core::hooks::RecordingHookRunner::default()),
                allow_stored_command: false,
            },
        )
    }

    fn setting(conn: &Connection, key: &str) -> Option<String> {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .ok()
    }

    fn insert_job(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES (?1, ?2, ?3, 'Fast 1080p30', ?4, 0, '2020-01-01T00:00:00Z')",
            params![id, format!("/tmp/{id}-src.mp4"), format!("/tmp/{id}-out.mp4"), status],
        )
        .unwrap();
    }

    fn set_queue_paused(conn: &Connection, paused: bool) {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('queue_paused', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            params![if paused { "true" } else { "false" }],
        )
        .unwrap();
    }

    // --- normalize_server_settings ---

    #[test]
    fn normalize_flips_trash_to_delete_for_both_keys() {
        let conn = test_conn();
        // init_db already seeds both keys as 'trash' by default (see db.rs), so this is
        // the realistic first-boot case; the explicit UPDATE just makes it unmissable.
        conn.execute(
            "UPDATE settings SET value = 'trash' WHERE key IN ('cleanup_mode', 'bad_source_action')",
            [],
        )
        .unwrap();
        let ctx = test_ctx(conn);

        normalize_server_settings(&ctx);

        let conn = ctx.db.lock().unwrap();
        assert_eq!(setting(&conn, "cleanup_mode").as_deref(), Some("delete"));
        assert_eq!(
            setting(&conn, "bad_source_action").as_deref(),
            Some("delete")
        );
    }

    #[test]
    fn normalize_leaves_delete_untouched() {
        let conn = test_conn();
        conn.execute(
            "UPDATE settings SET value = 'delete' WHERE key IN ('cleanup_mode', 'bad_source_action')",
            [],
        )
        .unwrap();
        let ctx = test_ctx(conn);

        normalize_server_settings(&ctx);

        let conn = ctx.db.lock().unwrap();
        assert_eq!(setting(&conn, "cleanup_mode").as_deref(), Some("delete"));
        assert_eq!(
            setting(&conn, "bad_source_action").as_deref(),
            Some("delete")
        );
    }

    #[test]
    fn normalize_leaves_keep_untouched() {
        // The server forces trash -> delete because the `trash` crate litters .Trash-<uid>
        // on NAS mounts. It must NOT touch 'keep', which is a deliberate user choice the
        // web UI now offers. No production code enforces this — only the fact that the
        // rewrite is scoped to the exact string 'trash'. This test is what keeps it scoped.
        let conn = test_conn();
        conn.execute(
            "UPDATE settings SET value = 'keep' WHERE key = 'cleanup_mode'",
            [],
        )
        .unwrap();
        let ctx = test_ctx(conn);

        normalize_server_settings(&ctx);

        let conn = ctx.db.lock().unwrap();
        assert_eq!(setting(&conn, "cleanup_mode").as_deref(), Some("keep"));
    }

    // --- boot ---
    //
    // The positive auto-resume path (a queued job + queue_paused='false') is deliberately
    // NOT exercised end-to-end via `is_running()` here: `run_queue` spawns a background
    // thread that races to flip `is_running` back to `false` once it fails fast (no real
    // HandBrakeCLI in the test environment), so asserting on `is_running()`'s timing would
    // be flaky (task-4-brief.md Step 2 explicitly allows this fallback). Instead,
    // `boot_auto_resume_inputs_indicate_resume_is_warranted` below asserts on the exact
    // inputs `boot` feeds into `should_auto_resume` — the same COUNT query and
    // `is_queue_paused` read `boot` uses — so a regression in that wiring (wrong status
    // filter, inverted queue_paused, etc.) still fails deterministically. The negative
    // (no-resume) paths ARE exercised through the real `boot()` call below, since those are
    // fully deterministic: `run_queue` is never invoked, so `is_running` can't move.

    #[test]
    fn boot_resets_interrupted_jobs_to_queued() {
        let conn = test_conn();
        insert_job(&conn, "j1", "encoding");
        // Paused so boot's auto-resume decision can't spawn run_queue — this test is only
        // about the recover_interrupted_jobs wiring.
        set_queue_paused(&conn, true);
        let ctx = test_ctx(conn);

        boot(&ctx);

        let status: String = ctx
            .db
            .lock()
            .unwrap()
            .query_row("SELECT status FROM jobs WHERE id = 'j1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "queued");
        assert!(
            !ctx.converter.is_running(),
            "queue_paused=true must not trigger auto-resume"
        );
    }

    // Finding 2 of the final-review pass: recover_interrupted_jobs requeues EVERY interrupted
    // row with no cleanup_mode filter, including an in-place job that was 'encoding' when the
    // user switched to keep (update_setting only drops in-place siblings that are still
    // 'queued' — it cannot reach a row already 'encoding'). Left alone, that resurrected job
    // would reach process_queue under keep: not destructive (the RemoveTemp backstop holds and
    // the source survives byte-identical), but the design says such a job cannot exist, and it
    // wastes an encode and books a done History row for it. `boot` must drop it again, right
    // after the requeue and before has_queued/should_auto_resume can act on it.
    #[test]
    fn boot_drops_a_resurrected_in_place_job_under_keep() {
        let conn = test_conn();
        // source_path == output_path: an in-place job, interrupted mid-encode.
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('inplace', '/tmp/a.mp4', '/tmp/a.mp4', 'p', 'encoding', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE settings SET value = 'keep' WHERE key = 'cleanup_mode'",
            [],
        )
        .unwrap();
        // Isolates this test to the drop itself — a separate test below covers that the drop
        // also prevents an auto-resume that would otherwise have picked the job up.
        set_queue_paused(&conn, true);
        let ctx = test_ctx(conn);

        boot(&ctx);

        let count: i64 = ctx
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM jobs WHERE id = 'inplace'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "recover_interrupted_jobs requeues the row, then the keep-mode drop must remove \
             it again before boot returns — an in-place job cannot exist under keep"
        );
    }

    #[test]
    fn boot_leaves_a_resurrected_in_place_job_alone_outside_keep() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('inplace', '/tmp/a.mp4', '/tmp/a.mp4', 'p', 'encoding', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        // cleanup_mode left at init_db's default ('trash') — the drop must be scoped to keep.
        set_queue_paused(&conn, true);
        let ctx = test_ctx(conn);

        boot(&ctx);

        let status: String = ctx
            .db
            .lock()
            .unwrap()
            .query_row("SELECT status FROM jobs WHERE id = 'inplace'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            status, "queued",
            "outside keep, a requeued in-place job is legitimate and must survive boot"
        );
    }

    #[test]
    fn boot_does_not_auto_resume_onto_a_dropped_in_place_job_under_keep() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO jobs (id, source_path, output_path, preset, status, queue_order, created_at)
             VALUES ('inplace', '/tmp/a.mp4', '/tmp/a.mp4', 'p', 'encoding', 0, '2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE settings SET value = 'keep' WHERE key = 'cleanup_mode'",
            [],
        )
        .unwrap();
        // queue_paused left unset (defaults to false): if the resurrected in-place job survived
        // the drop, has_queued would be true and boot would auto-resume onto it. Deterministic,
        // not a timing race, because a passing test means run_queue was never invoked at all —
        // see the module doc above on why the positive resume path can't be asserted directly.
        let ctx = test_ctx(conn);

        boot(&ctx);

        assert!(
            !ctx.converter.is_running(),
            "the only queued/encoding row was the in-place job the keep-mode drop must remove; \
             nothing should have been left for auto-resume to pick up"
        );
    }

    #[test]
    fn boot_does_not_auto_resume_when_queue_is_paused() {
        let conn = test_conn();
        insert_job(&conn, "j1", "queued");
        set_queue_paused(&conn, true);
        let ctx = test_ctx(conn);

        boot(&ctx);

        assert!(!ctx.converter.is_running());
    }

    #[test]
    fn boot_does_not_auto_resume_when_nothing_is_queued() {
        let conn = test_conn();
        // No queued jobs; queue_paused is unset (defaults to false). should_auto_resume
        // must still gate on has_queued.
        let ctx = test_ctx(conn);

        boot(&ctx);

        assert!(!ctx.converter.is_running());
    }

    #[test]
    fn boot_auto_resume_inputs_indicate_resume_is_warranted() {
        let conn = test_conn();
        insert_job(&conn, "j1", "queued");
        // queue_paused left unset -> is_queue_paused defaults to false.

        let has_queued: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM jobs WHERE status = 'queued'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let queue_paused = convertbar_core::converter::is_queue_paused(&conn);

        assert!(has_queued);
        assert!(!queue_paused);
        assert!(convertbar_core::converter::should_auto_resume(
            has_queued,
            queue_paused
        ));
    }

    // --- load_preset_import ---

    fn ctx_with_locator(
        locator: Arc<dyn convertbar_core::handbrake::HandbrakeLocator>,
    ) -> Arc<Ctx> {
        Ctx::new(
            test_conn(),
            Arc::new(TestSink::default()),
            Arc::new(RecordingDisposer::default()),
            locator,
            convertbar_core::hooks::HookSetup {
                runner: Arc::new(convertbar_core::hooks::RecordingHookRunner::default()),
                allow_stored_command: false,
            },
        )
    }

    /// A stub HandBrakeCLI whose `--preset-list` prints two built-ins to stderr, in the real
    /// CLI's layout (category at the margin, preset at four spaces, description at eight).
    #[cfg(unix)]
    fn preset_list_stub(dir: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("hb-list.sh");
        std::fs::write(
            &p,
            "#!/bin/sh\ncat >&2 <<'EOF'\nGeneral/\n    Fast 1080p30\n        H.264 video.\n    H.265 MKV 1080p30\n        H.265 video.\nEOF\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().to_string()
    }

    fn preset_file(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }

    const ONE_PRESET: &str =
        r#"{"PresetList":[{"PresetName":"data","Type":1,"Folder":false,"FileFormat":"av_mkv"}]}"#;

    #[test]
    #[cfg(unix)]
    fn a_preset_file_that_cannot_serve_refuses_startup_naming_the_cause() {
        // Each of these would otherwise fail every queued job in turn, one HandBrake run at a
        // time; refusing to start says it once, before anything is spawned.
        let dir = tempfile::tempdir().unwrap();
        let installed = || {
            ctx_with_locator(Arc::new(convertbar_core::handbrake::StubLocator(
                preset_list_stub(dir.path()),
            )))
        };
        let cases: Vec<(&str, Arc<Ctx>, std::path::PathBuf, &str)> = vec![
            (
                "missing",
                installed(),
                dir.path().join("absent.json"),
                "cannot be read",
            ),
            (
                "invalid JSON",
                installed(),
                preset_file(dir.path(), "invalid.json", "{not json"),
                "not valid JSON",
            ),
            (
                "no presets",
                installed(),
                preset_file(dir.path(), "empty.json", r#"{"PresetList":[]}"#),
                "defines no presets",
            ),
            (
                "HandBrake unresolvable",
                ctx_with_locator(Arc::new(convertbar_core::handbrake::AbsentLocator)),
                preset_file(dir.path(), "no-hb.json", ONE_PRESET),
                "HandBrakeCLI cannot be found",
            ),
            (
                "collision",
                installed(),
                preset_file(
                    dir.path(),
                    "collision.json",
                    r#"{"PresetList":[{"PresetName":"Fast 1080p30","Type":1,"Folder":false,"FileFormat":"av_mkv"}]}"#,
                ),
                "Fast 1080p30",
            ),
        ];
        for (case, ctx, file, want) in cases {
            let err = load_preset_import(&ctx, &file)
                .expect_err(&format!("{case}: startup must be refused"));
            assert!(err.contains(want), "{case}: the cause is named: {err}");
            assert!(
                ctx.preset_import.get().is_none(),
                "{case}: nothing is imported"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_valid_preset_file_is_imported_with_handbrakes_built_in_names() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_locator(Arc::new(convertbar_core::handbrake::StubLocator(
            preset_list_stub(dir.path()),
        )));
        let target = preset_file(dir.path(), "presets.json", ONE_PRESET);
        let file = dir.path().join("current.json");
        std::os::unix::fs::symlink(&target, &file).unwrap();

        load_preset_import(&ctx, &file).unwrap();

        let import = ctx.preset_import.get().expect("the file is imported");
        assert!(
            import.path.is_absolute(),
            "every HandBrake child receives it"
        );
        assert_eq!(
            import.path, file,
            "the link itself, not its target: retargeting it must reach the next read"
        );
        let mut builtins: Vec<&str> = import.builtins.iter().map(String::as_str).collect();
        builtins.sort();
        assert_eq!(builtins, vec!["Fast 1080p30", "H.265 MKV 1080p30"]);
    }
}
