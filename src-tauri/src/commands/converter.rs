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
    pub priority_is_group_scoped: bool,
}

#[tauri::command]
pub fn get_platform_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        can_pause_process: ConverterState::can_pause_process(),
        priority_is_group_scoped: cfg!(target_os = "linux"),
    }
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `priority_is_group_scoped` must name Linux specifically (`cfg!(target_os = "linux")`),
    /// not `cfg!(unix)` — the easy copy-paste from `can_pause_process` one line above, since
    /// that field genuinely is `cfg!(unix)`. Swapping the two would show macOS users a caveat
    /// that only describes Linux's cgroup-scoped scheduling. Pinned on macOS specifically: it's
    /// the platform where `cfg!(unix)` (true) and `cfg!(target_os = "linux")` (false) actually
    /// disagree, so this is where the mutation is observable.
    #[test]
    #[cfg(target_os = "macos")]
    fn priority_is_not_group_scoped_on_macos() {
        assert!(!get_platform_capabilities().priority_is_group_scoped);
    }
}
