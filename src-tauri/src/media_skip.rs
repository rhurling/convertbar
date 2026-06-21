//! Pure policy for skipping queued files whose source already meets/exceeds the target preset.
//! No I/O — every function here is table-testable. The HandBrake shell-out that produces a
//! `SourceMedia` lives in `handbrake.rs`.

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
}
