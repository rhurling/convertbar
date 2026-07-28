//! Settings and preset-suffix routes: `/api/settings*`, `/api/presets/{preset}/suffix`. All are
//! short DB-lock calls and run inline — none need `spawn_blocking`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use convertbar_core::settings_ops;

use super::{core_err, ServerState};

pub async fn get_settings(State(s): State<ServerState>) -> Response {
    match settings_ops::get_settings(&s.ctx) {
        Ok(v) => Json(v).into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettingBody {
    value: String,
}

pub async fn update_setting(
    State(s): State<ServerState>,
    Path(key): Path<String>,
    Json(b): Json<UpdateSettingBody>,
) -> Response {
    match settings_ops::update_setting(&s.ctx, &key, &b.value) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}

pub async fn get_preset_suffix(
    State(s): State<ServerState>,
    Path(preset): Path<String>,
) -> Response {
    match s.ctx.db.lock() {
        Ok(conn) => Json(settings_ops::read_suffix_template(&conn, &preset)).into_response(),
        Err(e) => core_err(e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPresetSuffixBody {
    suffix: String,
}

pub async fn set_preset_suffix(
    State(s): State<ServerState>,
    Path(preset): Path<String>,
    Json(b): Json<SetPresetSuffixBody>,
) -> Response {
    match settings_ops::set_preset_suffix(&s.ctx, &preset, &b.suffix) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => core_err(e).into_response(),
    }
}
