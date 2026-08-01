use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::config::AuthMode;

use super::{blocking_response, ServerState};

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AppInfo {
    pub version: String,
    pub head: String,
    pub can_pause_process: bool,
    pub auth_required: bool,
    /// Lets the file-browser modal start (and confine breadcrumb up-navigation) at a
    /// configured root instead of always guessing "/" — which 403s on any deployment that
    /// restricts `CONVERTBAR_BROWSE_ROOTS` (see `routes::fs`).
    ///
    /// Canonicalized, because `/api/fs/list` answers in canonical paths and the client compares
    /// the two: it checks which root contains the directory it is in, and slices that root off
    /// to build the breadcrumb. A root advertised as the raw config string is a prefix of
    /// nothing the client will ever be shown the moment the path involves a symlink.
    pub browse_roots: Vec<String>,
}

pub async fn get_app_info(State(state): State<ServerState>) -> Response {
    // `blocking_response`, not a plain async fn: canonicalizing touches the filesystem once per
    // root, and this route is hit on every page load. A root on an unresponsive network mount
    // would otherwise stall an executor thread rather than just its own request.
    blocking_response(move || {
        Json(AppInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            head: "server".to_string(),
            can_pause_process: cfg!(unix),
            auth_required: !matches!(state.config.auth, AuthMode::Open),
            browse_roots: state
                .config
                .browse_roots
                .iter()
                .map(|p| {
                    // A root that does not resolve is advertised as configured rather than
                    // dropped: it can legitimately not exist yet (a NAS mount that attaches
                    // after startup — see `routes::fs`), and a picker offered no roots at all
                    // is worse than one offered a root that is not browsable yet.
                    std::fs::canonicalize(p)
                        .unwrap_or_else(|_| p.clone())
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
        })
        .into_response()
    })
    .await
}
