//! Pure policy for skipping queued files whose source already meets/exceeds the target preset.
//! No I/O — every function here is table-testable. The HandBrake shell-out that produces a
//! `SourceMedia` lives in `handbrake.rs`.

use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMedia {
    /// Normalized codec slug (same vocabulary as `handbrake::classify_preset`'s codec).
    pub codec: String,
    pub height: i64,
}

/// Compression-efficiency rank of a codec slug. Higher = more efficient (smaller files at
/// equal quality). `None` means "unrecognized" — the caller treats that as uncertainty and
/// never skips. Intentionally coarse buckets: av1/h265/vp9 are treated as equally efficient.
pub fn efficiency_rank(codec: &str) -> Option<u8> {
    match codec {
        "av1" | "h265" | "vp9" => Some(3),
        "h264" => Some(2),
        "mpeg2" | "mpeg4" | "vc1" => Some(1),
        "prores" | "dnxhr" | "ffv1" => Some(0),
        _ => None,
    }
}

/// Parse a `classify_preset` resolution slug ("1080p", "" ...) into a numeric height.
/// "" or anything unparseable -> 0 (no downscale benefit possible).
pub fn target_height_from_resolution(resolution: &str) -> i64 {
    resolution
        .strip_suffix('p')
        .and_then(|n| n.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Decide whether a source already meets/exceeds the target so re-encoding would not help.
/// Skip only when NEITHER downscaling NOR a more-efficient codec would shrink the file.
/// Unknown codec on either side forces "could help" so the file is queued, never skipped.
pub fn should_skip_by_media(
    source_codec: &str,
    source_height: i64,
    target_codec: &str,
    target_height: i64,
) -> bool {
    let resolution_would_help = target_height > 0 && source_height > target_height;
    let codec_would_help = match (efficiency_rank(source_codec), efficiency_rank(target_codec)) {
        (Some(source_rank), Some(target_rank)) => target_rank > source_rank,
        _ => true, // unknown on either side -> assume a conversion could help
    };
    !resolution_would_help && !codec_would_help
}

/// From candidate `(path, probed media)` pairs, return the set of paths to skip because they
/// already meet the target. `None` media (probe failed / not a video) is never skipped.
pub fn select_media_skips(
    candidates: &[(String, Option<SourceMedia>)],
    target_codec: &str,
    target_height: i64,
) -> HashSet<String> {
    candidates
        .iter()
        .filter_map(|(path, media)| {
            let m = media.as_ref()?;
            if should_skip_by_media(&m.codec, m.height, target_codec, target_height) {
                Some(path.clone())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn efficiency_rank_buckets_codecs() {
        // Modern efficient codecs share the top bucket.
        for c in ["av1", "h265", "vp9"] {
            assert_eq!(efficiency_rank(c), Some(3), "codec {c}");
        }
        assert_eq!(efficiency_rank("h264"), Some(2));
        // Older lossy codecs rank below h264 so they always re-encode.
        for c in ["mpeg2", "mpeg4", "vc1"] {
            assert_eq!(efficiency_rank(c), Some(1), "codec {c}");
        }
        // Intermediate/lossless rank lowest — they must always re-encode to a delivery codec.
        for c in ["prores", "dnxhr", "ffv1"] {
            assert_eq!(efficiency_rank(c), Some(0), "codec {c}");
        }
        // Unknown is the uncertainty sentinel — None, never a number.
        assert_eq!(efficiency_rank("unknown"), None);
        assert_eq!(efficiency_rank("totally-made-up"), None);
    }

    #[test]
    fn parses_target_height_from_resolution_slug() {
        // classify_preset emits "{height}p" or "" (when the preset keeps source resolution).
        assert_eq!(target_height_from_resolution("1080p"), 1080);
        assert_eq!(target_height_from_resolution("2160p"), 2160);
        assert_eq!(target_height_from_resolution("720p"), 720);
        // No cap / unparseable -> 0, which the policy reads as "no resolution benefit possible".
        assert_eq!(target_height_from_resolution(""), 0);
        assert_eq!(target_height_from_resolution("p"), 0);
        assert_eq!(target_height_from_resolution("source"), 0);
    }

    #[test]
    fn skip_decision_only_when_neither_dimension_helps() {
        // (source_codec, source_h, target_codec, target_h, expect_skip, why)
        let cases = [
            // Neither helps -> skip. The waste case the feature exists to prevent.
            (
                "av1",
                1080,
                "h265",
                1080,
                true,
                "av1 1080p -> h265 1080p is pure waste",
            ),
            // Codec helps (h264 -> h265 at same res is a real size win) -> convert.
            (
                "h264",
                1080,
                "h265",
                1080,
                false,
                "h264 -> h265 saves space",
            ),
            // Intermediate source always re-encodes (ProRes ranks lowest).
            (
                "prores",
                1080,
                "h265",
                1080,
                false,
                "ProRes must re-encode to a delivery codec",
            ),
            // Resolution helps (downscale) -> convert even if codec matches.
            (
                "h265",
                2160,
                "h265",
                1080,
                false,
                "4K -> 1080p downscale shrinks the file",
            ),
            // No upscale benefit + same codec -> skip.
            (
                "h265",
                720,
                "h265",
                1080,
                true,
                "never upscale 720p to a 1080p target",
            ),
            // Downgrade saves nothing -> skip (compatibility users turn the toggle off).
            (
                "h265",
                1080,
                "h264",
                1080,
                true,
                "h265 -> h264 at same res saves nothing",
            ),
            // Uncertainty: unknown on EITHER side -> never skip.
            (
                "unknown",
                1080,
                "h265",
                1080,
                false,
                "unknown source codec is never skipped",
            ),
            (
                "h264",
                1080,
                "unknown",
                1080,
                false,
                "unknown target codec is never skipped",
            ),
            // Target with no resolution cap (height 0): resolution can't help; decide on codec.
            ("h265", 1080, "h265", 0, true, "no cap + same codec -> skip"),
            (
                "h264",
                1080,
                "h265",
                0,
                false,
                "no cap but codec upgrade -> convert",
            ),
        ];
        for (sc, sh, tc, th, expect, why) in cases {
            assert_eq!(should_skip_by_media(sc, sh, tc, th), expect, "{why}");
        }
    }

    #[test]
    fn selects_only_at_target_paths_with_known_media() {
        let candidates = vec![
            // Already at target -> selected for skip.
            (
                "/m/av1.mp4".to_string(),
                Some(SourceMedia {
                    codec: "av1".into(),
                    height: 1080,
                }),
            ),
            // Codec upgrade available -> not skipped.
            (
                "/m/h264.mp4".to_string(),
                Some(SourceMedia {
                    codec: "h264".into(),
                    height: 1080,
                }),
            ),
            // Probe failed / not introspectable -> never skipped (uncertainty).
            ("/m/unknown.mp4".to_string(), None),
        ];
        let skip = select_media_skips(&candidates, "h265", 1080);
        assert!(skip.contains("/m/av1.mp4"));
        assert!(!skip.contains("/m/h264.mp4"));
        assert!(
            !skip.contains("/m/unknown.mp4"),
            "None media must never be skipped"
        );
        assert_eq!(skip.len(), 1);
    }
}
