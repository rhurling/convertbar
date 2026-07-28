use convertbar_core::ctx::Ctx;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::updater::{self, UpdateState, UpdaterRuntime};

#[tauri::command]
pub fn get_update_state(app: AppHandle) -> Result<UpdateState, String> {
    updater::build_state_public(&app)
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<(), String> {
    // Manual: forced regardless of mode, and never installs (U7). Refused outright while an
    // install is pending or installed — see `updater::manual_check_block`.
    updater::run_cycle(app, true).await
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    updater::install_pending(app, updater::InstallTrigger::UserRequested).await
}

/// Generic over the runtime so a `MockRuntime` test can drive the real command — the
/// `cancel_pending_version` call below is load-bearing and would otherwise be untestable.
#[tauri::command]
pub fn skip_update_version<R: tauri::Runtime>(
    app: AppHandle<R>,
    version: String,
) -> Result<(), String> {
    {
        let ctx = app
            .try_state::<Arc<Ctx>>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
        updater::set_skipped_version_public(&conn, &version);
    }

    if let Some(runtime) = app.try_state::<Arc<UpdaterRuntime>>() {
        if let Ok(mut a) = runtime.available.lock() {
            *a = None;
        }
    }
    // A skipped version must not stay queued behind the idle gate — the retry paths install a
    // pending update without re-consulting the skip list.
    updater::cancel_pending_version(&app, &version);
    // Emits the cleared state, so the panel drops the banner without a round trip.
    updater::clear_status(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::{AvailableUpdate, PendingInstall};

    #[test]
    fn skipping_a_version_cancels_the_install_pending_for_it() {
        // Drives the real command, so deleting its `cancel_pending_version` call turns this red:
        // without it the deferral stays live and the next queue drain installs the very version
        // the user just skipped (the retry paths do not re-consult the skip list).
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        app.manage(Ctx::new(
            conn,
            Arc::new(convertbar_core::events::TestSink::default()),
            Arc::new(convertbar_core::dispose::RecordingDisposer::default()),
            Arc::new(convertbar_core::handbrake::PanickingLocator),
        ));
        let runtime = Arc::new(UpdaterRuntime::default());
        *runtime.pending.lock().unwrap() = Some(PendingInstall {
            update: AvailableUpdate {
                version: "2.0.0".into(),
                date: None,
                notes: None,
            },
            user_requested: false,
        });
        app.manage(runtime.clone());

        // Skipping a different version leaves it alone.
        skip_update_version(app.handle().clone(), "1.9.0".to_string()).unwrap();
        assert!(runtime.pending.lock().unwrap().is_some());

        skip_update_version(app.handle().clone(), "2.0.0".to_string()).unwrap();
        assert!(
            runtime.pending.lock().unwrap().is_none(),
            "skipping the pending version must cancel the install waiting behind the idle gate"
        );
    }

    /// A child that outlives the test unless something kills it, so the assertion below is about
    /// a real process being reaped and not about a flag being flipped.
    fn long_running_child() -> std::process::Child {
        #[cfg(windows)]
        {
            // A cmd-internal busy loop, like converter's fake HandBrake: `timeout`/`pause` need a
            // console stdin, which the test harness does not guarantee.
            std::process::Command::new("cmd")
                .args(["/c", "for /l %i in (1,1,2000000000) do rem"])
                .spawn()
                .unwrap()
        }
        #[cfg(not(windows))]
        {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap()
        }
    }

    #[test]
    fn restarting_after_an_install_kills_the_active_encoder() {
        // Reachable with a live encoder: once an install completes `installing` clears, a watched
        // file can land and start the queue, and only then does the user press "Restart now".
        // `AppHandle::restart` on the main thread goes straight to cleanup_before_exit +
        // process::restart (tauri app.rs:589), skipping RunEvent::ExitRequested and so the kill in
        // lib.rs. Drop the kill here and HandBrakeCLI survives the restart, still writing into the
        // partial output while the next launch's auto-resume deletes that file and starts a second
        // encoder against the same path.
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let ctx = Ctx::new(
            rusqlite::Connection::open_in_memory().unwrap(),
            Arc::new(convertbar_core::events::TestSink::default()),
            Arc::new(convertbar_core::dispose::RecordingDisposer::default()),
            Arc::new(convertbar_core::handbrake::PanickingLocator),
        );
        let converter = ctx.converter.clone();
        let child = long_running_child();
        *converter.current_pid.lock().unwrap() = Some(child.id());
        *converter.current_child.lock().unwrap() = Some(child);
        app.manage(ctx);

        let mut restarted = false;
        restart_after_killing_encoder(app.handle(), || restarted = true);

        assert!(restarted, "the restart itself must still happen");
        assert!(
            converter
                .current_child
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_some(),
            "the encoder must be dead and reaped before the app restarts, or it outlives it"
        );
    }
}

/// The whole body of `restart_app` bar the restart itself, with the restart injected: calling
/// the real `AppHandle::restart` terminates the process, so this is the largest slice of the
/// command a test can drive.
///
/// `AppHandle::restart` skips RunEvent::ExitRequested when it is called on the main thread
/// (tauri app.rs:589) — and a sync command may well run there — so the encoder is killed here
/// rather than relying on the exit handler. Killing twice is harmless; not killing orphans
/// HandBrakeCLI across the restart.
fn restart_after_killing_encoder<R: tauri::Runtime>(app: &AppHandle<R>, restart: impl FnOnce()) {
    if let Some(ctx) = app.try_state::<Arc<Ctx>>() {
        crate::converter::kill_active_child(&ctx.converter);
    }
    restart();
}

/// Generic over the runtime for the same reason as `skip_update_version`: so a `MockRuntime`
/// test can drive the kill above.
#[tauri::command]
pub fn restart_app<R: tauri::Runtime>(app: AppHandle<R>) {
    restart_after_killing_encoder(&app, || {
        app.restart();
    });
}
