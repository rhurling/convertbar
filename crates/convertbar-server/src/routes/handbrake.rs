//! Handbrake probe/scan routes: `/api/handbrake/*`, preset-suffix generation, and suffix-template
//! resolution. `detect_handbrake`/`list_handbrake_presets`/`validate_handbrake`/
//! `generate_preset_suffix` all shell out (to HandBrakeCLI or `which`/`where`) and so go through
//! `blocking_json`, mirroring the desktop's async commands
//! (`src-tauri/src/commands/handbrake.rs`). `resolve_suffix_template` is a pure string transform
//! and runs inline.

use axum::extract::{Path, State};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;

use convertbar_core::handbrake as hb;
use convertbar_core::handbrake::PresetMetadata;
use convertbar_core::types::HandbrakeStatus;

use super::{blocking_json, ServerState};

pub async fn detect_handbrake(State(s): State<ServerState>) -> Response {
    let ctx = s.ctx.clone();
    blocking_json(move || hb::resolve_handbrake_path(&ctx)).await
}

pub async fn list_handbrake_presets(State(s): State<ServerState>) -> Response {
    let ctx = s.ctx.clone();
    blocking_json(move || hb::list_available_presets(&ctx)).await
}

pub async fn validate_handbrake(State(s): State<ServerState>) -> Response {
    let ctx = s.ctx.clone();
    blocking_json(move || match hb::resolve_handbrake_path(&ctx)? {
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
}

pub async fn generate_preset_suffix(
    State(s): State<ServerState>,
    Path(preset): Path<String>,
) -> Response {
    let ctx = s.ctx.clone();
    blocking_json(move || {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use serde_json::json;

    use crate::routes::api_router;
    use crate::routes::tests::{request_json, test_state_with_locator};

    /// A stub HandBrakeCLI whose `--preset-list` prints two built-ins to stderr in the real
    /// CLI's layout, and which records whether it was ever handed the import flag.
    #[cfg(unix)]
    fn preset_list_stub(dir: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("hb-list.sh");
        let flagged = dir.join("saw-import-flag");
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\ncase \" $* \" in *\" --preset-import-file \"*) touch '{}' ;; esac\ncat >&2 <<'EOF2'\nGeneral/\n    Fast 1080p30\n        H.264 video.\n    H.265 MKV 1080p30\n        H.265 video.\nEOF2\n",
                flagged.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().to_string()
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn the_preset_list_puts_the_files_presets_first_then_the_built_ins() {
        // Names come from the file itself, not from `--preset-list` with the flag: the CLI prints
        // a `Type 0` entry at column 0 with its description indented like a preset name.
        let dir = tempfile::tempdir().unwrap();
        let stub = preset_list_stub(dir.path());
        let (state, _shutdown) =
            test_state_with_locator(Arc::new(convertbar_core::handbrake::StubLocator(stub)));
        let file = dir.path().join("presets.json");
        let preset_file = |extra: &str| {
            std::fs::write(
                &file,
                json!({"PresetList": [
                    {"PresetName": "Data x265", "Type": 1, "Folder": false},
                    {"PresetName": "Exported", "Type": 0, "Folder": false},
                    {"PresetName": "Shows", "Type": 1, "Folder": true, "ChildrenArray": [
                        {"PresetName": "Episode x265", "Type": 1, "Folder": false}
                    ]},
                    {"PresetName": extra, "Type": 1, "Folder": false}
                ]})
                .to_string(),
            )
            .unwrap();
        };
        preset_file("Extra");
        state
            .ctx
            .preset_import
            .set(convertbar_core::handbrake::PresetImport {
                path: file.clone(),
                builtins: ["Fast 1080p30", "H.265 MKV 1080p30"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            })
            .unwrap();

        let (code, body) = request_json(
            api_router(state.clone()),
            "GET",
            "/api/handbrake/presets",
            None,
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!([
                "Data x265",
                "Exported",
                "Episode x265",
                "Extra",
                "Fast 1080p30",
                "H.265 MKV 1080p30"
            ])
        );
        assert!(
            !dir.path().join("saw-import-flag").exists(),
            "the built-ins are listed without the import flag"
        );

        // The file changed after startup and now shadows a built-in: say so, don't list it.
        preset_file("Fast 1080p30");
        let (code, body) =
            request_json(api_router(state), "GET", "/api/handbrake/presets", None).await;
        assert_ne!(code, StatusCode::OK);
        assert!(body.to_string().contains("Fast 1080p30"), "{body}");
    }
}
