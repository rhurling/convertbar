use std::process::Command;

pub fn detect_handbrake_path() -> Option<String> {
    // Try `which HandBrakeCLI`
    if let Ok(output) = Command::new("which").arg("HandBrakeCLI").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    // Try known paths
    let known_paths = [
        "/usr/local/bin/HandBrakeCLI",
        "/opt/homebrew/bin/HandBrakeCLI",
    ];

    for path in &known_paths {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    None
}

pub fn list_presets(handbrake_path: &str) -> Result<Vec<String>, String> {
    let output = Command::new(handbrake_path)
        .arg("--preset-list")
        .output()
        .map_err(|e| format!("Failed to run HandBrakeCLI: {}", e))?;

    // HandBrakeCLI outputs preset list to stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut presets = Vec::new();

    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("+ ") {
            // Skip category headers (lines ending with "/")
            if !name.ends_with('/') {
                presets.push(name.to_string());
            }
        }
    }

    Ok(presets)
}
