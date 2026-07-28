//! Pure policy for deciding WHY a conversion failed, so a genuinely bad source file can be
//! told apart from a broken environment. No I/O — every function here is table-testable,
//! mirroring `media_skip.rs`. The caller gathers the facts; this module only decides.
//!
//! The governing rule is the same as the skip policy's: uncertainty is never destructive.

/// Stored `jobs.failure_class` value for a source condemned by rule 4 (HandBrake could not
/// scan a file we ourselves could read).
pub const CLASS_BAD_SOURCE: &str = "bad_source";
/// Stored value for a source condemned by the Phase 2 decode-shortfall guard. Kept distinct
/// from [`CLASS_BAD_SOURCE`] because purge re-scans the latter and must NOT re-scan this one:
/// a truncated file passes a scan by construction, so re-scanning would clear the whole list.
pub const CLASS_BAD_SOURCE_TRUNCATED: &str = "bad_source_truncated";
/// Stored value after the user's bulk action destroyed the source. Keeps the history row
/// visible while dropping it out of the review list.
pub const CLASS_BAD_SOURCE_PURGED: &str = "bad_source_purged";
/// Stored value after a purge rescan finds the file fine after all (`PurgeOutcome::Recovered`).
/// Distinct from `CLASS_BAD_SOURCE_PURGED` (nothing was destroyed here) and from NULL (which
/// reads as "this row predates the failure_class feature") — a recovered row's history stays a
/// distinguishable, debuggable fact instead of being erased or conflated with pre-feature data.
/// Like `CLASS_BAD_SOURCE_PURGED`, this drops the row out of the review list's `IN (...)` filter
/// (see `get_bad_sources_inner`) without touching the row's terminal `status`.
pub const CLASS_BAD_SOURCE_RECOVERED: &str = "bad_source_recovered";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// The source file itself is the problem: HandBrake could not scan a file we could read.
    BadSource,
    /// The source decoded to far fewer frames than its container claimed. Distinct from
    /// [`FailureClass::BadSource`] because purge re-scans the latter and must NOT re-scan
    /// this one — a truncated file passes a scan by construction. Never returned by
    /// [`classify`]; only the Phase 2 guard constructs it.
    BadSourceTruncated,
    /// Config, disk, permissions, a missing binary — never the file's fault.
    Environment,
    /// Not enough evidence. Never destructive.
    Unknown,
}

impl FailureClass {
    /// The persisted string. Pinned by test — these values live in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            FailureClass::BadSource => CLASS_BAD_SOURCE,
            FailureClass::BadSourceTruncated => CLASS_BAD_SOURCE_TRUNCATED,
            FailureClass::Environment => "environment",
            FailureClass::Unknown => "unknown",
        }
    }
}

/// Everything the caller observed about a failure. `source_readable` is deliberately OUR
/// observation, not HandBrake's: HandBrake emits byte-identical output for a corrupt file
/// and for a healthy file it lacks permission to open.
pub struct FailureFacts<'a> {
    /// `None` when the child was killed or signalled rather than exiting.
    pub exit_code: Option<i32>,
    pub source_readable: bool,
    pub stderr_tail: &'a str,
}

/// Diagnostics that mean the environment failed, never the file. `invalid preset` is here
/// because a HandBrake upgrade that renames a preset exits 2 — the same code as bad input —
/// and would otherwise condemn every file in the queue.
const ENVIRONMENT_MARKERS: [&str; 6] = [
    "invalid preset",
    "no space left",
    "permission denied",
    "not permitted",
    "read-only",
    "cannot create",
];

/// Diagnostics that mean HandBrake could not make sense of the input.
const SOURCE_MARKERS: [&str; 3] = ["unrecognized file type", "no title found", "0 valid title"];

/// Decide why a job failed. First matching rule wins; see the design spec for the rationale
/// behind the ordering (rule 2 MUST precede rule 4).
pub fn classify(facts: &FailureFacts) -> FailureClass {
    // 1. We could not read it ourselves, so HandBrake's verdict proves nothing about the file.
    if !facts.source_readable {
        return FailureClass::Environment;
    }
    let lower = facts.stderr_tail.to_lowercase();
    // 2. An explicit environment diagnostic. Before rule 4 so `Invalid preset` (exit 2)
    //    can never reach the BadSource branch.
    if ENVIRONMENT_MARKERS.iter().any(|m| lower.contains(m)) {
        return FailureClass::Environment;
    }
    // 3. Exit 3 is HB_ERROR_INIT / a libhb work failure — measured for a missing and a
    //    read-only output directory with a VALID source. Bad input is signalled with 2.
    if facts.exit_code == Some(3) {
        return FailureClass::Environment;
    }
    // 4. Exit 2 AND HandBrake could not parse a file we just opened successfully.
    if facts.exit_code == Some(2) && SOURCE_MARKERS.iter().any(|m| lower.contains(m)) {
        return FailureClass::BadSource;
    }
    FailureClass::Unknown
}

/// A source counts as truncated only when it decoded to less than this fraction of the
/// frames HandBrake expected. Measured margin is wide — healthy encodes sit at exactly
/// 1.00, truncated ones at 0.21–0.32 — so 0.90 absorbs container accounting quirks
/// without approaching either cluster.
pub const MIN_DECODED_FRACTION: f64 = 0.90;

/// Parse one `sync: got N frames, M expected` line into `(got, expected)`.
fn parse_sync_line(line: &str) -> Option<(u64, u64)> {
    let rest = line.split_once("sync: got ")?.1;
    let (got, rest) = rest.split_once(" frames, ")?;
    let expected = rest.strip_suffix(" expected")?;
    Some((got.trim().parse().ok()?, expected.trim().parse().ok()?))
}

/// The frame shortfall HandBrake reported, as `(decoded, expected)`.
///
/// Takes the LAST such line in the tail. Returns `None` when the marker is absent or
/// unparseable, which the caller must treat as uncertainty — never as truncation.
pub fn decode_shortfall(stderr_tail: &str) -> Option<(u64, u64)> {
    stderr_tail.lines().filter_map(parse_sync_line).next_back()
}

/// Whether a decode shortfall is large enough to mean the source is truncated.
///
/// `expected == 0` is uncertainty (nothing to compare against) and is never truncated.
/// Expressed as a multiplication rather than a division so a zero denominator is impossible.
pub fn is_truncated(got: u64, expected: u64) -> bool {
    expected > 0 && (got as f64) < (expected as f64) * MIN_DECODED_FRACTION
}

/// Substrings that mark a line as the actual failure reason. HandBrake opens its
/// stderr with a build banner and host-info preamble (none of which match these), so
/// the first hit is the diagnostic rather than the noise above it.
const DIAGNOSTIC_MARKERS: [&str; 18] = [
    "error",
    "failed",
    "fatal",
    "aborted",
    "not found",
    "no such file",
    "no title",
    "unrecognized",
    "unsupported",
    "invalid",
    "corrupt",
    "no space",
    "read-only",
    "permission denied",
    "not permitted",
    "cannot",
    "could not",
    "unable",
];

/// The first line that reads like a failure reason, or None if nothing stands out.
pub fn diagnostic_headline<'a>(lines: &[&'a str]) -> Option<&'a str> {
    lines.iter().copied().find(|line| {
        let lower = line.to_lowercase();
        DIAGNOSTIC_MARKERS.iter().any(|m| lower.contains(m))
    })
}

/// The bare failure prefixes written before the diagnostic headline was promoted. A
/// stored message whose first line is exactly one of these predates the change and
/// still leads with HandBrake's banner. Kept in sync with the `message_with_tail`
/// callers below.
const LEGACY_ERROR_PREFIXES: [&str; 2] = [
    "Conversion failed:",
    "Conversion produced an empty output file:",
];

/// Rewrite a previously-stored error message so its first line is the failure reason
/// instead of HandBrake's build banner. Returns None when the message is already
/// headlined, isn't one of our messages, or has no recognizable diagnostic — which
/// makes the backfill that calls this idempotent (a rewritten first line no longer
/// matches a legacy prefix).
pub fn promote_stored_diagnostic(message: &str) -> Option<String> {
    let (first_line, body) = message.split_once('\n')?;
    if !LEGACY_ERROR_PREFIXES.contains(&first_line) {
        return None;
    }
    let headline = diagnostic_headline(&body.lines().collect::<Vec<_>>())?;
    Some(format!("{first_line} {headline}\n{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real HandBrakeCLI 1.11.2 stderr. This EXACT text is produced by all three of:
    // a zero-byte file, a directory, and a healthy file with mode 000. HandBrake cannot
    // tell them apart — which is why rule 1 exists.
    const SCAN_OPEN_FAILED: &str = r#"Opening subject.mkv...
[10:04:13] hb_scan: path=subject.mkv, title_index=1
[10:04:13] hb_stream_open: open subject.mkv failed
[10:04:13] scan: unrecognized file type
[10:04:13] libhb: scan thread found 0 valid title(s)
No title found.
"#;

    const INVALID_PRESET: &str = r#"[10:04:12] hb_init: starting libhb thread
Invalid preset No Such Preset 9000
Valid presets are:
"#;

    const WORK_RESULT_3: &str = r#"[10:01:06] libhb: work result = 3

Encode failed (error 3).
HandBrake has exited.
"#;

    fn facts<'a>(exit: Option<i32>, readable: bool, tail: &'a str) -> FailureFacts<'a> {
        FailureFacts {
            exit_code: exit,
            source_readable: readable,
            stderr_tail: tail,
        }
    }

    // THE LOAD-BEARING TEST. Identical stderr, identical exit code, opposite verdict.
    // The ONLY difference is our own readability observation. If rule 1 is ever removed
    // or reordered below rule 4, the second assertion flips to BadSource and ConvertBar
    // starts offering healthy files (on a hiccuping mount, a bad permission) for deletion.
    #[test]
    fn readability_is_the_only_thing_separating_corrupt_from_unreadable() {
        assert_eq!(
            classify(&facts(Some(2), true, SCAN_OPEN_FAILED)),
            FailureClass::BadSource,
            "we opened the file ourselves, so HandBrake failing to scan it means the file is bad"
        );
        assert_eq!(
            classify(&facts(Some(2), false, SCAN_OPEN_FAILED)),
            FailureClass::Environment,
            "we could NOT open it, so the identical HandBrake output proves nothing about the file"
        );
    }

    // A preset rename in a HandBrake upgrade exits 2 and would fire on EVERY queued file.
    // Rule 2 must be evaluated before rule 4 or one broken dependency trashes a library.
    #[test]
    fn invalid_preset_is_environment_despite_exit_2() {
        assert_eq!(
            classify(&facts(Some(2), true, INVALID_PRESET)),
            FailureClass::Environment
        );
    }

    #[test]
    fn exit_3_is_always_environment() {
        // Measured for both "output dir missing" and "output dir read-only" with a VALID
        // source. HandBrake signals bad input with 2, never 3.
        assert_eq!(
            classify(&facts(Some(3), true, WORK_RESULT_3)),
            FailureClass::Environment
        );
    }

    #[test]
    fn unrecognized_failures_are_unknown_and_never_destructive() {
        // No markers, exit 1.
        assert_eq!(
            classify(&facts(Some(1), true, "something went sideways")),
            FailureClass::Unknown
        );
        // Killed by a signal — no exit code at all.
        assert_eq!(classify(&facts(None, true, "")), FailureClass::Unknown);
        // Exit 2 but no source marker: not enough evidence to condemn the file.
        assert_eq!(
            classify(&facts(Some(2), true, "some unfamiliar diagnostic")),
            FailureClass::Unknown
        );
        // Exit 0 with an empty output stays Unknown deliberately — the cause of that
        // state is not understood well enough to destroy on it.
        assert_eq!(
            classify(&facts(Some(0), true, SCAN_OPEN_FAILED)),
            FailureClass::Unknown
        );
    }

    #[test]
    fn stored_strings_are_pinned() {
        // These strings are persisted in SQLite and queried by the review list. Changing
        // one silently orphans every existing row.
        assert_eq!(FailureClass::BadSource.as_str(), "bad_source");
        assert_eq!(
            FailureClass::BadSourceTruncated.as_str(),
            "bad_source_truncated"
        );
        assert_eq!(FailureClass::Environment.as_str(), "environment");
        assert_eq!(FailureClass::Unknown.as_str(), "unknown");
        assert_eq!(CLASS_BAD_SOURCE, "bad_source");
        assert_eq!(CLASS_BAD_SOURCE_TRUNCATED, "bad_source_truncated");
        assert_eq!(CLASS_BAD_SOURCE_PURGED, "bad_source_purged");
        assert_eq!(CLASS_BAD_SOURCE_RECOVERED, "bad_source_recovered");
        assert_ne!(
            FailureClass::Unknown.as_str(),
            "",
            "Unknown must never serialize to NULL/empty — NULL means 'predates this feature'"
        );
    }

    // Real tail from a truncated MP4 encode (HandBrakeCLI 1.11.2, exit 0).
    const TRUNCATED_TAIL: &str = r#"[10:01:21] sync: expecting 480 video frames
[10:01:21] h264-decoder done: 131 frames, 1 decoder errors
[10:01:21] sync: got 131 frames, 480 expected
[10:01:21] libhb: work result = 0
"#;

    // Real tail from a truncated MKV encode. Note ZERO decoder errors — a cleanly
    // truncated MKV decodes its available frames without error, so decoder errors must
    // NOT be a required condition or MKV truncation is missed entirely.
    const TRUNCATED_MKV_TAIL: &str = r#"[10:12:15] h264-decoder done: 155 frames, 0 decoder errors
[10:12:15] sync: got 155 frames, 480 expected
"#;

    const HEALTHY_TAIL: &str = r#"[10:01:59] h264-decoder done: 480 frames, 0 decoder errors
[10:01:59] sync: got 480 frames, 480 expected
"#;

    #[test]
    fn parses_the_frame_shortfall_marker() {
        assert_eq!(decode_shortfall(TRUNCATED_TAIL), Some((131, 480)));
        assert_eq!(decode_shortfall(TRUNCATED_MKV_TAIL), Some((155, 480)));
        assert_eq!(decode_shortfall(HEALTHY_TAIL), Some((480, 480)));
    }

    #[test]
    fn absent_or_garbled_marker_is_uncertainty_not_truncation() {
        assert_eq!(decode_shortfall(""), None);
        assert_eq!(decode_shortfall("no marker here at all"), None);
        assert_eq!(
            decode_shortfall("sync: got many frames, some expected"),
            None
        );
        assert_eq!(decode_shortfall("sync: got 131 frames"), None);
    }

    #[test]
    fn takes_the_last_marker_when_several_are_present() {
        // Defensive: multi-pass and subtitle-scan encodes were both checked and emit exactly
        // one line, but a Phase 2 false positive routes a HEALTHY file into the purge list,
        // so a stray earlier line must never decide the verdict.
        let two = "[00:00:01] sync: got 10 frames, 480 expected\n\
                   [00:00:09] sync: got 480 frames, 480 expected\n";
        assert_eq!(decode_shortfall(two), Some((480, 480)));
    }

    #[test]
    fn truncation_threshold_separates_real_cases_with_margin() {
        // (got, expected, expect_truncated, why)
        let cases = [
            (480u64, 480u64, false, "healthy CFR MP4 decodes every frame"),
            (
                150,
                150,
                false,
                "healthy VFR — expected comes from sync's own accounting",
            ),
            (131, 480, true, "truncated MP4, 27%"),
            (155, 480, true, "truncated MKV, 32%, zero decoder errors"),
            (593, 2880, true, "truncated 2-min clip, 21%"),
            (
                0,
                0,
                false,
                "no frames expected — uncertainty, never truncated",
            ),
            (0, 480, true, "nothing decoded at all"),
            (432, 480, false, "exactly at the 90% floor is not truncated"),
            (431, 480, true, "just past the floor is truncated"),
        ];
        for (got, expected, want, why) in cases {
            assert_eq!(is_truncated(got, expected), want, "{why}");
        }
    }

    #[test]
    fn promote_stored_diagnostic_rewrites_old_banner_first_messages() {
        let old = "Conversion failed:\n\
                   [00:00:00] Compile-time hardening features are enabled\n\
                   [mov] moov atom not found\n\
                   No title found.";
        let promoted =
            promote_stored_diagnostic(old).expect("a banner-first legacy row should be rewritten");
        assert_eq!(
            promoted.lines().next().unwrap(),
            "Conversion failed: [mov] moov atom not found"
        );
        assert!(promoted.contains("No title found."), "detail is preserved");
        // Idempotent: a second pass over the rewritten message is a no-op.
        assert_eq!(promote_stored_diagnostic(&promoted), None);
    }

    #[test]
    fn promote_stored_diagnostic_leaves_foreign_messages_untouched() {
        // Already headlined (space after the prefix, not a bare "prefix:").
        assert_eq!(
            promote_stored_diagnostic(
                "Conversion failed: moov atom not found\nmoov atom not found"
            ),
            None
        );
        // Single-line generic fallback with no tail to promote from.
        assert_eq!(promote_stored_diagnostic("Conversion failed"), None);
        // Legacy shape but nothing diagnostic in the body — leave it rather than promote noise.
        assert_eq!(
            promote_stored_diagnostic("Conversion failed:\nScanning title 1\nOpening file"),
            None
        );
        // The empty-output prefix is handled too.
        assert_eq!(
            promote_stored_diagnostic(
                "Conversion produced an empty output file:\nbanner\nNo space left on device"
            )
            .unwrap()
            .lines()
            .next()
            .unwrap(),
            "Conversion produced an empty output file: No space left on device"
        );
    }
}
