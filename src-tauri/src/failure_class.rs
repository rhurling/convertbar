//! Pure policy for deciding WHY a conversion failed, so a genuinely bad source file can be
//! told apart from a broken environment. No I/O — every function here is table-testable,
//! mirroring `media_skip.rs`. The caller gathers the facts; this module only decides.
//!
//! The governing rule is the same as the skip policy's: uncertainty is never destructive.

// Nothing calls this module yet — the encode loop wires it in as a follow-up task. Silence
// dead_code until then rather than let clippy -D warnings block landing the policy on its own.
#![allow(dead_code)]

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
        assert_ne!(
            FailureClass::Unknown.as_str(),
            "",
            "Unknown must never serialize to NULL/empty — NULL means 'predates this feature'"
        );
    }
}
