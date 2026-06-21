//! Source-file introspection via `HandBrakeCLI --scan --json`: produces a
//! `media_skip::SourceMedia` (normalized codec slug + height) for the skip-by-source-media
//! policy. Kept separate from `handbrake.rs` (preset handling) and the pure `media_skip.rs`.

use crate::media_skip::SourceMedia;
use std::process::Command;

/// Map a HandBrake `--scan` `VideoCodec` string (often the libav *decoder* name) to the same
/// codec-slug vocabulary `classify_preset` emits. Substring-based because HandBrake reports
/// e.g. "libdav1d" for AV1 and "hevc" for h265. Unrecognized -> "unknown" (never skipped).
pub fn normalize_source_codec(handbrake_codec: &str) -> String {
    let c = handbrake_codec.to_lowercase();
    if c.contains("hevc") || c.contains("h265") || c.contains("x265") {
        "h265"
    } else if c.contains("av1") || c.contains("dav1d") || c.contains("aom") {
        "av1"
    } else if c.contains("h264") || c.contains("avc") || c.contains("x264") {
        "h264"
    } else if c.contains("vp9") {
        "vp9"
    } else if c.contains("prores") {
        "prores"
    } else if c.contains("dnxh") {
        "dnxhr"
    } else if c.contains("ffv1") {
        "ffv1"
    } else if c.contains("mpeg2") || c.contains("mpeg-2") {
        "mpeg2"
    } else if c.contains("mpeg4")
        || c.contains("mpeg-4")
        || c.contains("divx")
        || c.contains("xvid")
    {
        "mpeg4"
    } else if c.contains("vc1") || c.contains("vc-1") || c.contains("wmv") {
        "vc1"
    } else {
        "unknown"
    }
    .to_string()
}

/// Parse HandBrakeCLI `--scan --json` stdout into the source's normalized codec + height.
/// Returns `None` when the title set is missing/empty or unparseable; the caller treats
/// `None` as uncertainty and does not skip the file.
pub fn parse_scan_media(stdout: &str) -> Option<SourceMedia> {
    const MARKER: &str = "JSON Title Set: ";
    let start = stdout.find(MARKER)? + MARKER.len();
    let json: serde_json::Value = serde_json::from_str(stdout[start..].trim()).ok()?;
    let title = json["TitleList"].get(0)?;
    let height = title["Geometry"]["Height"].as_i64().unwrap_or(0);
    let raw_codec = title["VideoCodec"].as_str().unwrap_or("");
    Some(SourceMedia {
        codec: normalize_source_codec(raw_codec),
        height,
    })
}

/// Run `HandBrakeCLI --scan --json` on a file and return its normalized codec + height.
/// Returns `None` if the process fails to launch or no parseable title is found — the caller
/// treats `None` as uncertainty and queues the file rather than skipping it.
pub fn probe_source(handbrake_path: &str, file: &str) -> Option<SourceMedia> {
    let output = Command::new(handbrake_path)
        .args(["--scan", "--json", "-i", file])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_scan_media(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_source_codec_maps_handbrake_decoder_names() {
        // Real VideoCodec strings observed from HandBrakeCLI --scan --json.
        let cases = [
            ("h264", "h264"),
            ("hevc", "h265"),    // HandBrake reports HEVC as "hevc"
            ("libdav1d", "av1"), // AV1 reports the dav1d DECODER name, not "av1"
            ("av1", "av1"),
            ("vp9", "vp9"),
            ("prores", "prores"),
            ("dnxhd", "dnxhr"),
            ("ffv1", "ffv1"),
            ("mpeg2video", "mpeg2"),
            ("mpeg4", "mpeg4"),
            ("vc1", "vc1"),
            ("", "unknown"),
            ("some-future-codec", "unknown"),
        ];
        for (input, want) in cases {
            assert_eq!(normalize_source_codec(input), want, "input {input}");
        }
    }

    const SCAN_FIXTURE_HEVC: &str = r#"Version: {
    "Name": "HandBrake",
    "VersionString": "1.11.2"
}
JSON Title Set: {
    "MainFeature": 0,
    "TitleList": [
        {
            "Geometry": { "Height": 720, "PAR": { "Den": 1, "Num": 1 }, "Width": 1280 },
            "VideoCodec": "hevc"
        }
    ]
}
"#;

    const SCAN_FIXTURE_EMPTY: &str = r#"Version: { "Name": "HandBrake" }
JSON Title Set: {
    "MainFeature": 0,
    "TitleList": []
}
"#;

    #[test]
    fn parse_scan_media_reads_height_and_normalized_codec() {
        let media = parse_scan_media(SCAN_FIXTURE_HEVC).expect("title set present");
        assert_eq!(media.codec, "h265", "hevc normalizes to h265");
        assert_eq!(media.height, 720);
    }

    #[test]
    fn parse_scan_media_returns_none_when_no_title() {
        // Empty TitleList (e.g. a non-video / unreadable file) -> None -> caller never skips.
        assert!(parse_scan_media(SCAN_FIXTURE_EMPTY).is_none());
        // No marker at all -> None.
        assert!(parse_scan_media("garbage output, no json").is_none());
    }

    // Local-only: needs ffmpeg to synthesize a clip and HandBrakeCLI to scan it.
    // Run with: cargo test -- --ignored probe_source_reads_real_clip
    #[test]
    #[ignore]
    fn probe_source_reads_real_clip() {
        let hb = crate::handbrake::detect_handbrake_path().expect("HandBrakeCLI on PATH");
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("probe.mp4");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=0.3:size=320x240:rate=12",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
            ])
            .arg(&clip)
            .status()
            .expect("run ffmpeg");
        assert!(status.success());
        let media = probe_source(&hb, clip.to_str().unwrap()).expect("probe returns media");
        assert_eq!(media.codec, "h264");
        assert_eq!(media.height, 240);
    }
}
