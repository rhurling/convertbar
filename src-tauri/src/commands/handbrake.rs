use std::sync::Arc;
use tauri::State;

use crate::handbrake as hb;
use crate::handbrake::PresetMetadata;
use crate::types::HandbrakeStatus;
use convertbar_core::ctx::Ctx;

// All four commands below reach a subprocess (HandBrakeCLI or `which`/`where`); as
// sync commands they ran on the main thread and stalled the UI for the subprocess
// duration. async + spawn_blocking moves them off it, mirroring add_files.

#[tauri::command]
pub async fn detect_handbrake(ctx: State<'_, Arc<Ctx>>) -> Result<Option<String>, String> {
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || hb::resolve_handbrake_path(&ctx))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_handbrake_presets(ctx: State<'_, Arc<Ctx>>) -> Result<Vec<String>, String> {
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || match hb::resolve_handbrake_path(&ctx)? {
        Some(p) => hb::list_presets(&p),
        None => Err("HandBrakeCLI not found".to_string()),
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn generate_preset_suffix(
    ctx: State<'_, Arc<Ctx>>,
    preset: String,
) -> Result<PresetMetadata, String> {
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Cache hit skips path resolution entirely (no DB lock, no `which`).
        {
            let cache = ctx.preset_cache.lock().map_err(|e| e.to_string())?;
            if let Some(metadata) = cache.get(&preset) {
                return Ok(metadata.clone());
            }
        }

        let handbrake_path = hb::resolve_handbrake_path(&ctx)?.ok_or("HandBrakeCLI not found")?;
        hb::cached_preset_metadata(&ctx, &handbrake_path, &preset)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn validate_handbrake(ctx: State<'_, Arc<Ctx>>) -> Result<HandbrakeStatus, String> {
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || match hb::resolve_handbrake_path(&ctx)? {
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
    .map_err(|e| e.to_string())?
}

/// Resolve an output-suffix template against preset metadata. The settings preview
/// invokes this instead of reimplementing the substitution in JS (the JS copy diverged,
/// e.g. producing `..h265` where this yields `.h265`).
#[tauri::command]
pub fn resolve_suffix_template(template: String, metadata: PresetMetadata) -> String {
    hb::resolve_suffix_template(&template, &metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(codec: &str) -> PresetMetadata {
        PresetMetadata {
            codec: codec.to_string(),
            resolution: "1080p".to_string(),
            quality: "q30".to_string(),
            preset: "test".to_string(),
            device: String::new(),
        }
    }

    // cached_preset_metadata's own tests moved to convertbar_core::handbrake alongside the
    // function itself (Task 5); this file now only wraps it behind a Ctx-fetching command.

    #[test]
    fn resolve_suffix_template_command_matches_the_backend_resolver() {
        // The frontend preview delegates to this command instead of a JS copy that
        // diverged (JS produced "..h265" for an empty resolution; the resolver gives ".h265").
        let m = metadata("h265");
        let resolved = resolve_suffix_template(".{resolution}.{codec}".to_string(), m);
        assert_eq!(resolved, ".1080p.h265");

        let mut empty_res = metadata("h265");
        empty_res.resolution = String::new();
        assert_eq!(
            resolve_suffix_template(".{resolution}.{codec}".to_string(), empty_res),
            ".h265"
        );
    }
}
