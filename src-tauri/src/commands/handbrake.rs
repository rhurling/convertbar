use rusqlite::params;
use tauri::State;

use crate::handbrake as hb;
use crate::handbrake::PresetMetadata;
use crate::types::HandbrakeStatus;
use crate::AppState;

#[tauri::command]
pub fn detect_handbrake(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;

    // Check user-configured path first
    let configured: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            params![],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Ok(Some(path.clone()));
        }
    }

    // Auto-detect
    Ok(hb::detect_handbrake_path())
}

#[tauri::command]
pub fn list_handbrake_presets(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let path = detect_handbrake(state)?;
    match path {
        Some(p) => hb::list_presets(&p),
        None => Err("HandBrakeCLI not found".to_string()),
    }
}

#[tauri::command]
pub fn generate_preset_suffix(state: State<'_, AppState>, preset: String) -> Result<PresetMetadata, String> {
    // Check cache first
    {
        let cache = state.preset_cache.lock().map_err(|e| e.to_string())?;
        if let Some(metadata) = cache.get(&preset) {
            return Ok(metadata.clone());
        }
    }

    // Get HandBrakeCLI path
    let handbrake_path = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let configured: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'handbrake_path'",
                params![],
                |row| row.get(0),
            )
            .ok();

        if let Some(ref path) = configured {
            if !path.is_empty() && std::path::Path::new(path).exists() {
                path.clone()
            } else {
                hb::detect_handbrake_path().ok_or("HandBrakeCLI not found")?
            }
        } else {
            hb::detect_handbrake_path().ok_or("HandBrakeCLI not found")?
        }
    };

    let metadata = hb::get_preset_metadata(&handbrake_path, &preset)?;

    // Cache the result
    {
        let mut cache = state.preset_cache.lock().map_err(|e| e.to_string())?;
        cache.insert(preset, metadata.clone());
    }

    Ok(metadata)
}

#[tauri::command]
pub fn validate_handbrake(state: State<'_, AppState>) -> Result<HandbrakeStatus, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;

    let configured: Option<String> = db.query_row(
        "SELECT value FROM settings WHERE key = 'handbrake_path'",
        params![],
        |row| row.get(0),
    ).ok();

    let path = if let Some(ref p) = configured {
        if !p.is_empty() && std::path::Path::new(p).exists() {
            Some(p.clone())
        } else {
            hb::detect_handbrake_path()
        }
    } else {
        hb::detect_handbrake_path()
    };

    match path {
        Some(p) => {
            let version = std::process::Command::new(&p)
                .arg("--version")
                .output()
                .ok()
                .and_then(|o| {
                    let out = String::from_utf8_lossy(&o.stderr);
                    out.lines().find(|l| l.contains("HandBrake")).map(|l| {
                        l.split_whitespace().nth(1).unwrap_or("unknown").to_string()
                    })
                })
                .unwrap_or_default();
            Ok(HandbrakeStatus { found: true, path: p, version })
        }
        None => Ok(HandbrakeStatus { found: false, path: String::new(), version: String::new() })
    }
}
