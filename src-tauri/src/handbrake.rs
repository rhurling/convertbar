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

    Ok(classify_preset(&json["PresetList"][0], preset_name))
}

/// Pure classification of a preset's exported JSON (the `PresetList[0]` object) plus its
/// name into the metadata used for output-filename suffixes. Split out of
/// `get_preset_metadata` so it can be table-tested without invoking HandBrakeCLI.
fn classify_preset(preset_obj: &serde_json::Value, preset_name: &str) -> PresetMetadata {
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

    PresetMetadata {
        codec,
        resolution,
        quality,
        preset: preset_slug,
        device,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(codec: &str, resolution: &str, device: &str) -> PresetMetadata {
        PresetMetadata {
            codec: codec.into(),
            resolution: resolution.into(),
            quality: "hq".into(),
            preset: "preset".into(),
            device: device.into(),
        }
    }

    #[test]
    fn slugify_collapses_separators_and_trims() {
        assert_eq!(
            slugify("H.265 Apple VideoToolbox 1080p"),
            "h-265-apple-videotoolbox-1080p"
        );
        assert_eq!(slugify("  Fast 1080p30  "), "fast-1080p30");
    }

    #[test]
    fn resolves_full_template() {
        let m = meta("h265", "1080p", "apple-videotoolbox");
        assert_eq!(
            resolve_suffix_template(".{resolution}-{codec}", &m),
            ".1080p-h265"
        );
    }

    #[test]
    fn drops_empty_var_and_its_trailing_separator_but_keeps_leading_dot() {
        let m = meta("h265", "", "apple-videotoolbox"); // empty resolution
        assert_eq!(
            resolve_suffix_template(".{resolution}-{codec}", &m),
            ".h265"
        );
    }

    #[test]
    fn drops_empty_var_and_its_leading_separator() {
        let m = meta("", "1080p", ""); // empty codec
        assert_eq!(
            resolve_suffix_template(".{resolution}-{codec}", &m),
            ".1080p"
        );
    }

    #[test]
    fn classify_preset_maps_encoder_to_codec() {
        // Realistic HandBrake VideoEncoder strings -> our codec slug.
        let cases = [
            ("x265", "h265"),
            ("x265_10bit", "h265"),
            ("vt_h265", "h265"),
            ("hevc", "h265"),
            ("x264", "h264"),
            ("nvenc_h264", "h264"),
            ("svt_av1", "av1"),
            ("vp9", "vp9"),
            ("prores_ks", "prores"),
            ("dnxhr", "dnxhr"),
            ("ffv1", "ffv1"),
            ("mpeg2", "unknown"),
        ];
        for (encoder, want) in cases {
            let obj = serde_json::json!({ "VideoEncoder": encoder });
            assert_eq!(
                classify_preset(&obj, "preset").codec,
                want,
                "encoder {encoder}"
            );
        }
    }

    #[test]
    fn classify_preset_maps_name_to_device() {
        let cases = [
            ("Apple VideoToolbox H.265 1080p", "apple-videotoolbox"),
            ("Apple 1080p30 Surround", "apple"),
            ("Amazon Fire 1080p30 Surround", "amazon-fire"),
            ("Android 1080p30 Surround", "android"),
            ("Chromecast 1080p30 Surround", "chromecast"),
            ("Playstation 1080p30 Surround", "playstation"),
            ("Roku 1080p30 Surround", "roku"),
            ("Xbox 1080p30 Surround", "xbox"),
            ("H.265 NVENC 1080p", "nvenc"),
            ("H.265 QSV 1080p", "qsv"),
            ("H.265 VCN 1080p", "vcn"),
            ("H.265 MF 1080p", "mf"),
            ("Production Standard", ""),
        ];
        for (name, want) in cases {
            let obj = serde_json::json!({});
            assert_eq!(classify_preset(&obj, name).device, want, "name {name}");
        }
    }

    #[test]
    fn classify_preset_maps_name_prefix_to_quality() {
        // "Super HQ" must win over "HQ" — it's checked first.
        let cases = [
            ("Very Fast 1080p30", "vf"),
            ("Fast 1080p30", "f"),
            ("Super HQ 1080p30 Surround", "shq"),
            ("HQ 1080p30 Surround", "hq"),
            ("Creator 2160p60 4K HEVC", "cr"),
            ("Production Standard", "prod"),
            ("Preservation 2160p60", "pres"),
        ];
        for (name, want) in cases {
            let obj = serde_json::json!({});
            assert_eq!(classify_preset(&obj, name).quality, want, "name {name}");
        }
    }

    #[test]
    fn classify_preset_quality_falls_back_to_rounded_slider() {
        // No recognized name prefix -> q{rounded VideoQualitySlider}.
        let obj = serde_json::json!({ "VideoQualitySlider": 22.4 });
        assert_eq!(
            classify_preset(&obj, "Apple 1080p30 Surround").quality,
            "q22"
        );
    }

    #[test]
    fn classify_preset_reads_resolution_from_height() {
        let zero = serde_json::json!({ "PictureHeight": 0 });
        assert_eq!(classify_preset(&zero, "preset").resolution, "");
        let hd = serde_json::json!({ "PictureHeight": 1080 });
        assert_eq!(classify_preset(&hd, "preset").resolution, "1080p");
    }
}
