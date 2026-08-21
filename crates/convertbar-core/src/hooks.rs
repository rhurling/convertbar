//! Post-convert hooks: payload construction, templating, path mapping, and the
//! `HookRunner` I/O seam. Everything above the trait is pure and tested without I/O.

use serde_json::{json, Value};

// emit_t lives on the EventSinkExt extension trait, not on EventSink itself.
use crate::events::EventSinkExt;

/// One finished job, exactly as booked in the `jobs` table plus three derived fields.
#[derive(Debug, Clone, PartialEq)]
pub struct JobPayload {
    pub job_id: String,
    pub status: String,
    pub source_path: String,
    pub output_path: String,
    /// The file that EXISTS now: output_path when kept_file is "converted", source_path when
    /// it is "original". Note that "original" is NOT only the `skipped` status: `KeptFile::
    /// Neither` (no usable output) also books kept_file = "original" while the job stays
    /// `done`, so a `done` job can legitimately carry the source path here. None for every
    /// other value of kept_file, including NULL — which is what an error row carries, cleanup
    /// failures included, because those arms call `record_job_error` and never set kept_file.
    /// Receivers act on this, never on output_path — see the spec.
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

/// Conservative cap on any SINGLE environment value handed to a command hook.
///
/// Linux rejects an env string longer than `MAX_ARG_STRLEN` (128 KiB) with `E2BIG`; macOS caps
/// args+env together at 256 KiB. 96 KiB sits under both with room for the rest of the
/// environment, and exists to turn an unreadable `E2BIG` from `spawn()` into a sentence naming
/// the variable and its size. `QUEUE_DRAINED_BATCH` is the primary defence — this is the
/// backstop for when a batch's per-job size estimate is wrong (very long paths, say).
///
/// The WEBHOOK path has no such limit: it is the escape hatch for very large payloads.
pub const MAX_COMMAND_ENV_VALUE_BYTES: usize = 96 * 1024;

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

        // Sits next to the spawn() it protects: past this point an oversized variable comes
        // back as a bare `E2BIG` from the OS, which tells an operator nothing.
        for (k, v) in &req.env {
            if v.len() > MAX_COMMAND_ENV_VALUE_BYTES {
                return Err(format!(
                    "{k} is {}k, over the {}k limit a command hook can receive; \
                     reduce the batch or use a webhook",
                    v.len() / 1024,
                    MAX_COMMAND_ENV_VALUE_BYTES / 1024
                ));
            }
        }

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
            // Known accepted edge case: a try_wait() I/O error returns here without killing
            // the child, leaving it unmanaged. This path is the waiter thread failing to poll
            // the OS, not the command itself failing, and is rare enough not to warrant a kill
            // attempt against a process handle we may not trust anymore.
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

/// How a head wires up hooks. `allow_stored_command` is the head's policy on whether the
/// `post_convert_command` / `queue_drained_command` settings ROWS are an accepted source.
///
/// The server head sets this false and reads the command from the environment only. "Env
/// first, then fall back to the row" applied uniformly would be a live hazard: a
/// convertbar.db copied or migrated from a desktop install carries a command row, and the
/// container would execute it. Copying a live database into a head has already caused one
/// incident in this project.
pub struct HookSetup {
    pub runner: std::sync::Arc<dyn HookRunner>,
    pub allow_stored_command: bool,
}

// --- The fire path: configuration, dispatch, and failure surfacing ---

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trigger {
    PostConvert,
    QueueDrained,
}

impl Trigger {
    fn event_name(self) -> &'static str {
        match self {
            Trigger::PostConvert => "post-convert",
            Trigger::QueueDrained => "queue-drained",
        }
    }
    fn url_key(self) -> &'static str {
        match self {
            Trigger::PostConvert => "post_convert_webhook_url",
            Trigger::QueueDrained => "queue_drained_webhook_url",
        }
    }
    fn headers_key(self) -> &'static str {
        match self {
            Trigger::PostConvert => "post_convert_webhook_headers",
            Trigger::QueueDrained => "queue_drained_webhook_headers",
        }
    }
    fn body_key(self) -> &'static str {
        match self {
            Trigger::PostConvert => "post_convert_webhook_body",
            Trigger::QueueDrained => "queue_drained_webhook_body",
        }
    }
    fn command_key(self) -> &'static str {
        match self {
            Trigger::PostConvert => "post_convert_command",
            Trigger::QueueDrained => "queue_drained_command",
        }
    }
    fn command_env_var(self) -> &'static str {
        match self {
            Trigger::PostConvert => "CONVERTBAR_POST_CONVERT_COMMAND",
            Trigger::QueueDrained => "CONVERTBAR_QUEUE_DRAINED_COMMAND",
        }
    }
}

pub struct HookConfig {
    pub url: String,
    pub headers: String,
    pub body: String,
    pub command: String,
    pub path_map: PathMap,
    pub timeout: Duration,
}

impl HookConfig {
    pub fn is_off(&self) -> bool {
        self.url.trim().is_empty() && self.command.trim().is_empty()
    }
}

fn setting(db: &rusqlite::Connection, key: &str) -> String {
    db.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_default()
}

/// Reads a trigger's configuration. CALLER MUST HOLD `ctx.db` and drop the guard before
/// dispatching — never hold it across a hook (CLAUDE.md, "Emitting Events Under the DB Lock").
pub fn read_hook_config(
    db: &rusqlite::Connection,
    trigger: Trigger,
    allow_stored_command: bool,
) -> HookConfig {
    // Environment wins. The stored row is consulted only where the head allows it.
    let command = std::env::var(trigger.command_env_var())
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| {
            if allow_stored_command {
                setting(db, trigger.command_key())
            } else {
                String::new()
            }
        });

    HookConfig {
        url: setting(db, trigger.url_key()),
        headers: setting(db, trigger.headers_key()),
        body: setting(db, trigger.body_key()),
        command,
        path_map: PathMap::parse(&setting(db, "hook_path_map")),
        timeout: Duration::from_secs(parse_timeout_seconds(&setting(db, "hook_timeout_seconds"))),
    }
}

/// Runs a trigger's configured hooks. MUST be called with no `ctx.db` guard held.
pub fn dispatch(ctx: &crate::ctx::Ctx, trigger: Trigger, cfg: &HookConfig, payload: Value) {
    if cfg.is_off() {
        return;
    }
    // Checked here, not only at the queue loop head: a quit during a hook would otherwise
    // block the queue thread for up to the timeout and could orphan a command child.
    if ctx.converter.is_shutting_down() {
        return;
    }

    if !cfg.url.trim().is_empty() {
        // Path mapping is a webhook concern only — a script rewrites paths itself.
        let mut mapped = payload.clone();
        map_payload_paths(&mut mapped, &cfg.path_map);
        let result = parse_headers(&cfg.headers).and_then(|headers| {
            let body = if cfg.body.trim().is_empty() {
                mapped.to_string()
            } else {
                render_body(&cfg.body, &mapped)
            };
            ctx.hooks.runner.run_webhook(&WebhookRequest {
                url: cfg.url.trim().to_string(),
                headers,
                body,
                timeout: cfg.timeout,
            })
        });
        if let Err(e) = result {
            report_failure(ctx, trigger, &format!("webhook: {e}"));
        }
    }

    if !cfg.command.trim().is_empty() {
        let result = ctx.hooks.runner.run_command(&CommandRequest {
            command: cfg.command.trim().to_string(),
            env: command_env(&payload), // raw, unmapped
            timeout: cfg.timeout,
        });
        if let Err(e) = result {
            report_failure(ctx, trigger, &format!("command: {e}"));
        }
    }
}

/// A hook failure never changes a job's status — the encode succeeded. It always logs and
/// emits; it notifies only once per queue run, because a broken receiver on a 200-file queue
/// would otherwise produce 200 notifications.
fn report_failure(ctx: &crate::ctx::Ctx, trigger: Trigger, reason: &str) {
    let event = trigger.event_name();
    eprintln!("convertbar: {event} hook failed — {reason}");
    ctx.events
        .emit_t("hook-failed", json!({ "event": event, "reason": reason }));

    // Counted so Task 6's queue-drained fire can tell whether ITS dispatch failed and
    // therefore whether the watermark may advance.
    ctx.converter
        .hook_failure_count
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let already = {
        let mut flag = ctx.converter.hook_failure_notified.lock().unwrap();
        std::mem::replace(&mut *flag, true)
    };
    if !already {
        ctx.events
            .notify("ConvertBar", &format!("{event} hook failed — {reason}"));
    }
}

/// Builds a payload from a job row. Returns None when the row is gone (the in-place + keep
/// case deletes it), which is exactly when there is nothing to report.
pub fn load_job_payload(db: &rusqlite::Connection, job_id: &str) -> Option<JobPayload> {
    db.query_row(
        "SELECT id, status, source_path, output_path, preset, kept_file, original_size, \
         converted_size, space_saved, error_message, failure_class, started_at, completed_at \
         FROM jobs WHERE id = ?1",
        rusqlite::params![job_id],
        |r| {
            Ok(row_to_payload(
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
            ))
        },
    )
    .ok()
}

#[allow(clippy::too_many_arguments)]
fn row_to_payload(
    job_id: String,
    status: String,
    source_path: String,
    output_path: String,
    preset: String,
    kept_file: Option<String>,
    original_size: Option<i64>,
    converted_size: Option<i64>,
    space_saved: Option<i64>,
    error_message: Option<String>,
    failure_class: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
) -> JobPayload {
    // The file that EXISTS now, keyed on kept_file: "converted" is the re-encode; "original"
    // is the skipped case, where the converted file was discarded, AND `KeptFile::Neither`,
    // where there was no usable output at all — that one stays status='done', so "original"
    // does not imply "skipped". Everything else — NULL included, which is what every error row
    // carries (the cleanup-failure arms call `record_job_error`, which leaves kept_file NULL)
    // — has no surviving result to name. Never blindly output_path.
    let result_path = match kept_file.as_deref() {
        Some("converted") => Some(output_path.clone()),
        Some("original") => Some(source_path.clone()),
        _ => None,
    };
    let output_dir = result_path.as_ref().and_then(|p| {
        std::path::Path::new(p)
            .parent()
            .map(|d| d.to_string_lossy().to_string())
    });
    let duration_seconds = match (&started_at, &completed_at) {
        (Some(s), Some(c)) => {
            let s = chrono::DateTime::parse_from_rfc3339(s).ok();
            let c = chrono::DateTime::parse_from_rfc3339(c).ok();
            match (s, c) {
                (Some(s), Some(c)) => Some((c - s).num_seconds()),
                _ => None,
            }
        }
        _ => None,
    };

    JobPayload {
        job_id,
        status,
        source_path: source_path.clone(),
        output_path: output_path.clone(),
        result_path,
        output_dir,
        in_place: crate::converter::is_in_place(&source_path, &output_path),
        preset,
        kept_file,
        original_size,
        converted_size,
        space_saved,
        duration_seconds,
        error_message,
        failure_class,
        started_at,
        completed_at,
    }
}

/// The single per-file entry point, called from BOTH fire points so neither builds a payload
/// and the two cannot drift.
pub fn fire_post_convert(ctx: &crate::ctx::Ctx, job_id: &str) {
    let (cfg, job) = {
        let db = match ctx.db.lock() {
            Ok(db) => db,
            Err(_) => return,
        };
        let cfg = read_hook_config(&db, Trigger::PostConvert, ctx.hooks.allow_stored_command);
        if cfg.is_off() {
            return;
        }
        let job = load_job_payload(&db, job_id);
        (cfg, job)
    }; // guard dropped BEFORE dispatch — a hook is slower than an emit, so the deadlock
       // window documented in CLAUDE.md is wider here, not narrower.

    let Some(job) = job else { return };
    dispatch(ctx, Trigger::PostConvert, &cfg, post_convert_payload(&job));
}

/// Where the last successful queue-drained report stopped. Persisted, not run-local: a pause,
/// a low-disk stop, or a restart must not lose the jobs completed before it.
const WATERMARK_KEY: &str = "last_queue_drained_at";

/// Jobs per drain payload.
///
/// This is a HARD safety bound, not a tuning knob. `command_env` puts the whole payload into
/// `CONVERTBAR_PAYLOAD`, and `run_command` sets that with `Command::env` before `spawn()`.
/// Linux caps a single env string at `MAX_ARG_STRLEN` (128 KiB); macOS caps args+env together
/// at 256 KiB. An unbounded payload therefore fails `spawn()` with `E2BIG` — and because that
/// failure correctly refuses to advance the watermark, the NEXT drain would build an even
/// larger payload. A wedge that never self-heals. At roughly 500 bytes per job object a full
/// batch is ~50 KB, comfortably under both caps with room for long paths.
const QUEUE_DRAINED_BATCH: usize = 100;

/// One batch of jobs completed since the watermark, oldest first. Reading from the table
/// rather than an in-memory accumulator is what lets the payload survive a pause, a low-disk
/// stop, or a restart — a run-local Vec would silently drop everything completed before the
/// interruption. Errors are logged, never silently returned as "nothing new".
pub fn load_jobs_since(
    db: &rusqlite::Connection,
    watermark: &str,
    limit: usize,
) -> Vec<JobPayload> {
    let mut stmt = match db.prepare(
        "SELECT id, status, source_path, output_path, preset, kept_file, original_size, \
         converted_size, space_saved, error_message, failure_class, started_at, completed_at \
         FROM jobs WHERE completed_at IS NOT NULL AND completed_at > ?1 \
         ORDER BY completed_at ASC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("convertbar: queue-drained hook could not prepare its job query — {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(rusqlite::params![watermark, limit as i64], |r| {
        Ok(row_to_payload(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
            r.get(8)?,
            r.get(9)?,
            r.get(10)?,
            r.get(11)?,
            r.get(12)?,
        ))
    });
    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            eprintln!("convertbar: queue-drained hook could not read completed jobs — {e}");
            Vec::new()
        }
    }
}

/// Read as a DELTA around a single dispatch, never absolutely — see the field's own doc on
/// `ConverterState::hook_failure_count`.
fn hook_failures_seen(ctx: &crate::ctx::Ctx) -> usize {
    ctx.converter
        .hook_failure_count
        .load(std::sync::atomic::Ordering::SeqCst)
}

/// Fires on a TRUE drain, in batches of `QUEUE_DRAINED_BATCH`. Advances the watermark only
/// after a batch dispatches cleanly, so a failed hook re-reports the same jobs next time
/// rather than losing them — and the batch bound keeps that retry from growing without limit.
/// A whole backlog drains inside this one call, not one batch per queue run.
pub fn fire_queue_drained(ctx: &crate::ctx::Ctx) {
    let cfg = {
        let db = match ctx.db.lock() {
            Ok(db) => db,
            Err(_) => return,
        };
        let cfg = read_hook_config(&db, Trigger::QueueDrained, ctx.hooks.allow_stored_command);
        if cfg.is_off() {
            return;
        }
        cfg
    };

    loop {
        let (watermark, jobs) = {
            let db = match ctx.db.lock() {
                Ok(db) => db,
                Err(_) => return,
            };
            // An absent watermark sorts before every RFC3339 timestamp, so a first run reports
            // every completed job in History. That one-time burst is preferred over seeding the
            // watermark at migration time, which would make a fresh install and an upgrade
            // behave differently for no visible reason. It arrives batched, not as one payload.
            let watermark = setting(&db, WATERMARK_KEY);
            let jobs = load_jobs_since(&db, &watermark, QUEUE_DRAINED_BATCH);
            (watermark, jobs)
        }; // guard dropped BEFORE dispatch — a hook can block for the full timeout.

        if jobs.is_empty() {
            return; // nothing new — an idle queue emits no empty payloads
        }
        let more_may_remain = jobs.len() >= QUEUE_DRAINED_BATCH;

        // A full batch may have cut a group of rows sharing one `completed_at` in half. The
        // next pass asks for `> watermark`, so the rows on the far side of that cut would be
        // silently SKIPPED — not re-sent. Drop the trailing tied rows here and let them come
        // back whole, together with their siblings, in the next iteration.
        let mut jobs = jobs;
        if more_may_remain {
            if let Some(boundary) = jobs.last().and_then(|j| j.completed_at.clone()) {
                let first_tied = jobs
                    .iter()
                    .position(|j| j.completed_at.as_deref() == Some(boundary.as_str()))
                    .unwrap_or(0);
                if first_tied == 0 {
                    // Every row in a full batch shares one timestamp: the tie group is larger
                    // than a batch and cannot be split, so send it whole. Rows beyond it that
                    // share the timestamp WILL be skipped. That residual is acceptable; a
                    // SILENT one is not, hence the log.
                    eprintln!(
                        "convertbar: queue-drained tie group at {boundary} fills an entire \
                         {QUEUE_DRAINED_BATCH}-job batch — any further jobs sharing that exact \
                         completed_at will be skipped"
                    );
                } else {
                    jobs.truncate(first_tied);
                }
            }
        }

        // AFTER the truncation, so the watermark names the last row actually reported.
        let newest = jobs.iter().filter_map(|j| j.completed_at.clone()).max();

        let failures_before = hook_failures_seen(ctx);
        dispatch(
            ctx,
            Trigger::QueueDrained,
            &cfg,
            queue_drained_payload(&jobs),
        );
        if hook_failures_seen(ctx) != failures_before {
            return; // do not advance past jobs the receiver never heard about
        }

        // TERMINATION GUARD. Everything past here must either advance the watermark or stop:
        // this loop re-selects from the same table on the next pass, so a watermark that does
        // not move re-sends the identical batch forever on the queue thread.
        let Some(newest) = newest else {
            eprintln!(
                "convertbar: queue-drained batch carried no usable completed_at — stopping \
                 rather than re-sending the same rows"
            );
            return;
        };
        if newest == watermark {
            eprintln!(
                "convertbar: queue-drained watermark did not advance past {newest} — stopping \
                 rather than re-sending the same rows"
            );
            return;
        }

        // Only now is ctx.db re-locked: dispatch has fully returned, so no guard was ever held
        // across a hook.
        let advanced = {
            let db = match ctx.db.lock() {
                Ok(db) => db,
                Err(_) => return,
            };
            db.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                rusqlite::params![WATERMARK_KEY, &newest],
            )
        };
        // A failed write is the other way the watermark stays put; same rule applies.
        if let Err(e) = advanced {
            eprintln!("convertbar: queue-drained watermark could not be stored — {e}");
            return;
        }

        if !more_may_remain {
            return;
        }
        eprintln!(
            "convertbar: queue-drained batch was full ({QUEUE_DRAINED_BATCH} jobs) — more \
             remain, sending the next batch"
        );
    }
}

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

    #[test]
    fn http_runner_refuses_an_oversized_env_value_with_a_readable_message() {
        // An operator must read a sentence, not decode `E2BIG` from spawn(). The command is
        // /usr/bin/true — present on macOS and Linux, unlike /bin/true, which does not exist on
        // macOS — so a green result cannot be the program failing to resolve instead.
        let r = HttpHookRunner;
        let req = CommandRequest {
            command: "/usr/bin/true".into(),
            env: vec![(
                "CONVERTBAR_PAYLOAD".into(),
                "x".repeat(MAX_COMMAND_ENV_VALUE_BYTES + 1),
            )],
            timeout: Duration::from_secs(10),
        };
        let err = r.run_command(&req).unwrap_err();
        assert!(
            err.contains("CONVERTBAR_PAYLOAD"),
            "the error must name the variable: {err}"
        );
        assert!(err.contains("96k"), "the error must name the limit: {err}");
        assert!(
            err.contains("webhook"),
            "the error must name the way out: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn http_runner_accepts_an_env_value_just_under_the_cap() {
        // Guards the guard: proves the cap is a boundary, not a blanket refusal that would
        // make the test above pass for the wrong reason. Unix-gated like every other test here
        // that spawns a real program.
        let r = HttpHookRunner;
        let req = CommandRequest {
            command: "/usr/bin/true".into(),
            env: vec![(
                "CONVERTBAR_PAYLOAD".into(),
                "x".repeat(MAX_COMMAND_ENV_VALUE_BYTES),
            )],
            timeout: Duration::from_secs(10),
        };
        assert!(r.run_command(&req).is_ok());
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
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("finished");
        let script = dir.path().join("slow.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 5\ntouch \"$MARKER\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let r = HttpHookRunner;
        let req = CommandRequest {
            command: script.to_string_lossy().to_string(),
            env: vec![("MARKER".into(), marker.to_string_lossy().to_string())],
            timeout: Duration::from_secs(1),
        };

        let started = std::time::Instant::now();
        let err = r.run_command(&req).unwrap_err();
        assert!(err.to_lowercase().contains("timed out"), "got: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "should have returned at the timeout, took {:?}",
            started.elapsed()
        );

        // The kill is what this test is NAMED for, and the assertions above pass with kill()
        // deleted. Wait well past the script's own sleep and prove it never reached its final
        // line: if the child survived the timeout, the marker exists.
        std::thread::sleep(Duration::from_secs(6));
        assert!(
            !marker.exists(),
            "child survived the timeout — kill() did not run"
        );
    }
}
