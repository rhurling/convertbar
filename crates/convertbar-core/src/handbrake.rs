use crate::ctx::Ctx;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::Duration;

const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

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

/// How the engine discovers HandBrakeCLI when no usable path is configured.
///
/// Injected on [`crate::ctx::Ctx`] rather than called directly so that tests can state which
/// world they are in. `detect_handbrake_path` shells out to `which`/`where`, i.e. it reads the
/// machine the test happens to run on — which let five tests pass locally and fail in CI.
pub trait HandbrakeLocator: Send + Sync {
    fn locate(&self) -> Option<String>;
}

/// Production: PATH detection via `which` / `where`.
pub struct PathLocator;

impl HandbrakeLocator for PathLocator {
    fn locate(&self) -> Option<String> {
        detect_handbrake_path()
    }
}

/// The test-harness default. Fails loud when a test reaches HandBrake resolution without
/// saying whether HandBrake is installed — the same tactic as `LockProbeSink` in `control.rs`.
/// Reaching this means the test's outcome would otherwise depend on the host.
pub struct PanickingLocator;

impl HandbrakeLocator for PanickingLocator {
    fn locate(&self) -> Option<String> {
        panic!(
            "HandBrake resolution was reached with the default test locator, so this test's \
             result would depend on whether HandBrakeCLI is installed on this machine. \
             Declare the world explicitly: `Arc::new(AbsentLocator)` for the CI world (no \
             HandBrake), or `Arc::new(StubLocator(path))` for the installed world."
        );
    }
}

/// The CI world: HandBrakeCLI is not installed.
pub struct AbsentLocator;

impl HandbrakeLocator for AbsentLocator {
    fn locate(&self) -> Option<String> {
        None
    }
}

/// The installed world, without requiring a real binary on the host.
pub struct StubLocator(pub String);

impl HandbrakeLocator for StubLocator {
    fn locate(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

/// The configured path if it points at an existing file, otherwise the locator's answer.
///
/// Filesystem work only — no DB access — so callers can hold or release the db guard as their
/// own call site requires.
pub fn resolve_with_locator(
    configured: Option<&str>,
    locator: &dyn HandbrakeLocator,
) -> Option<String> {
    if let Some(path) = configured {
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }
    locator.locate()
}

/// `HandBrakeCLI --version` with a hard deadline — a binary on a hung network mount
/// must not stall the validation thread indefinitely. `--version` output is tiny, so
/// reading stderr after exit cannot hit the pipe-buffer limit.
pub fn handbrake_version(hb_path: &str) -> Option<String> {
    use std::io::Read;

    let mut child = std::process::Command::new(hb_path)
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

pub fn list_presets(handbrake_path: &str) -> Result<Vec<String>, String> {
    let output = Command::new(handbrake_path)
        .arg("--preset-list")
        .output()
        .map_err(|e| format!("Failed to run HandBrakeCLI: {}", e))?;

    // HandBrakeCLI outputs the preset list to stderr.
    interpret_preset_list(
        output.status.success(),
        &String::from_utf8_lossy(&output.stderr),
    )
}

/// Interpret a `--preset-list` run's outcome. HandBrakeCLI prints presets to stderr and
/// normally exits 0; a non-zero exit with *nothing parseable* means the CLI itself failed
/// (missing shared lib, wrong binary), so surface that as an error instead of an empty
/// `Ok(vec![])` — which the UI can't distinguish from "this build has no presets" and would
/// render as a silently empty preset dropdown. If presets did parse, a non-zero exit is
/// ignored (better to show them than hide a working list). Split out so it can be unit-tested
/// without invoking HandBrakeCLI.
fn interpret_preset_list(success: bool, stderr: &str) -> Result<Vec<String>, String> {
    let presets = parse_preset_list(stderr);
    if presets.is_empty() && !success {
        return Err(format!(
            "HandBrakeCLI --preset-list failed: {}",
            truncate_str(stderr.trim(), 200)
        ));
    }
    Ok(presets)
}

/// Truncate `s` to at most `max_bytes`, backing up to the previous char boundary so slicing a
/// multibyte codepoint can't panic. Used only to bound diagnostic strings for error messages.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Extract preset names from `HandBrakeCLI --preset-list` output. Presets are the lines indented
/// exactly four spaces; category headers sit at the left margin and preset *property* lines are
/// indented eight, so both are excluded by the indentation test. A trailing `/` (a category that
/// happens to be indented) and blank lines are skipped. Split out of `list_presets` so the
/// indentation rules can be table-tested without invoking HandBrakeCLI.
fn parse_preset_list(stderr: &str) -> Vec<String> {
    let mut presets = Vec::new();
    for line in stderr.lines() {
        if line.starts_with("    ") && !line.starts_with("        ") {
            let name = line.trim().to_string();
            if !name.is_empty() && !name.ends_with('/') {
                presets.push(name);
            }
        }
    }
    presets
}

pub(crate) fn get_preset_metadata(
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

    interpret_preset_export(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        preset_name,
    )
}

/// Interpret a `--preset-export` run's outcome: a non-zero exit is surfaced as an error with
/// HandBrake's own stderr diagnostic (instead of a misleading "failed to parse JSON" on empty
/// stdout), and the diagnostic slice of stdout is truncated on a char boundary so a multibyte
/// codepoint straddling byte 200 can't panic the error path. Split out so it can be unit-tested
/// without invoking HandBrakeCLI.
fn interpret_preset_export(
    success: bool,
    stdout: &str,
    stderr: &str,
    preset_name: &str,
) -> Result<PresetMetadata, String> {
    if !success {
        return Err(format!(
            "HandBrakeCLI --preset-export failed: {}",
            truncate_str(stderr.trim(), 200)
        ));
    }

    let json: serde_json::Value = serde_json::from_str(stdout).map_err(|e| {
        format!(
            "Failed to parse preset JSON: {}. Output: {}",
            e,
            truncate_str(stdout, 200)
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

/// The user-configured path if it points at an existing file, otherwise PATH
/// detection. The DB lock is released before `which`/`where` shells out.
pub fn resolve_handbrake_path(ctx: &Ctx) -> Result<Option<String>, String> {
    let configured: Option<String> = {
        let conn = ctx.db.lock().map_err(|e| e.to_string())?;
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

    Ok(detect_handbrake_path())
}

/// Preset metadata via the shared cache. The cache mutex is deliberately NOT held
/// across the HandBrake shell-out: any command contending this lock would otherwise
/// block for the whole subprocess run (lock convoy). Concurrent misses may fetch the
/// same metadata twice; the duplicate insert is harmless.
pub fn cached_preset_metadata(
    ctx: &Ctx,
    hb_path: &str,
    preset: &str,
) -> Result<PresetMetadata, String> {
    {
        let cache = ctx.preset_cache.lock().map_err(|e| e.to_string())?;
        if let Some(m) = cache.get(preset) {
            return Ok(m.clone());
        }
    }

    let metadata = get_preset_metadata(hb_path, preset)?;

    ctx.preset_cache
        .lock()
        .map_err(|e| e.to_string())?
        .insert(preset.to_string(), metadata.clone());
    Ok(metadata)
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

    fn test_ctx() -> std::sync::Arc<Ctx> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        Ctx::new(
            conn,
            std::sync::Arc::new(crate::events::TestSink::default()),
            std::sync::Arc::new(crate::dispose::DeleteDisposer),
            std::sync::Arc::new(crate::handbrake::PanickingLocator),
        )
    }

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
    fn parse_preset_list_keeps_only_four_space_indented_preset_names() {
        // A realistic slice of `HandBrakeCLI --preset-list`: unindented category headers,
        // four-space preset names, an eight-space property line, a four-space category with a
        // trailing slash, and a blank line — only the true preset names must come through.
        let output = "\
General/
    Very Fast 1080p30
    Fast 1080p30
        VideoEncoder: x264
Matroska/
    H.265 MKV 1080p30
    Nested Category/

    Production Standard
";
        assert_eq!(
            parse_preset_list(output),
            vec![
                "Very Fast 1080p30",
                "Fast 1080p30",
                "H.265 MKV 1080p30",
                "Production Standard",
            ],
            "categories (col 0 / trailing slash), property lines (8-space), and blanks are dropped"
        );
    }

    #[test]
    fn parse_preset_list_empty_output_yields_no_presets() {
        assert!(parse_preset_list("").is_empty());
    }

    #[test]
    fn interpret_preset_list_errors_when_cli_fails_with_no_presets() {
        // A non-zero exit with no parseable presets is the CLI itself failing (missing lib,
        // bad binary). Returning Ok(vec![]) here makes the UI show an empty dropdown instead
        // of "couldn't load presets"; the stderr diagnostic must be surfaced.
        let err = interpret_preset_list(false, "error while loading shared libraries: libx.so")
            .unwrap_err();
        assert!(
            err.contains("error while loading shared libraries"),
            "the CLI diagnostic must be surfaced, got: {err}"
        );
    }

    #[test]
    fn interpret_preset_list_returns_presets_even_on_nonzero_exit() {
        // If HandBrake still printed a usable list, a non-zero exit must not hide it.
        let stderr = "General/\n    Fast 1080p30\n";
        assert_eq!(
            interpret_preset_list(false, stderr).unwrap(),
            vec!["Fast 1080p30"]
        );
    }

    #[test]
    fn interpret_preset_list_ok_empty_on_clean_exit() {
        assert_eq!(
            interpret_preset_list(true, "").unwrap(),
            Vec::<String>::new(),
            "a clean exit with no presets is a legitimate empty list, not an error"
        );
    }

    #[test]
    fn truncate_str_never_splits_a_multibyte_codepoint() {
        // 'é' is two bytes; place one so byte 200 lands mid-codepoint. `&s[..200]` would panic.
        let s = "a".repeat(199) + "é" + "trailing";
        assert!(
            !s.is_char_boundary(200),
            "test premise: byte 200 splits a codepoint"
        );
        let t = truncate_str(&s, 200);
        assert!(t.len() <= 200);
        assert_eq!(t, "a".repeat(199), "backs up to the char boundary at 199");
    }

    #[test]
    fn interpret_preset_export_errors_on_nonzero_exit_with_stderr() {
        let err = interpret_preset_export(false, "", "No such preset: Bogus", "Bogus").unwrap_err();
        assert!(
            err.contains("No such preset"),
            "the CLI stderr diagnostic must be surfaced, got: {err}"
        );
    }

    #[test]
    fn interpret_preset_export_does_not_panic_on_multibyte_at_the_slice_boundary() {
        // Invalid JSON whose byte 200 splits a codepoint: the old `&stdout[..200]` panicked
        // building the parse-error message. It must now degrade to a bounded, valid message.
        let stdout = "x".repeat(199) + &"é".repeat(50); // 299 bytes, not JSON
        let err = interpret_preset_export(true, &stdout, "", "P").unwrap_err();
        assert!(err.contains("Failed to parse preset JSON"));
    }

    #[test]
    fn interpret_preset_export_classifies_valid_json() {
        let stdout = r#"{"PresetList":[{"VideoEncoder":"x265","PictureHeight":1080}]}"#;
        let m = interpret_preset_export(true, stdout, "", "HQ 1080p30").unwrap();
        assert_eq!(m.codec, "h265");
        assert_eq!(m.resolution, "1080p");
        assert_eq!(m.quality, "hq");
    }

    #[test]
    fn cached_preset_metadata_serves_hits_without_shelling_out() {
        let ctx = test_ctx();
        ctx.preset_cache
            .lock()
            .unwrap()
            .insert("My Preset".to_string(), meta("h265", "1080p", ""));

        // The bogus binary path proves a cache hit never reaches the subprocess:
        // if it did, this would error instead of returning the cached value.
        let m = cached_preset_metadata(&ctx, "/nonexistent/HandBrakeCLI", "My Preset").unwrap();
        assert_eq!(m.codec, "h265");
    }

    #[test]
    fn cached_preset_metadata_miss_reaches_the_fetch_and_propagates_errors() {
        let ctx = test_ctx();

        let result = cached_preset_metadata(&ctx, "/nonexistent/HandBrakeCLI", "My Preset");
        assert!(result.is_err(), "a cache miss must attempt the real fetch");

        // A failed fetch must not poison the cache mutex or insert junk.
        assert!(ctx.preset_cache.lock().unwrap().is_empty());
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

    #[test]
    fn resolve_with_locator_prefers_an_existing_configured_path() {
        // A configured path that exists must win outright — the locator is the *fallback*, and
        // consulting it anyway would spawn a subprocess the user already told us to skip.
        let f = tempfile::NamedTempFile::new().unwrap();
        let configured = f.path().to_str().unwrap();
        let got = resolve_with_locator(Some(configured), &StubLocator("/never/used".into()));
        assert_eq!(got.as_deref(), Some(configured));
    }

    #[test]
    fn resolve_with_locator_falls_back_when_the_configured_path_is_gone() {
        // A stale setting (HandBrake uninstalled/moved since it was saved) must not be trusted.
        let got = resolve_with_locator(
            Some("/nonexistent/HandBrakeCLI"),
            &StubLocator("/found/here".into()),
        );
        assert_eq!(got.as_deref(), Some("/found/here"));
    }

    #[test]
    fn resolve_with_locator_falls_back_when_configured_is_empty() {
        // "" is the shipped DB default, so this is the unconfigured-user path, not an edge case.
        let got = resolve_with_locator(Some(""), &StubLocator("/found/here".into()));
        assert_eq!(got.as_deref(), Some("/found/here"));
    }

    #[test]
    fn absent_locator_reports_handbrake_missing() {
        // The CI world, expressible for the first time.
        assert_eq!(resolve_with_locator(Some(""), &AbsentLocator), None);
    }

    #[test]
    #[should_panic(expected = "Declare the world explicitly")]
    fn panicking_locator_fails_loud_rather_than_reading_the_machine() {
        // The guard itself must be able to fail; a fixture default that silently succeeded
        // would reintroduce exactly the machine-coupling this seam removes.
        let _ = resolve_with_locator(Some(""), &PanickingLocator);
    }
}
