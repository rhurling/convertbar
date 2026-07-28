//! Converter-control routes: `/api/converter/*`. Every handler here is a short lock op (signal
//! send, flag flip, or a `Mutex` read) — same discipline as the desktop's sync commands
//! (`src-tauri/src/commands/converter.rs`). None run in `spawn_blocking`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use convertbar_core::control;
use convertbar_core::converter::LowDiskPause;

use super::{core_err, ServerState};

pub async fn start_queue(State(s): State<ServerState>) -> Response {
    match control::start_queue(&s.ctx) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn pause_conversion(State(s): State<ServerState>) -> Response {
    match control::pause_conversion(&s.ctx) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn resume_conversion(State(s): State<ServerState>) -> Response {
    match control::resume_conversion(&s.ctx) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn cancel_conversion(State(s): State<ServerState>) -> Response {
    match control::cancel_conversion(&s.ctx) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn pause_after_current(State(s): State<ServerState>) -> Response {
    match control::pause_after_current(&s.ctx) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn cancel_pause_after_current(State(s): State<ServerState>) -> Response {
    match control::cancel_pause_after_current(&s.ctx) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn get_pause_after_current(State(s): State<ServerState>) -> Json<bool> {
    Json(control::get_pause_after_current(&s.ctx))
}

pub async fn get_low_disk_pause(State(s): State<ServerState>) -> Json<Option<LowDiskPause>> {
    Json(control::get_low_disk_pause(&s.ctx))
}
