//! Handbrake probe/scan routes: `/api/handbrake/*`, preset-suffix generation, and suffix-template
//! resolution. `detect_handbrake`/`list_handbrake_presets`/`validate_handbrake`/
//! `generate_preset_suffix` all shell out (to HandBrakeCLI or `which`/`where`) and run inside
//! `spawn_blocking`, mirroring the desktop's async commands
//! (`src-tauri/src/commands/handbrake.rs`). `resolve_suffix_template` is a pure string transform
//! and runs inline.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use convertbar_core::handbrake as hb;
use convertbar_core::handbrake::PresetMetadata;
use convertbar_core::types::HandbrakeStatus;

use super::{core_err, ServerState};

pub async fn detect_handbrake(State(s): State<ServerState>) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || hb::resolve_handbrake_path(&ctx)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => core_err(format!("task panicked: {join}")).into_response(),
    }
}

pub async fn list_handbrake_presets(State(s): State<ServerState>) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || {
        let path = hb::require_handbrake_path(&ctx)?;
        hb::list_presets(&path)
    })
    .await
    {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => core_err(format!("task panicked: {join}")).into_response(),
    }
}

pub async fn validate_handbrake(State(s): State<ServerState>) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || match hb::resolve_handbrake_path(&ctx)? {
        Some(p) => {
            let version = hb::handbrake_version(&p).unwrap_or_default();
            Ok(HandbrakeStatus {
                found: true,
                path: p,
                version,
            })
        }
        None => Ok(HandbrakeStatus {
            found: false,
            path: String::new(),
            version: String::new(),
        }),
    })
    .await
    {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => core_err(format!("task panicked: {join}")).into_response(),
    }
}

pub async fn generate_preset_suffix(
    State(s): State<ServerState>,
    Path(preset): Path<String>,
) -> Response {
    let ctx = s.ctx.clone();
    match tokio::task::spawn_blocking(move || {
        // Cache hit skips path resolution entirely (no DB lock, no `which`).
        {
            let cache = ctx.preset_cache.lock().map_err(|e| e.to_string())?;
            if let Some(metadata) = cache.get(&preset) {
                return Ok(metadata.clone());
            }
        }

        let handbrake_path = hb::require_handbrake_path(&ctx)?;
        hb::cached_preset_metadata(&ctx, &handbrake_path, &preset)
    })
    .await
    {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => core_err(e).into_response(),
        Err(join) => core_err(format!("task panicked: {join}")).into_response(),
    }
}

/// Resolve an output-suffix template against preset metadata. The settings preview calls this
/// instead of reimplementing the substitution in JS (the JS copy diverged, e.g. producing
/// `..h265` where this yields `.h265`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveSuffixTemplateBody {
    template: String,
    metadata: PresetMetadata,
}

pub async fn resolve_suffix_template(Json(b): Json<ResolveSuffixTemplateBody>) -> Json<String> {
    Json(hb::resolve_suffix_template(&b.template, &b.metadata))
}
