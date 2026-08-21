//! Post-convert hooks: payload construction, templating, path mapping, and the
//! `HookRunner` I/O seam. Everything above the trait is pure and tested without I/O.

use serde_json::{json, Value};

/// One finished job, exactly as booked in the `jobs` table plus three derived fields.
#[derive(Debug, Clone, PartialEq)]
pub struct JobPayload {
    pub job_id: String,
    pub status: String,
    pub source_path: String,
    pub output_path: String,
    /// The file that EXISTS now: output_path when kept_file is "converted", source_path when
    /// it is "original" (the skipped and cleanup-failure cases). None on error. Receivers act
    /// on this, never on output_path — see the spec.
    pub result_path: Option<String>,
    pub output_dir: Option<String>,
    pub in_place: bool,
    pub preset: String,
    pub kept_file: Option<String>,
    pub original_size: Option<i64>,
    pub converted_size: Option<i64>,
    /// Optimization delta, `original - converted`. Negative on a skipped job by design.
    pub space_saved: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub error_message: Option<String>,
    pub failure_class: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Every path-valued key in a payload object. Path mapping rewrites exactly these.
const PATH_FIELDS: &[&str] = &["source_path", "output_path", "result_path", "output_dir"];

fn job_object(j: &JobPayload) -> Value {
    json!({
        "job_id": j.job_id,
        "status": j.status,
        "source_path": j.source_path,
        "output_path": j.output_path,
        "result_path": j.result_path,
        "output_dir": j.output_dir,
        "in_place": j.in_place,
        "preset": j.preset,
        "kept_file": j.kept_file,
        "original_size": j.original_size,
        "converted_size": j.converted_size,
        "space_saved": j.space_saved,
        "duration_seconds": j.duration_seconds,
        "error_message": j.error_message,
        "failure_class": j.failure_class,
        "started_at": j.started_at,
        "completed_at": j.completed_at,
    })
}

pub fn post_convert_payload(job: &JobPayload) -> Value {
    let mut v = job_object(job);
    v.as_object_mut()
        .expect("job_object builds an object")
        .insert("event".into(), json!("post-convert"));
    v
}

pub fn queue_drained_payload(jobs: &[JobPayload]) -> Value {
    let errors = jobs.iter().filter(|j| j.status == "error").count();
    let completed = jobs.len() - errors;
    let space_saved: i64 = jobs.iter().filter_map(|j| j.space_saved).sum();

    // Deduped, first-seen order. An error job has no output_dir and contributes nothing:
    // rescanning a directory for a file that was never produced is at best wasted work.
    let mut output_dirs: Vec<String> = Vec::new();
    for d in jobs.iter().filter_map(|j| j.output_dir.as_ref()) {
        if !output_dirs.iter().any(|e| e == d) {
            output_dirs.push(d.clone());
        }
    }

    json!({
        "event": "queue-drained",
        // From the reported set, NOT from had_errors: a cancelled job sets had_errors but
        // contributes no row, and {"run_status":"error","errors":0} must be impossible.
        "run_status": if errors > 0 { "error" } else { "idle" },
        "completed": completed,
        "errors": errors,
        "space_saved": space_saved,
        "output_dirs": output_dirs,
        "jobs": jobs.iter().map(job_object).collect::<Vec<_>>(),
    })
}

/// Prefix rewrite rules, pre-sorted longest-`from`-first so a more specific rule wins
/// regardless of the order the user typed them in.
#[derive(Debug, Default, Clone)]
pub struct PathMap(Vec<(String, String)>);

impl PathMap {
    pub fn parse(raw: &str) -> Self {
        let mut rules: Vec<(String, String)> = raw
            .lines()
            .filter_map(|line| {
                let (from, to) = line.split_once("=>")?;
                let (from, to) = (from.trim(), to.trim());
                // Strip a trailing separator from both sides so the rule is stored in the same
                // form `apply`'s segment-boundary check expects: a trailing slash consumed into
                // `from` would leave `apply` no separator to match on for any real subpath, so
                // `/media/ => ...` would silently never rewrite; a trailing slash left on `to`
                // would double up into `/data//...`.
                let from = from.trim_end_matches(['/', '\\']);
                let to = to.trim_end_matches(['/', '\\']);
                if from.is_empty() {
                    // Also covers a degenerate `/ => ...` root rule, once trimmed — there is no
                    // sensible meaning to invent for rewriting the filesystem root, so it is
                    // skipped like any other empty `from`.
                    return None;
                }
                Some((from.to_string(), to.to_string()))
            })
            .collect();
        rules.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        PathMap(rules)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// First matching rule wins and rewriting stops there — deliberately not chained, so a
    /// map of `/media => /data` plus `/data => /zzz` cannot rewrite one path twice.
    pub fn apply(&self, path: &str) -> String {
        for (from, to) in &self.0 {
            let rest = match path.strip_prefix(from.as_str()) {
                Some(r) => r,
                None => continue,
            };
            // Segment boundary only: `/media` must not match `/mediafoo`.
            if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') {
                return format!("{to}{rest}");
            }
        }
        path.to_string()
    }
}

/// Rewrites every path-valued field in a payload, recursing into `jobs[]` and `output_dirs`.
pub fn map_payload_paths(v: &mut Value, map: &PathMap) {
    if map.is_empty() {
        return;
    }
    if let Some(obj) = v.as_object_mut() {
        for key in PATH_FIELDS {
            if let Some(Value::String(s)) = obj.get(*key) {
                let mapped = map.apply(s);
                obj.insert((*key).to_string(), json!(mapped));
            }
        }
        if let Some(Value::Array(dirs)) = obj.get_mut("output_dirs") {
            for d in dirs.iter_mut() {
                if let Value::String(s) = d {
                    *d = json!(map.apply(s));
                }
            }
        }
        if let Some(Value::Array(jobs)) = obj.get_mut("jobs") {
            for j in jobs.iter_mut() {
                map_payload_paths(j, map);
            }
        }
    }
}

/// Substitutes `{{placeholder}}` from `payload`.
///
/// Scalars are JSON-escaped WITHOUT surrounding quotes, so the template supplies them and a
/// path containing `"` or `\` cannot break out of the surrounding JSON. `null` renders as the
/// empty string, never the token `null`. A `_json` suffix inserts pre-formed JSON raw, and
/// `{{payload_json}}` is the whole payload. An unknown placeholder is left untouched — a
/// silent empty string would send a malformed request that looks well-formed.
///
/// `_json` insertion is deliberately NOT context-sensitive: it always means "insert raw at a
/// JSON value position." A `_json` placeholder used inside an already-open JSON string (rather
/// than at a value position) will produce invalid JSON — that is by design, not a bug; see
/// `a_json_placeholder_inside_a_string_is_not_special_cased` below.
pub fn render_body(template: &str, payload: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = match after.find("}}") {
            Some(e) => e,
            None => {
                // Unterminated: emit the rest verbatim and stop. Must RETURN, not break — the
                // prefix before `{{` is already in `out`, so falling through to the trailing
                // push would append it a second time ("abc{{def" -> "abcabc{{def").
                out.push_str(&rest[start..]);
                return out;
            }
        };
        let name = after[..end].trim();
        let tail = &after[end + 2..];

        match resolve_placeholder(name, payload) {
            Some(text) => out.push_str(&text),
            None => {
                out.push_str("{{");
                out.push_str(&after[..end]);
                out.push_str("}}");
            }
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn resolve_placeholder(name: &str, payload: &Value) -> Option<String> {
    if name == "payload_json" {
        return Some(payload.to_string());
    }
    if let Some(key) = name.strip_suffix("_json") {
        return payload.get(key).map(|v| v.to_string());
    }
    let v = payload.get(name)?;
    Some(match v {
        Value::Null => String::new(),
        Value::String(s) => {
            // Escape as a JSON string, then strip the quotes the template supplies.
            let quoted = Value::String(s.clone()).to_string();
            quoted[1..quoted.len() - 1].to_string()
        }
        other => other.to_string(),
    })
}

/// One `Name: value` per line. Blank lines ignored. `Content-Type: application/json` is added
/// unless the user set one. A line without a colon is a configuration error and fails loud
/// rather than firing with the bad line dropped.
pub fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("header line is missing a ':' — {line}"))?;
        let name = name.trim();
        if name.is_empty() {
            return Err(format!("header line has an empty name — {line}"));
        }
        out.push((name.to_string(), value.trim().to_string()));
    }
    if !out
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        out.push(("Content-Type".into(), "application/json".into()));
    }
    Ok(out)
}

pub const DEFAULT_HOOK_TIMEOUT_SECONDS: u64 = 30;

pub fn parse_timeout_seconds(raw: &str) -> u64 {
    raw.trim()
        .parse::<u64>()
        .map(|v| v.clamp(1, 300))
        .unwrap_or(DEFAULT_HOOK_TIMEOUT_SECONDS)
}

/// Splits a command line into program + arguments. Quoted segments stay intact; there are no
/// escape sequences, no variable expansion, and no globbing. Executed without a shell — a user
/// who wants shell semantics points the hook at a script.
pub fn split_command(raw: &str) -> Result<Vec<String>, String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has_part = false;

    for c in raw.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has_part = true;
            }
            None if c.is_whitespace() => {
                if has_part {
                    parts.push(std::mem::take(&mut cur));
                    has_part = false;
                }
            }
            None => {
                cur.push(c);
                has_part = true;
            }
        }
    }
    if quote.is_some() {
        return Err("command has an unterminated quote".into());
    }
    if has_part {
        parts.push(cur);
    }
    Ok(parts)
}

/// Environment for a command hook. Scalars become `CONVERTBAR_<UPPER>`; `null` becomes the
/// empty string. `output_dirs` is passed as a JSON array rather than a joined string, because
/// a path containing a space would otherwise be unrecoverable. `jobs` gets no variable — it
/// is reachable through `CONVERTBAR_PAYLOAD`, which always carries the whole payload.
pub fn command_env(payload: &Value) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    if let Some(obj) = payload.as_object() {
        for (k, v) in obj {
            if k == "jobs" {
                continue;
            }
            let name = format!("CONVERTBAR_{}", k.to_uppercase());
            let value = match v {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            env.push((name, value));
        }
    }
    env.push(("CONVERTBAR_PAYLOAD".into(), payload.to_string()));
    env
}

use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct WebhookRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandRequest {
    pub command: String,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

/// The hook I/O edge, injected per head so tests never touch the network or spawn processes
/// they did not ask for. Same pattern as `FileDisposer` and `HandbrakeLocator`.
pub trait HookRunner: Send + Sync {
    fn run_webhook(&self, req: &WebhookRequest) -> Result<(), String>;
    fn run_command(&self, req: &CommandRequest) -> Result<(), String>;
}

/// Production runner: real HTTP, real process spawn.
pub struct HttpHookRunner;

impl HookRunner for HttpHookRunner {
    fn run_webhook(&self, req: &WebhookRequest) -> Result<(), String> {
        // RootCerts::PlatformVerifier is REQUIRED, not merely the cargo feature: enabling
        // the feature only makes the verifier available, and the default stays webpki's
        // bundled Mozilla roots. Without this line a receiver behind a private CA or a
        // self-signed homelab proxy fails, and no test can catch it because every test uses
        // a recording runner.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(req.timeout))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();

        let mut request = agent.post(&req.url);
        for (name, value) in &req.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        match request.send(req.body.as_bytes()) {
            Ok(_) => Ok(()),
            Err(ureq::Error::StatusCode(code)) => Err(format!("receiver returned HTTP {code}")),
            Err(e) => Err(e.to_string()),
        }
    }

    fn run_command(&self, req: &CommandRequest) -> Result<(), String> {
        let parts = split_command(&req.command)?;
        let (program, args) = parts
            .split_first()
            .ok_or_else(|| "command is empty".to_string())?;

        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        for (k, v) in &req.env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;

        // std::process has no wait-with-timeout. Share the child with a waiter thread so the
        // timeout arm can still kill it. The waiter POLLS with try_wait rather than calling
        // wait(): wait() would hold the mutex for the child's entire lifetime and the timeout
        // arm could never lock it to kill. No libc and no per-platform code needed.
        let child = std::sync::Arc::new(Mutex::new(child));
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let child = std::sync::Arc::clone(&child);
            std::thread::spawn(move || loop {
                let polled = { child.lock().unwrap().try_wait() };
                match polled {
                    Ok(Some(status)) => {
                        let _ = tx.send(Ok(status));
                        return;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
            });
        }

        match rx.recv_timeout(req.timeout) {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(format!(
                "command exited with status {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into())
            )),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                let _ = child.lock().unwrap().kill();
                Err(format!("command timed out after {:?}", req.timeout))
            }
        }
    }
}

/// Test-harness default: records what was asked for, performs no I/O, always succeeds.
/// Unlike `PanickingLocator`, this does NOT fail loud — every `process_queue` test reaches a
/// fire point, and with no hook configured the fire is a no-op anyway.
#[derive(Default)]
pub struct RecordingHookRunner {
    pub webhooks: Mutex<Vec<WebhookRequest>>,
    pub commands: Mutex<Vec<CommandRequest>>,
}

impl HookRunner for RecordingHookRunner {
    fn run_webhook(&self, req: &WebhookRequest) -> Result<(), String> {
        self.webhooks.lock().unwrap().push(req.clone());
        Ok(())
    }
    fn run_command(&self, req: &CommandRequest) -> Result<(), String> {
        self.commands.lock().unwrap().push(req.clone());
        Ok(())
    }
}

/// Drives the failure-surfacing paths.
pub struct FailingHookRunner;

impl HookRunner for FailingHookRunner {
    fn run_webhook(&self, _req: &WebhookRequest) -> Result<(), String> {
        Err("receiver refused".into())
    }
    fn run_command(&self, _req: &CommandRequest) -> Result<(), String> {
        Err("receiver refused".into())
    }
}

// Deliberately NO SlowHookRunner. Timeout enforcement lives inside `HttpHookRunner` — the
// ureq agent config for webhooks, `recv_timeout` for commands — so a slow *double* would only
// block `dispatch` for its full sleep and prove nothing about the timeout. Real timeout
// behaviour is covered by `http_runner_kills_a_command_that_outruns_the_timeout` above,
// against a real child process.

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> JobPayload {
        JobPayload {
            job_id: "j1".into(),
            status: "done".into(),
            source_path: "/media/movies/x.mkv".into(),
            output_path: "/media/movies/x.h265.mkv".into(),
            result_path: Some("/media/movies/x.h265.mkv".into()),
            output_dir: Some("/media/movies".into()),
            in_place: false,
            preset: "Fast 1080p30".into(),
            kept_file: Some("converted".into()),
            original_size: Some(4160749568),
            converted_size: Some(1073741824),
            space_saved: Some(3087007744),
            duration_seconds: Some(412),
            error_message: None,
            failure_class: None,
            started_at: Some("2026-08-21T10:00:00+00:00".into()),
            completed_at: Some("2026-08-21T10:06:52+00:00".into()),
        }
    }

    fn skipped() -> JobPayload {
        JobPayload {
            status: "skipped".into(),
            result_path: Some("/media/movies/x.mkv".into()),
            output_dir: Some("/media/movies".into()),
            kept_file: Some("original".into()),
            space_saved: Some(-500),
            ..sample()
        }
    }

    fn errored() -> JobPayload {
        JobPayload {
            status: "error".into(),
            result_path: None,
            output_dir: None,
            kept_file: None,
            converted_size: None,
            space_saved: None,
            duration_seconds: None,
            error_message: Some("boom".into()),
            failure_class: Some("bad_source".into()),
            ..sample()
        }
    }

    // ---- payload ----

    #[test]
    fn post_convert_payload_carries_event_and_fields() {
        let v = post_convert_payload(&sample());
        assert_eq!(v["event"], "post-convert");
        assert_eq!(v["status"], "done");
        assert_eq!(v["result_path"], "/media/movies/x.h265.mkv");
        assert_eq!(v["space_saved"], 3087007744i64);
    }

    #[test]
    fn skipped_result_path_is_the_source_not_the_deleted_output() {
        // The whole reason result_path exists: on a skipped job output_path names a file
        // that was discarded. A receiver told to scan it would scan nothing.
        let v = post_convert_payload(&skipped());
        assert_eq!(v["result_path"], "/media/movies/x.mkv");
        assert_ne!(v["result_path"], v["output_path"]);
    }

    #[test]
    fn error_payload_has_null_result_path_and_carries_the_failure() {
        let v = post_convert_payload(&errored());
        assert!(v["result_path"].is_null());
        assert_eq!(v["error_message"], "boom");
        assert_eq!(v["failure_class"], "bad_source");
    }

    #[test]
    fn queue_drained_aggregates_counts_dirs_and_savings() {
        let v = queue_drained_payload(&[sample(), skipped(), errored()]);
        assert_eq!(v["event"], "queue-drained");
        assert_eq!(v["completed"], 2); // done + skipped
        assert_eq!(v["errors"], 1);
        assert_eq!(v["space_saved"], 3087007244i64); // 3087007744 + (-500) + 0
                                                     // deduped, first-seen order, and the error job contributes no directory
        assert_eq!(v["output_dirs"], serde_json::json!(["/media/movies"]));
        assert_eq!(v["jobs"].as_array().unwrap().len(), 3);
        // jobs[] elements carry no "event" key
        assert!(v["jobs"][0].get("event").is_none());
    }

    #[test]
    fn queue_drained_run_status_ignores_cancelled_jobs() {
        // run_status comes from the reported set, never from had_errors — a cancelled job
        // sets had_errors but contributes no row, and {"run_status":"error","errors":0}
        // must be unrepresentable.
        assert_eq!(queue_drained_payload(&[sample()])["run_status"], "idle");
        assert_eq!(queue_drained_payload(&[errored()])["run_status"], "error");
    }

    // ---- path mapping ----

    #[test]
    fn path_map_rewrites_a_prefix() {
        let m = PathMap::parse("/media => /data");
        assert_eq!(m.apply("/media/movies/x.mkv"), "/data/movies/x.mkv");
    }

    #[test]
    fn path_map_longest_prefix_wins_regardless_of_line_order() {
        let m = PathMap::parse("/media => /a\n/media/movies => /b");
        assert_eq!(m.apply("/media/movies/x.mkv"), "/b/x.mkv");
    }

    #[test]
    fn path_map_matches_only_on_a_segment_boundary() {
        let m = PathMap::parse("/media => /data");
        assert_eq!(m.apply("/mediafoo/x.mkv"), "/mediafoo/x.mkv");
    }

    #[test]
    fn path_map_does_not_chain_rewrites() {
        // /media -> /data, then /data -> /zzz must NOT apply to the same path twice.
        let m = PathMap::parse("/media => /data\n/data => /zzz");
        assert_eq!(m.apply("/media/x.mkv"), "/data/x.mkv");
    }

    #[test]
    fn empty_path_map_is_identity() {
        assert_eq!(PathMap::parse("").apply("/media/x.mkv"), "/media/x.mkv");
    }

    #[test]
    fn path_map_tolerates_trailing_slashes_on_either_side() {
        // "/media/ => /data/" is a rule a user would reasonably write. Before normalization
        // the trailing slash was consumed into `from`, so the remainder could never start
        // with a separator and the segment-boundary check rejected every real subpath —
        // the rule silently did nothing.
        let m = PathMap::parse("/media/ => /data/");
        assert_eq!(m.apply("/media/movies/x.mkv"), "/data/movies/x.mkv");
    }

    #[test]
    fn path_map_does_not_double_separators() {
        let m = PathMap::parse("/media => /data/");
        assert_eq!(m.apply("/media/movies/x.mkv"), "/data/movies/x.mkv");
    }

    #[test]
    fn path_map_still_honours_longest_prefix_after_normalization() {
        // Normalization must not disturb the longest-first sort.
        let m = PathMap::parse("/media/ => /a\n/media/movies/ => /b");
        assert_eq!(m.apply("/media/movies/x.mkv"), "/b/x.mkv");
    }

    #[test]
    fn map_payload_paths_rewrites_nested_jobs_too() {
        let mut v = queue_drained_payload(&[sample()]);
        map_payload_paths(&mut v, &PathMap::parse("/media => /data"));
        assert_eq!(v["output_dirs"], serde_json::json!(["/data/movies"]));
        assert_eq!(v["jobs"][0]["result_path"], "/data/movies/x.h265.mkv");
        assert_eq!(v["jobs"][0]["source_path"], "/data/movies/x.mkv");
        assert_eq!(v["jobs"][0]["output_dir"], "/data/movies");
    }

    #[test]
    fn map_payload_paths_leaves_nulls_alone() {
        let mut v = post_convert_payload(&errored());
        map_payload_paths(&mut v, &PathMap::parse("/media => /data"));
        assert!(v["result_path"].is_null());
    }

    // ---- templating ----

    #[test]
    fn a_bare_placeholder_renders_its_value() {
        let v = post_convert_payload(&sample());
        assert_eq!(render_body("{{status}}", &v), "done");
    }

    #[test]
    fn scalar_placeholders_are_json_escaped_without_quotes() {
        let mut p = sample();
        p.result_path = Some(r#"/media/a"b\c.mkv"#.into());
        let v = post_convert_payload(&p);
        let out = render_body(r#"{"p":"{{result_path}}"}"#, &v);
        // The rendered body must parse — the escaping is what makes that true.
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(parsed["p"], r#"/media/a"b\c.mkv"#);
    }

    #[test]
    fn numeric_placeholder_renders_unquoted_digits() {
        let v = post_convert_payload(&sample());
        assert_eq!(render_body("{{space_saved}}", &v), "3087007744");
    }

    #[test]
    fn null_placeholder_renders_empty_string_not_the_word_null() {
        let v = post_convert_payload(&errored());
        assert_eq!(render_body("{{result_path}}", &v), "");
    }

    #[test]
    fn json_suffix_placeholders_insert_raw_json() {
        let v = queue_drained_payload(&[sample()]);
        assert_eq!(
            render_body("{{output_dirs_json}}", &v),
            r#"["/media/movies"]"#
        );
    }

    #[test]
    fn payload_json_placeholder_renders_the_whole_payload() {
        let v = post_convert_payload(&sample());
        let out = render_body("{{payload_json}}", &v);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(parsed["job_id"], "j1");
    }

    #[test]
    fn the_stash_template_renders_to_valid_graphql_json() {
        let v = queue_drained_payload(&[sample()]);
        let out = render_body(
            r#"{"query":"mutation($input: ScanMetadataInput!) { metadataScan(input: $input) }","variables":{"input":{"paths":{{output_dirs_json}}}}}"#,
            &v,
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        // The array is passed as a GraphQL VARIABLE, so the placeholder sits at a JSON value
        // position and raw insertion is correct. Interpolating it into the query string
        // instead would splice unescaped quotes into that string and break the outer JSON.
        assert_eq!(
            parsed["variables"]["input"]["paths"],
            serde_json::json!(["/media/movies"])
        );
    }

    #[test]
    fn a_json_placeholder_inside_a_string_is_not_special_cased() {
        // Documents the rule's boundary: `_json` means "insert raw at a value position". It is
        // deliberately NOT context-sensitive, so this template produces invalid JSON rather
        // than silently escaping. The Stash example uses GraphQL variables to avoid it.
        let v = queue_drained_payload(&[sample()]);
        let out = render_body(r#"{"q":"x {{output_dirs_json}} y"}"#, &v);
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_err());
    }

    #[test]
    fn an_unterminated_placeholder_is_emitted_once_verbatim() {
        let v = post_convert_payload(&sample());
        assert_eq!(render_body("abc{{def", &v), "abc{{def");
    }

    #[test]
    fn unknown_placeholder_is_left_untouched() {
        let v = post_convert_payload(&sample());
        assert_eq!(render_body("{{nope}}", &v), "{{nope}}");
    }

    // ---- headers ----

    #[test]
    fn parse_headers_reads_lines_and_ignores_blanks() {
        let h = parse_headers("ApiKey: abc\n\nX-Other:  v \n").unwrap();
        assert!(h.contains(&("ApiKey".to_string(), "abc".to_string())));
        assert!(h.contains(&("X-Other".to_string(), "v".to_string())));
    }

    #[test]
    fn parse_headers_adds_default_content_type() {
        let h = parse_headers("").unwrap();
        assert!(h
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
    }

    #[test]
    fn explicit_content_type_overrides_the_default_case_insensitively() {
        let h = parse_headers("content-type: text/plain").unwrap();
        let cts: Vec<&String> = h
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v)
            .collect();
        assert_eq!(cts, vec!["text/plain"]);
    }

    #[test]
    fn parse_headers_rejects_a_line_without_a_colon() {
        // Fail loud: firing with the bad line silently dropped would send the wrong request.
        assert!(parse_headers("ApiKey abc").is_err());
    }

    // ---- timeout ----

    #[test]
    fn timeout_parses_clamps_and_defaults() {
        assert_eq!(parse_timeout_seconds("45"), 45);
        assert_eq!(parse_timeout_seconds("0"), 1);
        assert_eq!(parse_timeout_seconds("99999"), 300);
        assert_eq!(parse_timeout_seconds("banana"), 30);
        assert_eq!(parse_timeout_seconds(""), 30);
    }

    // ---- command ----

    #[test]
    fn split_command_splits_on_whitespace() {
        assert_eq!(
            split_command("/bin/x a b").unwrap(),
            vec!["/bin/x", "a", "b"]
        );
    }

    #[test]
    fn split_command_keeps_quoted_segments_intact() {
        assert_eq!(
            split_command(r#"/bin/x "a b" 'c d'"#).unwrap(),
            vec!["/bin/x", "a b", "c d"]
        );
    }

    #[test]
    fn split_command_does_not_expand_anything() {
        assert_eq!(
            split_command("/bin/x $HOME *").unwrap(),
            vec!["/bin/x", "$HOME", "*"]
        );
    }

    #[test]
    fn split_command_rejects_an_unterminated_quote() {
        assert!(split_command(r#"/bin/x "a b"#).is_err());
    }

    #[test]
    fn command_env_exposes_scalars_and_the_whole_payload() {
        let v = post_convert_payload(&sample());
        let env = command_env(&v);
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("CONVERTBAR_EVENT"), "post-convert");
        assert_eq!(get("CONVERTBAR_STATUS"), "done");
        assert_eq!(get("CONVERTBAR_RESULT_PATH"), "/media/movies/x.h265.mkv");
        assert_eq!(get("CONVERTBAR_SPACE_SAVED"), "3087007744");
        assert_eq!(get("CONVERTBAR_IN_PLACE"), "false");
        let payload: serde_json::Value =
            serde_json::from_str(&get("CONVERTBAR_PAYLOAD")).expect("valid JSON");
        assert_eq!(payload["job_id"], "j1");
    }

    #[test]
    fn command_env_renders_null_as_empty_string() {
        let env = command_env(&post_convert_payload(&errored()));
        assert_eq!(
            env.iter()
                .find(|(n, _)| n == "CONVERTBAR_RESULT_PATH")
                .unwrap()
                .1,
            ""
        );
    }

    #[test]
    fn command_env_renders_output_dirs_as_json_and_omits_jobs() {
        // A space-joined list would be unrecoverable for a path containing a space.
        let env = command_env(&queue_drained_payload(&[sample()]));
        assert_eq!(
            env.iter()
                .find(|(n, _)| n == "CONVERTBAR_OUTPUT_DIRS")
                .unwrap()
                .1,
            r#"["/media/movies"]"#
        );
        assert!(env.iter().all(|(n, _)| n != "CONVERTBAR_JOBS"));
    }

    // ---- HookRunner ----

    use std::time::Duration;

    #[test]
    fn recording_runner_captures_requests_and_succeeds() {
        let r = RecordingHookRunner::default();
        let req = WebhookRequest {
            url: "http://example.invalid/x".into(),
            headers: vec![("A".into(), "b".into())],
            body: "{}".into(),
            timeout: Duration::from_secs(1),
        };
        assert!(r.run_webhook(&req).is_ok());
        assert_eq!(r.webhooks.lock().unwrap().len(), 1);
        assert_eq!(
            r.webhooks.lock().unwrap()[0].url,
            "http://example.invalid/x"
        );
    }

    #[test]
    fn failing_runner_reports_an_error_for_both_mechanisms() {
        let r = FailingHookRunner;
        let w = WebhookRequest {
            url: "http://x.invalid".into(),
            headers: vec![],
            body: String::new(),
            timeout: Duration::from_secs(1),
        };
        let c = CommandRequest {
            command: "/bin/true".into(),
            env: vec![],
            timeout: Duration::from_secs(1),
        };
        assert!(r.run_webhook(&w).is_err());
        assert!(r.run_command(&c).is_err());
    }

    #[test]
    fn http_runner_rejects_an_empty_command() {
        let r = HttpHookRunner;
        let c = CommandRequest {
            command: "   ".into(),
            env: vec![],
            timeout: Duration::from_secs(1),
        };
        assert!(r.run_command(&c).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn http_runner_runs_a_real_command_and_passes_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.txt");
        let script = dir.path().join("hook.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s' \"$CONVERTBAR_STATUS\" > \"$CONVERTBAR_OUT\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let r = HttpHookRunner;
        let req = CommandRequest {
            command: script.to_string_lossy().to_string(),
            env: vec![
                ("CONVERTBAR_STATUS".into(), "done".into()),
                ("CONVERTBAR_OUT".into(), out.to_string_lossy().to_string()),
            ],
            timeout: Duration::from_secs(10),
        };
        r.run_command(&req).expect("hook script should succeed");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "done");
    }

    #[cfg(unix)]
    #[test]
    fn http_runner_reports_a_non_zero_exit() {
        let r = HttpHookRunner;
        let req = CommandRequest {
            command: "/bin/sh -c 'exit 3'".into(),
            env: vec![],
            timeout: Duration::from_secs(10),
        };
        let err = r.run_command(&req).unwrap_err();
        assert!(err.contains('3'), "error should name the exit code: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn http_runner_kills_a_command_that_outruns_the_timeout() {
        // A hung receiver must not wedge the queue thread forever.
        let r = HttpHookRunner;
        let req = CommandRequest {
            command: "/bin/sleep 30".into(),
            env: vec![],
            timeout: Duration::from_secs(1),
        };
        let started = std::time::Instant::now();
        let err = r.run_command(&req).unwrap_err();
        assert!(err.to_lowercase().contains("timed out"), "got: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "should have been killed at the timeout, took {:?}",
            started.elapsed()
        );
    }
}
