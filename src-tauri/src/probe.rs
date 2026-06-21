//! Source-file introspection via `HandBrakeCLI --scan --json`: produces a
//! `media_skip::SourceMedia` (normalized codec slug + height) for the skip-by-source-media
//! policy. Kept separate from `handbrake.rs` (preset handling) and the pure `media_skip.rs`.

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
}
