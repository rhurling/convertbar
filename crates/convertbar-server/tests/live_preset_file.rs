//! Live end-to-end run of a preset file through the real server binary (ADR 0004).
//!
//! Every other preset-file test drives a stand-in HandBrake script, which proves ConvertBar passes
//! the flags it means to — not that HandBrake reads them the way we assume. This one asks the
//! host's HandBrakeCLI to export a keep-every-track MKV preset, starts `convertbar-server` with it
//! in `CONVERTBAR_PRESET_FILE`, queues an `.mkv` and an `.mp4` over HTTP with an empty suffix, and
//! checks with ffprobe that the `.mkv` was replaced in place as Matroska, the `.mp4` gained an
//! `.mkv` beside it, and neither lost an audio or subtitle track. The printed report is the
//! slice's recorded artifact. Needs HandBrakeCLI, ffmpeg and ffprobe on PATH. Run with:
//!   cargo test -p convertbar-server --test live_preset_file -- --ignored --nocapture

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PRESET: &str = "MKV Keep All";

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn http(port: u16, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to server");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, payload) = raw.split_once("\r\n\r\n").expect("an HTTP response");
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    // Only a body-less reply (204) reads as Null; anything else must be JSON, so a rejected
    // request cannot pass for an empty answer.
    let json = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload)
            .unwrap_or_else(|e| panic!("{method} {path} answered {status}: {payload} ({e})"))
    };
    (status, json)
}

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("spawn tool");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A lossless 2-second clip (so the re-encode is smaller and the job ends `done`, not
/// `skipped`) with two audio tracks and two subtitle tracks — more than the built-in presets
/// keep, which is the point of a custom preset.
fn make_source(dir: &Path, name: &str, subtitle_codec: &str) -> PathBuf {
    for (lang, text) in [("eng", "Hello"), ("ger", "Hallo")] {
        std::fs::write(
            dir.join(format!("{lang}.srt")),
            format!("1\n00:00:00,000 --> 00:00:01,500\n{text}\n"),
        )
        .unwrap();
    }
    let source = dir.join(name);
    run(Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=1280x720:rate=25",
        ])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .args(["-f", "lavfi", "-i", "sine=frequency=880:duration=2"])
        .arg("-i")
        .arg(dir.join("eng.srt"))
        .arg("-i")
        .arg(dir.join("ger.srt"))
        .args([
            "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3", "-map", "4",
        ])
        .args([
            "-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv420p", "-c:a", "aac",
        ])
        .args(["-c:s", subtitle_codec])
        .args([
            "-metadata:s:s:0",
            "language=eng",
            "-metadata:s:s:1",
            "language=ger",
        ])
        .arg(&source));
    source
}

/// (container format name, first video codec, video, audio, subtitle stream counts) as
/// ffprobe reads them.
fn probe(path: &Path) -> (String, String, usize, usize, usize) {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe could not read {path:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let count = |kind: &str| {
        v["streams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["codec_type"] == kind)
            .count()
    };
    (
        v["format"]["format_name"].as_str().unwrap().to_string(),
        v["streams"][0]["codec_name"].as_str().unwrap().to_string(),
        count("video"),
        count("audio"),
        count("subtitle"),
    )
}

#[test]
#[ignore]
fn a_keep_all_mkv_preset_file_replaces_mkv_in_place_and_writes_mkv_beside_mp4() {
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    std::fs::create_dir(&media).unwrap();

    // The preset file comes from HandBrake's own exporter, the way the dotfiles side makes one.
    let preset_file = tmp.path().join("presets.json");
    run(Command::new("HandBrakeCLI")
        .args(["-Z", "H.265 MKV 480p30", "--all-audio", "--all-subtitles"])
        .args(["-e", "x265", "--encoder-preset", "ultrafast", "-q", "30"])
        .args(["--preset-export", PRESET, "--preset-export-file"])
        .arg(&preset_file));

    let mkv_source = make_source(&media, "film.mkv", "srt");
    let mp4_source = make_source(&media, "clip.mp4", "mov_text");
    let mp4_output = media.join("clip.mkv");
    let before = [probe(&mkv_source), probe(&mp4_source)];

    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // A fresh data dir: no watchers and no real queue — never the user's convertbar.db.
    let _server = Server(
        Command::new(env!("CARGO_BIN_EXE_convertbar-server"))
            .env("CONVERTBAR_DATA_DIR", tmp.path().join("data"))
            .env("CONVERTBAR_NO_AUTH", "1")
            .env("CONVERTBAR_BIND", "127.0.0.1")
            .env("CONVERTBAR_PORT", port.to_string())
            .env("CONVERTBAR_PRESET_FILE", &preset_file)
            .env_remove("CONVERTBAR_AUTH_TOKEN")
            .stdout(Stdio::null())
            .spawn()
            .expect("start convertbar-server"),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "server never started listening");
        std::thread::sleep(Duration::from_millis(100));
    }

    let (_, presets) = http(port, "GET", "/api/handbrake/presets", None);
    assert_eq!(presets[0], PRESET, "the file's preset must be listed first");

    let path = format!("/api/presets/{}/suffix", PRESET.replace(' ', "%20"));
    assert_eq!(http(port, "PUT", &path, Some(json!({"suffix": ""}))).0, 204);
    let setting = Some(json!({"value": PRESET}));
    assert_eq!(http(port, "PUT", "/api/settings/preset", setting).0, 204);
    let setting = Some(json!({"value": "delete"}));
    assert_eq!(
        http(port, "PUT", "/api/settings/cleanup_mode", setting).0,
        204
    );

    let paths = json!({"paths": [mkv_source, mp4_source]});
    let (status, added) = http(port, "POST", "/api/queue/files", Some(paths));
    assert_eq!(status, 200, "add refused: {added}");
    assert_eq!(added["added"].as_array().unwrap().len(), 2, "{added}");
    assert_eq!(http(port, "POST", "/api/converter/start", None).0 / 100, 2);

    let deadline = Instant::now() + Duration::from_secs(300);
    let history = loop {
        let (_, queue) = http(port, "GET", "/api/queue", None);
        let (_, history) = http(port, "GET", "/api/history?limit=10&offset=0", None);
        if queue.as_array().is_some_and(|q| q.is_empty()) {
            break history;
        }
        assert!(Instant::now() < deadline, "queue never drained: {queue}");
        std::thread::sleep(Duration::from_millis(500));
    };
    let statuses: Vec<_> = history["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("no jobs in history: {history}"))
        .iter()
        .map(|j| &j["status"])
        .collect();
    assert_eq!(
        statuses,
        ["done", "done"],
        "both encodes must finish done: {history:#}"
    );

    // In place: the .mkv source's own path now holds the re-encode, and no temp file is left.
    // Beside: the .mp4 source got an .mkv next to it, then was deleted (cleanup_mode delete).
    assert!(mkv_source.exists() && mp4_output.exists() && !mp4_source.exists());
    let leftovers: Vec<_> = std::fs::read_dir(&media)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".mkv") || n.ends_with(".mp4"))
        .collect();
    let mut leftovers = leftovers;
    leftovers.sort();
    assert_eq!(leftovers, ["clip.mkv", "film.mkv"]);

    let after = [probe(&mkv_source), probe(&mp4_output)];
    println!("report: preset file {preset_file:?}, preset {PRESET:?}");
    for (label, b, a) in [
        ("film.mkv -> film.mkv (in place)", &before[0], &after[0]),
        ("clip.mp4 -> clip.mkv (beside)", &before[1], &after[1]),
    ] {
        println!("  {label}: before {b:?}, after {a:?}");
        assert!(a.0.contains("matroska"), "{label}: not Matroska: {a:?}");
        // h264 before, hevc after: the file was re-encoded, not merely left in place.
        assert_eq!(
            (b.1.as_str(), a.1.as_str()),
            ("h264", "hevc"),
            "{label}: not re-encoded"
        );
        assert_eq!(
            (a.2, a.3, a.4),
            (b.2, b.3, b.4),
            "{label}: a track was dropped"
        );
    }
}
