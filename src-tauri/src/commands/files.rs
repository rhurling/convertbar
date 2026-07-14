use crate::types::PathsExist;

/// One stat per path, called when the history context menu opens so it can
/// disable Open/Reveal for a file that has since been trashed or moved.
#[tauri::command]
pub fn check_paths_exist(source_path: String, output_path: String) -> PathsExist {
    PathsExist {
        source_exists: std::path::Path::new(&source_path).exists(),
        output_exists: std::path::Path::new(&output_path).exists(),
    }
}

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reveal_in_dir(path: String) -> Result<(), String> {
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_paths_exist_stats_each_path_independently() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("clip.mp4");
        std::fs::write(&existing, b"x").unwrap();
        let missing = dir.path().join("gone.mp4");

        let result = check_paths_exist(
            existing.to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
        );
        assert!(result.source_exists);
        assert!(!result.output_exists);

        let both = check_paths_exist(
            existing.to_string_lossy().into_owned(),
            existing.to_string_lossy().into_owned(),
        );
        assert!(both.source_exists && both.output_exists);
    }
}
