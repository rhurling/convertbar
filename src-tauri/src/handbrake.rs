use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetMetadata {
    pub codec: String,
    pub resolution: String,
    pub quality: String,
    pub preset: String,
    pub device: String,
}

pub fn detect_handbrake_path() -> Option<String> {
    // Use `where` on Windows, `which` on Unix
    let cmd = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };

    if let Ok(output) = Command::new(cmd).arg("HandBrakeCLI").output() {
        if output.status.success() {
            // `where` on Windows may return multiple lines; take the first
            let path = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                return Some(path);
            }
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
        if line.starts_with("    ") && !line.starts_with("        ") {
            let name = line.trim().to_string();
            if !name.is_empty() && !name.ends_with('/') {
                presets.push(name);
            }
        }
    }

    Ok(presets)
}

pub fn get_preset_metadata(
    handbrake_path: &str,
    preset_name: &str,
) -> Result<PresetMetadata, String> {
    let output = Command::new(handbrake_path)
        .arg("--preset")
        .arg(preset_name)
        .arg("--preset-export")
        .arg("tmp")
        .output()
        .map_err(|e| format!("Failed to run HandBrakeCLI: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    let json: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
        format!(
            "Failed to parse preset JSON: {}. Output: {}",
            e,
            &stdout[..stdout.len().min(200)]
        )
    })?;

    let preset_obj = &json["PresetList"][0];

    let video_encoder = preset_obj["VideoEncoder"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let picture_height = preset_obj["PictureHeight"].as_i64().unwrap_or(0);

    let quality_slider = preset_obj["VideoQualitySlider"].as_f64().unwrap_or(0.0);

    // Codec
    let encoder_lower = video_encoder.to_lowercase();
    let codec = if encoder_lower.contains("h265")
        || encoder_lower.contains("hevc")
        || encoder_lower.contains("x265")
    {
        "h265"
    } else if encoder_lower.contains("h264") || encoder_lower.contains("x264") {
        "h264"
    } else if encoder_lower.contains("av1") {
        "av1"
    } else if encoder_lower.contains("vp9") {
        "vp9"
    } else if encoder_lower.contains("prores") {
        "prores"
    } else if encoder_lower.contains("dnxhr") {
        "dnxhr"
    } else if encoder_lower.contains("ffv1") {
        "ffv1"
    } else {
        "unknown"
    }
    .to_string();

    // Resolution
    let resolution = if picture_height == 0 {
        String::new()
    } else {
        format!("{}p", picture_height)
    };

    // Quality - parse from preset name first
    let quality = if preset_name.starts_with("Very Fast") {
        "vf".to_string()
    } else if preset_name.starts_with("Fast") {
        "f".to_string()
    } else if preset_name.starts_with("Super HQ") {
        "shq".to_string()
    } else if preset_name.starts_with("HQ") {
        "hq".to_string()
    } else if preset_name.starts_with("Creator") {
        "cr".to_string()
    } else if preset_name.starts_with("Production") {
        "prod".to_string()
    } else if preset_name.starts_with("Preservation") {
        "pres".to_string()
    } else {
        format!("q{}", quality_slider.round() as i64)
    };

    // Preset slug
    let preset_slug = slugify(preset_name);

    // Device
    let name_lower = preset_name.to_lowercase();
    let device = if name_lower.contains("apple videotoolbox") {
        "apple-videotoolbox"
    } else if name_lower.starts_with("apple") {
        "apple"
    } else if name_lower.starts_with("amazon fire") {
        "amazon-fire"
    } else if name_lower.starts_with("android") {
        "android"
    } else if name_lower.starts_with("chromecast") {
        "chromecast"
    } else if name_lower.starts_with("playstation") {
        "playstation"
    } else if name_lower.starts_with("roku") {
        "roku"
    } else if name_lower.starts_with("xbox") {
        "xbox"
    } else if name_lower.contains("nvenc") {
        "nvenc"
    } else if name_lower.contains("qsv") {
        "qsv"
    } else if name_lower.contains("vcn") {
        "vcn"
    } else if name_lower.contains(" mf ") {
        "mf"
    } else {
        ""
    }
    .to_string();

    Ok(PresetMetadata {
        codec,
        resolution,
        quality,
        preset: preset_slug,
        device,
    })
}

fn slugify(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut slug = String::with_capacity(lower.len());
    let mut last_was_sep = true; // treat start as separator to strip leading
    for c in lower.chars() {
        if c.is_alphanumeric() {
            slug.push(c);
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('-');
            last_was_sep = true;
        }
    }
    // strip trailing separator
    if slug.ends_with('-') {
        slug.pop();
    }
    slug
}

pub fn resolve_suffix_template(template: &str, metadata: &PresetMetadata) -> String {
    let vars: &[(&str, &str)] = &[
        ("{codec}", &metadata.codec),
        ("{resolution}", &metadata.resolution),
        ("{quality}", &metadata.quality),
        ("{preset}", &metadata.preset),
        ("{device}", &metadata.device),
    ];

    let mut result = template.to_string();
    for &(var, value) in vars {
        if value.is_empty() {
            // Remove variable and one adjacent separator (- _ .) but not leading dot
            // Try patterns: sep+var, var+sep
            let separators = ['-', '_', '.'];
            let mut replaced = false;
            for sep in &separators {
                // pattern: var followed by separator  e.g. "{device}-"
                let pattern = format!("{}{}", var, sep);
                if result.contains(&pattern) {
                    result = result.replacen(&pattern, "", 1);
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                for sep in &separators {
                    // pattern: separator followed by var  e.g. "-{device}"
                    let pattern = format!("{}{}", sep, var);
                    // Don't remove the leading dot of the template
                    if let Some(pos) = result.find(&pattern) {
                        // Only skip if this separator is the very first char AND it's a dot
                        if pos == 0 && *sep == '.' {
                            // Remove just the variable, not the leading dot
                            result = result.replacen(var, "", 1);
                        } else {
                            result = result.replacen(&pattern, "", 1);
                        }
                        replaced = true;
                        break;
                    }
                }
            }
            if !replaced {
                // Just remove the variable placeholder
                result = result.replacen(var, "", 1);
            }
        } else {
            result = result.replace(var, value);
        }
    }
    result
}
