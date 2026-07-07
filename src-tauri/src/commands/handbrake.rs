use rusqlite::params;
use std::time::Duration;
use tauri::{AppHandle, Manager};

use crate::handbrake as hb;
use crate::handbrake::PresetMetadata;
use crate::types::HandbrakeStatus;
use crate::AppState;

const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The user-configured path if it points at an existing file, otherwise PATH
/// detection. The DB lock is released before `which`/`where` shells out.
fn resolve_handbrake_path(state: &AppState) -> Result<Option<String>, String> {
    let configured: Option<String> = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'handbrake_path'",
            params![],
            |row| row.get(0),
        )
        .ok()
    };

    if let Some(ref path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Ok(Some(path.clone()));
        }
    }

    Ok(hb::detect_handbrake_path())
}

/// Preset metadata via the shared cache. The cache mutex is deliberately NOT held
/// across the HandBrake shell-out: any command contending this lock would otherwise
/// block for the whole subprocess run (lock convoy). Concurrent misses may fetch the
/// same metadata twice; the duplicate insert is harmless.
pub(crate) fn cached_preset_metadata(
    state: &AppState,
    hb_path: &str,
    preset: &str,
) -> Result<PresetMetadata, String> {
    {
        let cache = state.preset_cache.lock().map_err(|e| e.to_string())?;
        if let Some(m) = cache.get(preset) {
            return Ok(m.clone());
        }
    }

    let metadata = hb::get_preset_metadata(hb_path, preset)?;

    state
        .preset_cache
        .lock()
        .map_err(|e| e.to_string())?
        .insert(preset.to_string(), metadata.clone());
    Ok(metadata)
}

// All four commands below reach a subprocess (HandBrakeCLI or `which`/`where`); as
// sync commands they ran on the main thread and stalled the UI for the subprocess
// duration. async + spawn_blocking moves them off it, mirroring add_files.

#[tauri::command]
pub async fn detect_handbrake(app: AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        resolve_handbrake_path(&state)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_handbrake_presets(app: AppHandle) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        match resolve_handbrake_path(&state)? {
            Some(p) => hb::list_presets(&p),
            None => Err("HandBrakeCLI not found".to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn generate_preset_suffix(
    app: AppHandle,
    preset: String,
) -> Result<PresetMetadata, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();

        // Cache hit skips path resolution entirely (no DB lock, no `which`).
        {
            let cache = state.preset_cache.lock().map_err(|e| e.to_string())?;
            if let Some(metadata) = cache.get(&preset) {
                return Ok(metadata.clone());
            }
        }

        let handbrake_path = resolve_handbrake_path(&state)?.ok_or("HandBrakeCLI not found")?;
        cached_preset_metadata(&state, &handbrake_path, &preset)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn validate_handbrake(app: AppHandle) -> Result<HandbrakeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        match resolve_handbrake_path(&state)? {
            Some(p) => {
                let version = handbrake_version(&p).unwrap_or_default();
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
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `HandBrakeCLI --version` with a hard deadline — a binary on a hung network mount
/// must not stall the validation thread indefinitely. `--version` output is tiny, so
/// reading stderr after exit cannot hit the pipe-buffer limit.
fn handbrake_version(path: &str) -> Option<String> {
    use std::io::Read;

    let mut child = std::process::Command::new(path)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    crate::probe::wait_with_timeout(&mut child, VERSION_CHECK_TIMEOUT)?;

    let mut stderr = String::new();
    child.stderr.take()?.read_to_string(&mut stderr).ok()?;
    stderr
        .lines()
        .find(|l| l.contains("HandBrake"))
        .map(|l| l.split_whitespace().nth(1).unwrap_or("unknown").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn test_state() -> AppState {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        AppState {
            db: Arc::new(Mutex::new(conn)),
            preset_cache: Mutex::new(HashMap::new()),
        }
    }

    fn metadata(codec: &str) -> PresetMetadata {
        PresetMetadata {
            codec: codec.to_string(),
            resolution: "1080p".to_string(),
            quality: "q30".to_string(),
            preset: "test".to_string(),
            device: String::new(),
        }
    }

    #[test]
    fn cached_preset_metadata_serves_hits_without_shelling_out() {
        let state = test_state();
        state
            .preset_cache
            .lock()
            .unwrap()
            .insert("My Preset".to_string(), metadata("h265"));

        // The bogus binary path proves a cache hit never reaches the subprocess:
        // if it did, this would error instead of returning the cached value.
        let m = cached_preset_metadata(&state, "/nonexistent/HandBrakeCLI", "My Preset").unwrap();
        assert_eq!(m.codec, "h265");
    }

    #[test]
    fn cached_preset_metadata_miss_reaches_the_fetch_and_propagates_errors() {
        let state = test_state();

        let result = cached_preset_metadata(&state, "/nonexistent/HandBrakeCLI", "My Preset");
        assert!(result.is_err(), "a cache miss must attempt the real fetch");

        // A failed fetch must not poison the cache mutex or insert junk.
        assert!(state.preset_cache.lock().unwrap().is_empty());
    }
}
