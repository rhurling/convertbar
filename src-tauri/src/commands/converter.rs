use convertbar_core::control;
use convertbar_core::ctx::Ctx;
use std::sync::Arc;
use tauri::{AppHandle, State};

use super::CommandError;
use crate::converter::ConverterState;

#[tauri::command]
pub fn start_queue(ctx: State<'_, Arc<Ctx>>) -> Result<(), CommandError> {
    Ok(control::start_queue(&ctx)?)
}

#[tauri::command]
pub fn pause_conversion(ctx: State<'_, Arc<Ctx>>) -> Result<(), CommandError> {
    Ok(control::pause_conversion(&ctx)?)
}

#[tauri::command]
pub fn resume_conversion(ctx: State<'_, Arc<Ctx>>) -> Result<(), CommandError> {
    Ok(control::resume_conversion(&ctx)?)
}

#[tauri::command]
pub fn cancel_conversion(ctx: State<'_, Arc<Ctx>>) -> Result<(), CommandError> {
    Ok(control::cancel_conversion(&ctx)?)
}

#[tauri::command]
pub fn pause_after_current(ctx: State<'_, Arc<Ctx>>) -> Result<(), CommandError> {
    Ok(control::pause_after_current(&ctx)?)
}

#[tauri::command]
pub fn cancel_pause_after_current(ctx: State<'_, Arc<Ctx>>) -> Result<(), CommandError> {
    Ok(control::cancel_pause_after_current(&ctx)?)
}

#[tauri::command]
pub fn get_pause_after_current(ctx: State<'_, Arc<Ctx>>) -> bool {
    control::get_pause_after_current(&ctx)
}

#[tauri::command]
pub fn get_low_disk_pause(ctx: State<'_, Arc<Ctx>>) -> Option<crate::converter::LowDiskPause> {
    control::get_low_disk_pause(&ctx)
}

#[derive(serde::Serialize)]
pub struct PlatformCapabilities {
    pub can_pause_process: bool,
}

#[tauri::command]
pub fn get_platform_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        can_pause_process: ConverterState::can_pause_process(),
    }
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}
