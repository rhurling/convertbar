//! Live end-to-end run of a remote root over the real ssh (ADRs 0005, 0006).
//!
//! Every other remote test drives the ssh shim, which runs the real remote scripts but on this
//! machine's filesystem and with this machine's tools. This one creates a scratch directory under
//! a real host's share, uploads an `.mp4` and a `.wmv` with an owner, group and mode no fresh
//! file there would have, and starts `convertbar-server` with that directory as its only remote
//! root and a keep-every-track MP4 preset file. It queues both over HTTP with an empty suffix,
//! then checks on the host that the `.mp4` was replaced in place, the `.wmv` was converted beside
//! itself and then deleted, every result kept its original's owner, group and mode, and no marked
//! temp was left. It also pulls the results back and checks them with ffprobe, and checks the
//! captured post-convert payloads. The printed report is the slice's recorded artifact, and the
//! scratch directory is removed afterwards.
//!
//! Needs HandBrakeCLI, ffmpeg and ffprobe on PATH, and a host reachable by `ssh -o BatchMode=yes`
//! with GNU coreutils ≥ 9.5. Run with:
//!   cargo test -p convertbar-server --test live_remote_roots -- --ignored --nocapture
//! `CONVERTBAR_LIVE_HOST` (default `unraid`) and `CONVERTBAR_LIVE_DIR` (default `/mnt/user/DATA`)
//! choose where the scratch directory goes.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PRESET: &str = "MP4 Keep All";
/// What the uploaded originals are given, so a result that kept them can be told from a fresh file.
const OWNER: &str = "99:100";
const MODE: &str = "640";

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The scratch directory on the host, removed however the test ends.
struct Scratch {
    host: String,
    dir: String,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Only ever a directory this test made: the pattern is the guard.
        let script = format!(
            "case '{d}' in */.convertbar-live-*) rm -r -- '{d}' ;; esac",
            d = self.dir
        );
        let _ = ssh_cmd(&self.host, &script).status();
    }
}

fn ssh_cmd(host: &str, script: &str) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=20",
        "--",
        host,
        script,
    ]);
    cmd
}

fn ssh(host: &str, script: &str) -> String {
    let out = ssh_cmd(host, script).output().expect("run ssh");
    assert!(
        out.status.success(),
        "ssh {host} {script:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn upload(host: &str, local: &Path, remote: &str) {
    let file = std::fs::File::open(local).unwrap();
    let status = ssh_cmd(host, &format!("cat > '{remote}'"))
        .stdin(file)
        .status()
        .expect("run ssh");
    assert!(status.success(), "upload of {remote} failed");
}

fn download(host: &str, remote: &str, local: &Path) {
    let file = std::fs::File::create(local).unwrap();
    let status = ssh_cmd(host, &format!("cat -- '{remote}'"))
        .stdout(file)
        .status()
        .expect("run ssh");
    assert!(status.success(), "download of {remote} failed");
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

/// A lossless 2-second MP4 with two audio and two subtitle tracks, so the re-encode is smaller
/// (the job ends `done`) and a dropped track shows.
fn make_mp4(dir: &Path) -> PathBuf {
    for (lang, text) in [("eng", "Hello"), ("ger", "Hallo")] {
        std::fs::write(
            dir.join(format!("{lang}.srt")),
            format!("1\n00:00:00,000 --> 00:00:01,500\n{text}\n"),
        )
        .unwrap();
    }
    let source = dir.join("clip.mp4");
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
        .args(["-c:s", "mov_text"])
        .args([
            "-metadata:s:s:0",
            "language=eng",
            "-metadata:s:s:1",
            "language=ger",
        ])
        .arg(&source));
    source
}

/// A 2-second WMV at a bitrate any HEVC encode undercuts.
fn make_wmv(dir: &Path) -> PathBuf {
    let source = dir.join("film.wmv");
    run(Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=2:size=1280x720:rate=25",
        ])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .args(["-c:v", "wmv2", "-b:v", "20M", "-c:a", "wmav2"])
        .arg(&source));
    source
}

/// (container format name, first video codec, video, audio, subtitle stream counts).
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
fn a_remote_mp4_is_replaced_in_place_and_a_wmv_converted_beside_with_the_original_deleted() {
    let host = std::env::var("CONVERTBAR_LIVE_HOST").unwrap_or_else(|_| "unraid".to_string());
    let share =
        std::env::var("CONVERTBAR_LIVE_DIR").unwrap_or_else(|_| "/mnt/user/DATA".to_string());
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("local");
    std::fs::create_dir(&local).unwrap();

    let preset_file = tmp.path().join("presets.json");
    run(Command::new("HandBrakeCLI")
        .args(["-Z", "Fast 480p30", "--all-audio", "--all-subtitles"])
        .args(["-e", "x265", "--encoder-preset", "ultrafast", "-q", "30"])
        .args(["--preset-export", PRESET, "--preset-export-file"])
        .arg(&preset_file));

    let mp4 = make_mp4(&local);
    let wmv = make_wmv(&local);
    let before = [probe(&mp4), probe(&wmv)];

    let scratch = Scratch {
        dir: ssh(
            &host,
            &format!("mktemp -d '{share}/.convertbar-live-XXXXXX'"),
        ),
        host: host.clone(),
    };
    let dir = scratch.dir.clone();
    let remote_mp4 = format!("{dir}/clip.mp4");
    let remote_wmv = format!("{dir}/film.wmv");
    let remote_out = format!("{dir}/film.mp4");
    upload(&host, &mp4, &remote_mp4);
    upload(&host, &wmv, &remote_wmv);
    ssh(
        &host,
        &format!("chown {OWNER} '{remote_mp4}' '{remote_wmv}' && chmod {MODE} '{remote_mp4}' '{remote_wmv}'"),
    );

    let payloads = tmp.path().join("payloads.jsonl");
    let hook = tmp.path().join("post-convert.sh");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$CONVERTBAR_PAYLOAD\" >> '{}'\n",
            payloads.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // A fresh data dir: no watchers and no real queue — never the user's convertbar.db. The only
    // remote root is the scratch directory.
    let _server = Server(
        Command::new(env!("CARGO_BIN_EXE_convertbar-server"))
            .env("CONVERTBAR_DATA_DIR", tmp.path().join("data"))
            .env("CONVERTBAR_NO_AUTH", "1")
            .env("CONVERTBAR_BIND", "127.0.0.1")
            .env("CONVERTBAR_PORT", port.to_string())
            .env("CONVERTBAR_PRESET_FILE", &preset_file)
            .env("CONVERTBAR_REMOTE_ROOTS", format!("{host}:{dir}"))
            .env("CONVERTBAR_STAGING_DIR", tmp.path().join("staging"))
            .env("CONVERTBAR_POST_CONVERT_COMMAND", &hook)
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

    let path = format!("/api/presets/{}/suffix", PRESET.replace(' ', "%20"));
    assert_eq!(http(port, "PUT", &path, Some(json!({"suffix": ""}))).0, 204);
    let setting = Some(json!({"value": PRESET}));
    assert_eq!(http(port, "PUT", "/api/settings/preset", setting).0, 204);
    let setting = Some(json!({"value": "delete"}));
    assert_eq!(
        http(port, "PUT", "/api/settings/cleanup_mode", setting).0,
        204
    );

    let (status, listing) = http(
        port,
        "GET",
        &format!(
            "/api/fs/list?remote={host}&path={}",
            dir.replace(' ', "%20")
        ),
        None,
    );
    assert_eq!(status, 200, "browse refused: {listing}");
    let mut names: Vec<&str> = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["clip.mp4", "film.wmv"]);

    let paths = json!({"remote": host, "path": dir});
    let (status, added) = http(port, "POST", "/api/queue/folder", Some(paths));
    assert_eq!(status, 200, "add refused: {added}");
    assert_eq!(added["added"].as_array().unwrap().len(), 2, "{added}");
    assert_eq!(http(port, "POST", "/api/converter/start", None).0 / 100, 2);

    let deadline = Instant::now() + Duration::from_secs(600);
    let history = loop {
        let (_, queue) = http(port, "GET", "/api/queue", None);
        let (_, history) = http(port, "GET", "/api/history?limit=10&offset=0", None);
        let jobs = queue.as_array().expect("the queue is a list");
        let failed: Vec<_> = jobs.iter().filter(|j| j["status"] == "error").collect();
        assert!(failed.is_empty(), "a remote job failed: {failed:#?}");
        if jobs.is_empty() {
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
        "both must finish done: {history:#}"
    );

    // On the host: the in-place result under the original's name, the new name beside the
    // deleted .wmv, both with the originals' owner, group and mode, and no marked temp left.
    let listing = ssh(&host, &format!("cd '{dir}' && ls -A | sort"));
    assert_eq!(listing, "clip.mp4\nfilm.mp4", "what is left on {host}");
    let stats = ssh(
        &host,
        &format!("stat -c '%n %u:%g %a %s' -- '{remote_mp4}' '{remote_out}'"),
    );
    for line in stats.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        assert_eq!(
            (fields[1], fields[2]),
            (OWNER, MODE),
            "{line}: owner, group or mode not taken over"
        );
    }

    let results = [local.join("result-clip.mp4"), local.join("result-film.mp4")];
    download(&host, &remote_mp4, &results[0]);
    download(&host, &remote_out, &results[1]);
    let after = [probe(&results[0]), probe(&results[1])];

    let captured = std::fs::read_to_string(&payloads).expect("the post-convert hook ran");
    let captured: Vec<Value> = captured
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(captured.len(), 2, "one payload per job: {captured:#?}");
    for p in &captured {
        assert_eq!(p["remote_host"], host.as_str(), "{p:#}");
        assert_eq!(p["status"], "done", "{p:#}");
        assert_eq!(p["skip_reason"], Value::Null, "{p:#}");
    }

    println!("report: host {host}, scratch {dir}, preset file {preset_file:?}, preset {PRESET:?}");
    println!(
        "  remote stat after (name uid:gid mode size):\n    {}",
        stats.replace('\n', "\n    ")
    );
    for (label, b, a) in [
        ("clip.mp4 -> clip.mp4 (in place)", &before[0], &after[0]),
        (
            "film.wmv -> film.mp4 (beside, original deleted)",
            &before[1],
            &after[1],
        ),
    ] {
        println!("  {label}: before {b:?}, after {a:?}");
        assert!(a.0.contains("mp4"), "{label}: not MP4: {a:?}");
        assert_eq!(a.1, "hevc", "{label}: not re-encoded");
        assert_eq!(
            (a.2, a.3, a.4),
            (b.2, b.3, b.4),
            "{label}: a track was dropped"
        );
    }
    for p in &captured {
        println!("  post-convert payload: {p}");
    }
}
