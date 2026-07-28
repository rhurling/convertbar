use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::config::AuthMode;

use super::ServerState;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AppInfo {
    pub version: String,
    pub head: String,
    pub can_pause_process: bool,
    pub auth_required: bool,
}

pub async fn get_app_info(State(state): State<ServerState>) -> Json<AppInfo> {
    Json(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        head: "server".to_string(),
        can_pause_process: cfg!(unix),
        auth_required: !matches!(state.config.auth, AuthMode::Open),
    })
}
