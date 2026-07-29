use super::{blocking, CommandError};
use crate::types::PathsExist;

/// One stat per path. Split out so its behaviour is testable without a runtime — the command
/// below exists only to get it off the calling thread.
fn stat_paths(source_path: &str, output_path: &str) -> PathsExist {
    PathsExist {
        source_exists: std::path::Path::new(source_path).exists(),
        output_exists: std::path::Path::new(output_path).exists(),
    }
}

// Every command in this file touches the filesystem before it can answer, and `exists()`,
// `metadata()` and `canonicalize()` all block for as long as the mount takes to answer — on a
// disconnected network share, until it times out. As sync commands they ran on the main thread,
// so the history context menu could freeze the whole UI on a job whose source had gone away.
// That is the probe hazard again, at a fifth, sixth and seventh entry point.
//
// Off-thread is also how the opener plugin itself calls its side of this: its own
// `#[tauri::command]`s are `async`, so it never assumed the main thread either.

/// Called when the history context menu opens, so it can disable Open/Reveal for a file that has
/// since been trashed or moved.
#[tauri::command]
pub async fn check_paths_exist(
    source_path: String,
    output_path: String,
) -> Result<PathsExist, CommandError> {
    blocking(move || Ok(stat_paths(&source_path, &output_path))).await
}

#[tauri::command]
pub async fn open_path(path: String) -> Result<(), CommandError> {
    // Stats the path before handing off to the detached opener (plugin `open.rs`), so it blocks
    // on exactly the same dead mount as `check_paths_exist`.
    blocking(move || tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string()))
        .await
}

#[tauri::command]
pub async fn reveal_in_dir(path: String) -> Result<(), CommandError> {
    // `canonicalize` before revealing, same blocking round trip.
    blocking(move || tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_paths_stats_each_path_independently() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("clip.mp4");
        std::fs::write(&existing, b"x").unwrap();
        let missing = dir.path().join("gone.mp4");

        let result = stat_paths(&existing.to_string_lossy(), &missing.to_string_lossy());
        assert!(result.source_exists);
        assert!(!result.output_exists);

        let both = stat_paths(&existing.to_string_lossy(), &existing.to_string_lossy());
        assert!(both.source_exists && both.output_exists);
    }
}
