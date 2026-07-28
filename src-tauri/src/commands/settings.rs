use convertbar_core::ctx::Ctx;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use crate::types::Settings;

#[tauri::command]
pub fn get_settings(app: AppHandle, ctx: State<'_, Arc<Ctx>>) -> Result<Settings, String> {
    let mut settings = convertbar_core::settings_ops::get_settings(&ctx)?;
    // Autostart plugin is the source of truth on desktop; core returns the stored value.
    settings.launch_at_login = app
        .autolaunch()
        .is_enabled()
        .unwrap_or(settings.launch_at_login);
    Ok(settings)
}

#[tauri::command]
pub fn update_setting(
    app: AppHandle,
    ctx: State<'_, Arc<Ctx>>,
    key: String,
    value: String,
) -> Result<(), String> {
    convertbar_core::settings_ops::update_setting(&ctx, &key, &value)?;
    // Sync autostart state with the plugin
    if key == "launch_at_login" {
        let autostart = app.autolaunch();
        if value == "true" {
            let _ = autostart.enable();
        } else {
            let _ = autostart.disable();
        }
    }
    Ok(())
}

#[tauri::command]
pub fn get_preset_suffix(ctx: State<'_, Arc<Ctx>>, preset: String) -> Result<String, String> {
    let conn = ctx.db.lock().map_err(|e| e.to_string())?;
    Ok(convertbar_core::settings_ops::read_suffix_template(
        &conn, &preset,
    ))
}

#[tauri::command]
pub fn set_preset_suffix(
    ctx: State<'_, Arc<Ctx>>,
    preset: String,
    suffix: String,
) -> Result<(), String> {
    convertbar_core::settings_ops::set_preset_suffix(&ctx, &preset, &suffix)
}
