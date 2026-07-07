//! Source-file introspection via `HandBrakeCLI --scan --json`: produces a
//! `media_skip::SourceMedia` (normalized codec slug + height) for the skip-by-source-media
//! policy. Kept separate from `handbrake.rs` (preset handling) and the pure `media_skip.rs`.

use crate::media_skip::SourceMedia;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Hard ceiling on a single `--scan`. Scanning one file is normally a few seconds; this exists
/// only so a pathological or stalled file (a half-written download, a wedged network mount) can't
/// wedge the background folder scan indefinitely. On timeout the child is killed and the caller
/// treats the resulting `None` as uncertainty — the file is queued rather than skipped.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

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
/// Returns `None` if the process fails to launch, exceeds `PROBE_TIMEOUT`, or yields no parseable
/// title — the caller treats `None` as uncertainty and queues the file rather than skipping it.
pub fn probe_source(handbrake_path: &str, file: &str) -> Option<SourceMedia> {
    let mut cmd = Command::new(handbrake_path);
    cmd.args(["--scan", "--json", "-i", file]);
    scan_with(cmd)
}

/// The spawn/drain/parse core of a probe, taking the prepared command so tests can
/// substitute a scan-shaped process without exec-ing a temp script.
fn scan_with(mut cmd: Command) -> Option<SourceMedia> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Drain stdout concurrently with the wait: with `--json`, scan Progress blocks and
    // multi-title/track sets can exceed the OS pipe buffer (~64KB). An undrained pipe
    // blocks HandBrake on write forever, so it would sit until the timeout kills it —
    // stalling the scan of every such file for the full PROBE_TIMEOUT.
    let stdout_thread = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s);
            s
        })
    });

    let status = wait_with_timeout(&mut child, PROBE_TIMEOUT);
    // The child has exited (or been killed), so the reader hit EOF: join is prompt.
    let stdout = stdout_thread.and_then(|t| t.join().ok())?;
    status?;
    parse_scan_media(&stdout)
}

/// Wait for `child` to exit, polling so we never block indefinitely. If it outlives `timeout` it
/// is killed and `None` is returned. Mirrors `converter::wait_for_active_child`'s poll-don't-block
/// approach so a stalled scan can't wedge the caller.
pub(crate) fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
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

    // A wedged HandBrake scan must not hang the caller forever: it is killed at the deadline and
    // reported as uncertainty (None), which the skip policy treats as "queue, don't skip".
    #[cfg(unix)]
    #[test]
    fn wait_with_timeout_kills_a_process_that_overruns() {
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let started = Instant::now();
        let result = wait_with_timeout(&mut child, Duration::from_millis(200));
        assert!(
            result.is_none(),
            "an overrunning scan must time out to None"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must return shortly after the deadline, not wait out the child"
        );
        assert!(
            child.try_wait().expect("try_wait").is_some(),
            "the timed-out child must have been killed, not leaked"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_survives_output_larger_than_the_pipe_buffer() {
        // A scan-shaped process floods stdout well past the ~64KB pipe buffer before
        // emitting a parseable title set. Without a concurrent drain, the flood blocks
        // the child on write and every such probe stalls for the full PROBE_TIMEOUT.
        // Spawns /bin/sh directly — exec-ing a written temp script is fragile in CI
        // (noexec tmp, fork/exec ETXTBSY races).
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "yes flood | head -c 200000; printf 'JSON Title Set: {\"TitleList\":[{\"Geometry\":{\"Height\":240},\"VideoCodec\":\"h264\"}]}'",
        ]);

        let started = Instant::now();
        let media = scan_with(cmd);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "an oversized scan output must not stall until the timeout kill"
        );
        let media = media.expect("the title set after the flood must still parse");
        assert_eq!(media.codec, "h264");
        assert_eq!(media.height, 240);
    }

    #[cfg(unix)]
    #[test]
    fn wait_with_timeout_returns_status_for_a_fast_process() {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let status = wait_with_timeout(&mut child, Duration::from_secs(5));
        assert!(
            status.map(|s| s.success()).unwrap_or(false),
            "a process that exits before the deadline returns its status"
        );
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
