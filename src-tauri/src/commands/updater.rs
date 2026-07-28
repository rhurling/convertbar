use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::updater::{self, UpdateState, UpdaterRuntime};

#[tauri::command]
pub fn get_update_state(app: AppHandle) -> Result<UpdateState, String> {
    updater::build_state_public(&app)
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<(), String> {
    // Manual: forced regardless of mode, and never installs (U7).
    updater::run_cycle(app, true).await;
    Ok(())
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    updater::install_pending(app).await
}

#[tauri::command]
pub fn skip_update_version(app: AppHandle, version: String) -> Result<(), String> {
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
    // Emits the cleared state, so the panel drops the banner without a round trip.
    updater::clear_status(&app);
    Ok(())
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
