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
        let state = app
            .try_state::<crate::AppState>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        let conn = state.db.lock().map_err(|e| e.to_string())?;
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
    use std::sync::Mutex;

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
        app.manage(crate::AppState {
            db: Arc::new(Mutex::new(conn)),
            preset_cache: Mutex::new(Default::default()),
        });
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
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    // `AppHandle::restart` skips RunEvent::ExitRequested when it is called on the main thread
    // (tauri app.rs:588) — and a sync command may well run there — so the encoder is killed
    // here rather than relying on the exit handler. Killing twice is harmless; not killing
    // orphans HandBrakeCLI across the restart.
    if let Some(conv) = app.try_state::<Arc<crate::converter::ConverterState>>() {
        crate::converter::kill_active_child(&conv);
    }
    app.restart();
}
