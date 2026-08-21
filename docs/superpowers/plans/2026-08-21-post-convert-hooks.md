# Post-Convert Hooks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fire a configurable webhook and/or local command after each conversion and once per true queue drain, carrying every fact ConvertBar knows about the job.

**Architecture:** All logic lives in a new head-agnostic `crates/convertbar-core/src/hooks.rs`. Payload construction, templating, header parsing, path mapping, and command splitting are pure functions tested without any I/O. The I/O edge sits behind a `HookRunner` trait injected through `Ctx`, following the existing `FileDisposer` / `HandbrakeLocator` pattern. Two fire points cover per-file outcomes; one watermark-driven fire point covers queue drains.

**Tech Stack:** Rust, rusqlite, serde_json, ureq 3 (new dependency), React + TypeScript for both UIs.

**Spec:** `docs/superpowers/specs/2026-08-21-post-convert-hooks-design.md` — read it before Task 1. It records *why* several non-obvious choices are the way they are, and those reasons are load-bearing.

## Global Constraints

- Rust: `cargo test --workspace` must pass after every task. `cargo fmt` before every commit.
- **Never hold the `ctx.db` lock across a hook call or an `emit_t`.** The desktop tray listener re-locks `ctx.db` synchronously on the same thread and `std::sync::Mutex` is not reentrant. Two shipped deadlocks came from violating this. Read settings into owned values inside a scoped block, drop the guard, then fire.
- Test fixtures must declare their HandBrake world (`AbsentLocator` / `StubLocator`); `PanickingLocator` is the default and panics on a queue thread poison `ctx.db`.
- `ureq` is pinned to `3` with the `platform-verifier` feature. Do not rely on bundled webpki-roots.
- New user-facing settings go in `ALLOWED_KEYS` **and** the `Settings` struct. `post_convert_command`, `queue_drained_command`, and `last_queue_drained_at` go in **neither** — that absence is the security boundary and the plan tests it.
- Conventional commits. Commit at the end of every task.
- `main` is protected; work happens on `feature/post-convert-hooks`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/convertbar-core/src/hooks.rs` (new) | Payload types, pure transform functions, `HookRunner` trait, `HttpHookRunner`, test doubles, the two fire functions |
| `crates/convertbar-core/src/lib.rs` | Register the module |
| `crates/convertbar-core/src/ctx.rs` | `HookSetup` field on `Ctx` |
| `crates/convertbar-core/src/converter.rs` | Two per-file fire points, `drained` flag, queue-drained fire, `ConverterState` flag |
| `crates/convertbar-core/src/settings_ops.rs` | New keys in `ALLOWED_KEYS` + `Settings`, hook settings reader |
| `crates/convertbar-core/src/db.rs` | Seed new settings defaults |
| `src-tauri/src/commands/hooks.rs` (new) | Desktop-only get/set for the command hook |
| `src/components/SettingsPage.tsx` | Hooks section, desktop + web variants |
| `README.md`, `docker-compose.example.yml`, `unraid-template.xml`, `CLAUDE.md` | Documentation |

---

### Task 1: Pure hook logic — payload, templating, mapping, parsing

**Files:**
- Create: `crates/convertbar-core/src/hooks.rs`
- Modify: `crates/convertbar-core/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `JobPayload`, `post_convert_payload`, `queue_drained_payload`, `PathMap`, `map_payload_paths`, `render_body`, `parse_headers`, `parse_timeout_seconds`, `split_command`, `command_env`.

- [ ] **Step 1: Register the module**

In `crates/convertbar-core/src/lib.rs`, add `pub mod hooks;` alongside the existing module declarations. An unregistered `.rs` file compiles to nothing and its tests silently do not run.

- [ ] **Step 2: Write the failing tests**

Create `crates/convertbar-core/src/hooks.rs` containing ONLY the test module below plus `use` lines. It will not compile yet — that is the point.

```rust
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
            r#"{"query":"mutation { metadataScan(input: {paths: {{output_dirs_json}}}) }"}"#,
            &v,
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(parsed["query"].as_str().unwrap().contains("/media/movies"));
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
        assert_eq!(split_command("/bin/x a b").unwrap(), vec!["/bin/x", "a", "b"]);
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
            env.iter().find(|(n, _)| n == "CONVERTBAR_RESULT_PATH").unwrap().1,
            ""
        );
    }

    #[test]
    fn command_env_renders_output_dirs_as_json_and_omits_jobs() {
        // A space-joined list would be unrecoverable for a path containing a space.
        let env = command_env(&queue_drained_payload(&[sample()]));
        assert_eq!(
            env.iter().find(|(n, _)| n == "CONVERTBAR_OUTPUT_DIRS").unwrap().1,
            r#"["/media/movies"]"#
        );
        assert!(env.iter().all(|(n, _)| n != "CONVERTBAR_JOBS"));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core hooks::`
Expected: FAIL — compile errors, `cannot find type JobPayload`, `cannot find function post_convert_payload`, and so on.

- [ ] **Step 4: Write the implementation**

Prepend to `crates/convertbar-core/src/hooks.rs`, above the test module:

```rust
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
                if from.is_empty() {
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
pub fn render_body(template: &str, payload: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = match after.find("}}") {
            Some(e) => e,
            None => break, // unterminated: emit the remainder verbatim
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
    if !out.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
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
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core hooks::`
Expected: PASS, all tests in the module.

- [ ] **Step 6: Format and commit**

```bash
cargo fmt
git add crates/convertbar-core/src/hooks.rs crates/convertbar-core/src/lib.rs
git commit -m "feat(hooks): pure payload, templating, path-mapping and parsing logic"
```

---

### Task 2: The `HookRunner` seam and its real implementation

**Files:**
- Modify: `crates/convertbar-core/src/hooks.rs`
- Modify: `crates/convertbar-core/Cargo.toml`

**Interfaces:**
- Consumes: `split_command`, `parse_headers` (Task 1).
- Produces: `WebhookRequest`, `CommandRequest`, `HookRunner`, `HttpHookRunner`, `RecordingHookRunner`, `FailingHookRunner`, `SlowHookRunner`.

- [ ] **Step 1: Add the dependency**

In `crates/convertbar-core/Cargo.toml`, under `[dependencies]`:

```toml
# Blocking HTTP for post-convert webhooks. `platform-verifier` is REQUIRED: ureq 3's default
# rustls setup bundles webpki-roots and ignores the OS trust store, so a receiver behind a
# private CA or a self-signed homelab proxy would fail with no obvious cause.
ureq = { version = "3", default-features = false, features = ["rustls", "platform-verifier", "gzip"] }
```

- [ ] **Step 2: Write the failing tests**

Append to the `mod tests` block in `hooks.rs`:

```rust
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
        assert_eq!(r.webhooks.lock().unwrap()[0].url, "http://example.invalid/x");
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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core hooks::`
Expected: FAIL — `cannot find type WebhookRequest`, `cannot find type HttpHookRunner`, and so on.

- [ ] **Step 4: Write the implementation**

Append to `hooks.rs`, above the test module:

```rust
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
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(req.timeout))
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

/// Blocks past any sane timeout, for tests that assert the queue is not wedged.
pub struct SlowHookRunner(pub Duration);

impl HookRunner for SlowHookRunner {
    fn run_webhook(&self, _req: &WebhookRequest) -> Result<(), String> {
        std::thread::sleep(self.0);
        Ok(())
    }
    fn run_command(&self, _req: &CommandRequest) -> Result<(), String> {
        std::thread::sleep(self.0);
        Ok(())
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core hooks::`
Expected: PASS. The timeout test should complete in roughly one second, not thirty.

- [ ] **Step 6: Verify the workspace still builds**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Format and commit**

```bash
cargo fmt
git add crates/convertbar-core/src/hooks.rs crates/convertbar-core/Cargo.toml Cargo.lock
git commit -m "feat(hooks): add HookRunner seam, ureq webhook and command runner"
```

---

### Task 3: Settings keys and the security boundary

**Files:**
- Modify: `crates/convertbar-core/src/settings_ops.rs`
- Modify: `crates/convertbar-core/src/db.rs:240-248`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: eight new `ALLOWED_KEYS` entries and eight matching `Settings` fields, all `String`.

**Background the engineer needs:** `update_setting` validates its key against `ALLOWED_KEYS` and
nothing else, and it is the only write path behind `PUT /api/settings/{key}`. `get_settings`
builds a typed `Settings` struct with an explicit `match` ending in `_ => {}`, so a key with no
field is dropped on read and never appears in `GET /api/settings`. Those two absences are the
entire security control for the command hook. Do not add `post_convert_command`,
`queue_drained_command`, or `last_queue_drained_at` to either list.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `settings_ops.rs`:

```rust
    #[test]
    fn hook_webhook_keys_are_writable_by_the_settings_ui() {
        let (ctx, _sink, _disp) = test_ctx(test_conn());
        for key in [
            "post_convert_webhook_url",
            "post_convert_webhook_headers",
            "post_convert_webhook_body",
            "queue_drained_webhook_url",
            "queue_drained_webhook_headers",
            "queue_drained_webhook_body",
            "hook_path_map",
            "hook_timeout_seconds",
        ] {
            assert!(
                ALLOWED_KEYS.contains(&key),
                "{key} is written by the Settings UI via update_setting"
            );
            assert!(update_setting(&ctx, key, "x").is_ok(), "{key} should be writable");
        }
    }

    #[test]
    fn command_hook_keys_are_not_writable_over_the_api() {
        // This absence IS the security boundary: update_setting is the only write path behind
        // PUT /api/settings/{key}, so a command key in ALLOWED_KEYS would make the server's
        // auth token the only thing between the network and arbitrary code execution.
        let (ctx, _sink, _disp) = test_ctx(test_conn());
        for key in ["post_convert_command", "queue_drained_command"] {
            assert!(!ALLOWED_KEYS.contains(&key), "{key} must NOT be remotely writable");
            assert!(update_setting(&ctx, key, "/bin/sh").is_err());
        }
    }

    #[test]
    fn internal_hook_keys_are_not_writable_over_the_api() {
        // Engine-written, like the three updater keys.
        assert!(!ALLOWED_KEYS.contains(&"last_queue_drained_at"));
    }

    #[test]
    fn command_hook_keys_do_not_leak_through_get_settings() {
        // The other half of the boundary: get_settings drops keys with no Settings field, so
        // GET /api/settings cannot read the command back out.
        let conn = test_conn();
        for key in ["post_convert_command", "queue_drained_command", "last_queue_drained_at"] {
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, 'SENTINEL')",
                rusqlite::params![key],
            )
            .unwrap();
        }
        let (ctx, _sink, _disp) = test_ctx(conn);
        let settings = get_settings(&ctx).unwrap();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            !json.contains("SENTINEL"),
            "a command key leaked into the settings snapshot: {json}"
        );
    }

    #[test]
    fn hook_settings_have_defaults() {
        let (ctx, _sink, _disp) = test_ctx(test_conn());
        let s = get_settings(&ctx).unwrap();
        assert_eq!(s.post_convert_webhook_url, "");
        assert_eq!(s.queue_drained_webhook_url, "");
        assert_eq!(s.hook_path_map, "");
        assert_eq!(s.hook_timeout_seconds, "30");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core settings_ops::`
Expected: FAIL — `no field post_convert_webhook_url on type Settings`, and the ALLOWED_KEYS
assertions fail.

- [ ] **Step 3: Implement**

In `settings_ops.rs`, add to `ALLOWED_KEYS` (after `"encode_priority"`):

```rust
    "post_convert_webhook_url",
    "post_convert_webhook_headers",
    "post_convert_webhook_body",
    "queue_drained_webhook_url",
    "queue_drained_webhook_headers",
    "queue_drained_webhook_body",
    "hook_path_map",
    "hook_timeout_seconds",
```

Add eight `pub` `String` fields with the same names to the `Settings` struct. In `get_settings`,
declare a `let mut <name> = String::new();` for each (except `hook_timeout_seconds`, which
starts as `String::from("30")`), add a `match` arm per key assigning `value`, and add each to the
returned struct literal.

In `db.rs`, add to the `defaults` array:

```rust
        ("post_convert_webhook_url", ""),
        ("post_convert_webhook_headers", ""),
        ("post_convert_webhook_body", ""),
        ("queue_drained_webhook_url", ""),
        ("queue_drained_webhook_headers", ""),
        ("queue_drained_webhook_body", ""),
        ("hook_path_map", ""),
        ("hook_timeout_seconds", "30"),
```

Do **not** seed `last_queue_drained_at`. An absent watermark means "no watermark", which the
Task 6 query handles.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core settings_ops::`
Expected: PASS.

- [ ] **Step 5: Check the frontend type still matches**

The `Settings` struct is mirrored in TypeScript. Run: `npx tsc`
Expected: PASS. If the mirror is a hand-written interface, add the eight `string` fields to it
now; Tasks 7 and 8 rely on them existing.

- [ ] **Step 6: Format and commit**

```bash
cargo fmt
git add crates/convertbar-core/src/settings_ops.rs crates/convertbar-core/src/db.rs src/
git commit -m "feat(hooks): add webhook settings keys and pin the command-hook boundary"
```

---

### Task 4: Inject the runner through `Ctx`

**Files:**
- Modify: `crates/convertbar-core/src/ctx.rs`
- Modify: all 16 `Ctx::new` call sites (see list below)

**Interfaces:**
- Consumes: `HookRunner`, `RecordingHookRunner`, `HttpHookRunner` (Task 2).
- Produces: `HookSetup { runner, allow_stored_command }`, `Ctx.hooks: HookSetup`, and a
  `Ctx::new` that takes `hooks: HookSetup` as its fifth parameter.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `ctx.rs` (create the module if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_carries_the_hook_setup_it_was_given() {
        let ctx = Ctx::new(
            rusqlite::Connection::open_in_memory().unwrap(),
            std::sync::Arc::new(crate::events::TestSink::default()),
            std::sync::Arc::new(crate::dispose::RecordingDisposer::default()),
            std::sync::Arc::new(crate::handbrake::AbsentLocator),
            crate::hooks::HookSetup {
                runner: std::sync::Arc::new(crate::hooks::RecordingHookRunner::default()),
                allow_stored_command: false,
            },
        );
        assert!(!ctx.hooks.allow_stored_command);
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p convertbar-core ctx::`
Expected: FAIL — `HookSetup` not found, and `Ctx::new` takes 4 arguments.

- [ ] **Step 3: Implement**

In `hooks.rs`:

```rust
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
```

In `ctx.rs`, add `pub hooks: crate::hooks::HookSetup,` to the struct and a
`hooks: crate::hooks::HookSetup` fifth parameter to `Ctx::new`, assigned into the struct literal.

- [ ] **Step 4: Fix every call site**

The compiler lists them. Production heads:

- `src-tauri/src/lib.rs:108` — `HookSetup { runner: Arc::new(HttpHookRunner), allow_stored_command: true }`
- `src-tauri/src/updater.rs:1867`, `src-tauri/src/commands/updater.rs:68,129` — same as above
- `crates/convertbar-server/src/main.rs:56`, `startup.rs:150`, `routes/mod.rs:282` —
  `HookSetup { runner: Arc::new(HttpHookRunner), allow_stored_command: false }`

Tests, in `converter.rs:1665,1676`, `settings_ops.rs:270`, `control.rs:399,628`,
`watch_ops.rs:239`, `queue_ops.rs:1565`, `watcher.rs:1165`, `handbrake.rs:536` —
`HookSetup { runner: Arc::new(RecordingHookRunner::default()), allow_stored_command: true }`.

**Keep `test_ctx`'s existing 3-tuple return type.** Dozens of tests destructure it, and
widening it would churn every one of them for no benefit. Instead add three helpers next to it
in `converter.rs`'s `mod tests`, mirroring the shape of `test_ctx_with_disposer`. Tasks 5 and 6
use all three by these exact names:

```rust
    /// Like `test_ctx`, but also hands back the recording hook runner so a test can assert on
    /// what was sent. Use this for any test that configures a hook.
    fn test_ctx_hooks(
        conn: Connection,
    ) -> (
        Arc<Ctx>,
        Arc<TestSink>,
        Arc<RecordingDisposer>,
        Arc<crate::hooks::RecordingHookRunner>,
    ) {
        test_ctx_hooks_with_policy(conn, true)
    }

    /// `allow_stored_command` false reproduces the server head's policy, where the
    /// post_convert_command settings ROW must be ignored entirely.
    fn test_ctx_hooks_with_policy(
        conn: Connection,
        allow_stored_command: bool,
    ) -> (
        Arc<Ctx>,
        Arc<TestSink>,
        Arc<RecordingDisposer>,
        Arc<crate::hooks::RecordingHookRunner>,
    ) {
        let sink = Arc::new(TestSink::default());
        let disposer = Arc::new(RecordingDisposer::default());
        let runner = Arc::new(crate::hooks::RecordingHookRunner::default());
        let ctx = Ctx::new(
            conn,
            sink.clone(),
            disposer.clone(),
            Arc::new(crate::handbrake::AbsentLocator),
            crate::hooks::HookSetup {
                runner: runner.clone(),
                allow_stored_command,
            },
        );
        (ctx, sink, disposer, runner)
    }

    /// For tests that need a FailingHookRunner or SlowHookRunner instead of the recorder.
    fn test_ctx_with_hook_runner(
        conn: Connection,
        runner: Arc<dyn crate::hooks::HookRunner>,
    ) -> (Arc<Ctx>, Arc<TestSink>, Arc<RecordingDisposer>) {
        let sink = Arc::new(TestSink::default());
        let disposer = Arc::new(RecordingDisposer::default());
        let ctx = Ctx::new(
            conn,
            sink.clone(),
            disposer.clone(),
            Arc::new(crate::handbrake::AbsentLocator),
            crate::hooks::HookSetup {
                runner,
                allow_stored_command: true,
            },
        );
        (ctx, sink, disposer)
    }

    /// Reads a settings row back — Task 6 asserts on the watermark with this.
    fn setting_value(db: &Arc<Mutex<Connection>>, key: &str) -> String {
        db.lock()
            .unwrap()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_default()
    }
```

Each of these installs `AbsentLocator`; a test that needs the installed world builds its `Ctx`
with `StubLocator` instead. Every existing fixture (`test_ctx`, `test_ctx_with_locator`,
`test_ctx_with_disposer`, and the equivalents in the other five modules) just gains a
`HookSetup { runner: Arc::new(RecordingHookRunner::default()), allow_stored_command: true }`
argument and keeps its current return type.

Note `settings_ops.rs:266` is a doc comment mentioning `Ctx::new`, not a call site — 17 grep
hits, 16 real calls.

- [ ] **Step 5: Run the full suite**

Run: `cargo test --workspace`
Expected: PASS. No test should reach the network — `RecordingHookRunner` performs no I/O.

- [ ] **Step 6: Format and commit**

```bash
cargo fmt
git add -A
git commit -m "feat(hooks): inject HookSetup through Ctx"
```

---

### Task 5: Fire the per-file hook

**Files:**
- Modify: `crates/convertbar-core/src/hooks.rs`
- Modify: `crates/convertbar-core/src/converter.rs` (`ConverterState`, `:1387`, `record_job_error_quiet`, `process_queue` run start)

**Interfaces:**
- Consumes: everything from Tasks 1–4.
- Produces: `hooks::fire_post_convert(ctx: &Ctx, job_id: &str)`, `hooks::load_job_payload`,
  `hooks::read_hook_config`, `hooks::Trigger`, `ConverterState::hook_failure_notified`.

**Background the engineer needs:** `post-convert` has TWO fire points because `process_queue`
books completions and failures through entirely separate paths. `converter.rs:1387` is the
completion booking (`done`/`skipped`). `record_job_error_quiet` (`converter.rs:734`) is the
single choke point for all nine error bookings — one direct call at `:869` and eight through
the `record_job_error` wrapper at `:921, :951, :1029, :1172, :1199, :1309, :1341, :1512`.
Attaching to the wrapper instead would silently miss `:869`. Neither site builds a payload;
both call `fire_post_convert`, which re-reads the freshly booked row.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `converter.rs`:

```rust
    #[test]
    fn done_job_fires_post_convert_once_with_the_result_path() {
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        set_setting(&ctx.db, "post_convert_webhook_url", "http://receiver.invalid/hook");
        // ... queue and run a job that converts smaller; use StubLocator + a fake HandBrake
        // that writes a small file, following the existing successful-conversion tests.
        let sent = hooks.webhooks.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
        assert_eq!(body["event"], "post-convert");
        assert_eq!(body["status"], "done");
        assert_eq!(body["result_path"], body["output_path"]);
    }

    #[test]
    fn skipped_job_fires_with_result_path_equal_to_the_source() {
        // Converted came out larger, so the original was kept and the encode discarded.
        // A receiver told to scan output_path would scan a file that no longer exists.
        // ... arrange a conversion whose output is >= the original ...
        let body: serde_json::Value = /* the single sent body */ unimplemented!();
        assert_eq!(body["status"], "skipped");
        assert_eq!(body["result_path"], body["source_path"]);
    }

    #[test]
    fn error_job_fires_through_a_real_failure_arm() {
        // Driven through an actual failing encode, NOT by calling record_job_error_quiet
        // directly: that is what makes this catch the hook being attached to the
        // record_job_error wrapper and thereby missing the direct call at converter.rs:869.
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        set_setting(&ctx.db, "post_convert_webhook_url", "http://receiver.invalid/hook");
        // ... queue a job whose source vanishes before the encode (the :869 path) ...
        let sent = hooks.webhooks.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
        assert_eq!(body["status"], "error");
        assert!(body["result_path"].is_null());
    }

    #[test]
    fn no_hook_fires_when_nothing_is_configured() {
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        // ... run a successful conversion with no hook settings ...
        assert!(hooks.webhooks.lock().unwrap().is_empty());
        assert!(hooks.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn in_place_keep_deletes_the_row_and_fires_nothing() {
        // The row is deleted rather than booked, so there is no conversion to report.
        // ... arrange cleanup_mode=keep with an in-place job that reached 'encoding' ...
        assert!(hooks.webhooks.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failing_hook_does_not_fail_the_job() {
        let (ctx, _sink, _disp) = test_ctx_with_hook_runner(
            test_conn(),
            std::sync::Arc::new(crate::hooks::FailingHookRunner),
        );
        set_setting(&ctx.db, "post_convert_webhook_url", "http://receiver.invalid/hook");
        // ... run a successful conversion ...
        assert_eq!(job_row(&ctx.db, "j1").0, "done", "a broken receiver is not a failed encode");
    }

    #[test]
    fn hook_failure_notifies_once_per_run_not_once_per_file() {
        // A broken receiver on a 200-file queue must not produce 200 notifications.
        // The flag lives on ConverterState, so this also pins that it resets between runs.
        let (ctx, sink, _disp) = test_ctx_with_hook_runner(
            test_conn(),
            std::sync::Arc::new(crate::hooks::FailingHookRunner),
        );
        set_setting(&ctx.db, "post_convert_webhook_url", "http://receiver.invalid/hook");
        // ... queue THREE jobs that all convert successfully, run the queue ...
        let hook_notes = sink
            .notifications
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, body)| body.contains("hook"))
            .count();
        assert_eq!(hook_notes, 1, "expected exactly one hook-failure notification per run");
    }

    #[test]
    fn shutdown_skips_the_hook_entirely() {
        // is_shutting_down is otherwise only checked at the loop head, so a quit would block
        // the queue thread for up to the timeout and could orphan a command child.
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        set_setting(&ctx.db, "post_convert_webhook_url", "http://receiver.invalid/hook");
        kill_active_child(&ctx.converter); // this is how the shutdown flag is armed
        crate::hooks::fire_post_convert(&ctx, "j1");
        assert!(hooks.webhooks.lock().unwrap().is_empty());
    }

    #[test]
    fn command_hook_receives_unmapped_paths_while_the_webhook_is_mapped() {
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        set_setting(&ctx.db, "post_convert_webhook_url", "http://receiver.invalid/hook");
        set_setting(&ctx.db, "post_convert_command", "/bin/true");
        set_setting(&ctx.db, "hook_path_map", "/media => /data");
        // ... run a successful conversion whose paths live under /media ...
        let webhook_body: serde_json::Value =
            serde_json::from_str(&hooks.webhooks.lock().unwrap()[0].body).unwrap();
        assert!(webhook_body["result_path"].as_str().unwrap().starts_with("/data/"));
        let env = &hooks.commands.lock().unwrap()[0].env;
        let raw = env.iter().find(|(k, _)| k == "CONVERTBAR_RESULT_PATH").unwrap();
        assert!(raw.1.starts_with("/media/"), "commands get raw paths; scripts rewrite them");
    }

    #[test]
    fn server_policy_ignores_a_stored_command_row() {
        // A convertbar.db copied from a desktop install must not make the container execute
        // the desktop user's command.
        let conn = test_conn();
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks_with_policy(conn, false);
        set_setting(&ctx.db, "post_convert_command", "/bin/true");
        // ... run a successful conversion ...
        assert!(hooks.commands.lock().unwrap().is_empty());
    }
```

**Note to the engineer:** the `unimplemented!()` and `// ...` lines above mark where you copy
the arrangement from the nearest existing `process_queue` test — these tests must run a real
queue, not a hand-built row. Every fixture must declare its HandBrake world (`StubLocator` for
the installed world, `AbsentLocator` for CI); the default `PanickingLocator` panics on the queue
thread and poisons `ctx.db`, which surfaces as a confusing `PoisonError` in the test thread.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p convertbar-core converter::tests`
Expected: FAIL — `fire_post_convert` not found, `test_ctx_hooks` not found.

- [ ] **Step 3: Implement the fire path in `hooks.rs`**

```rust
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
    ctx.events.emit_t(
        "hook-failed",
        json!({ "event": event, "reason": reason }),
    );

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
        ctx.events.notify(
            "ConvertBar",
            &format!("{event} hook failed — {reason}"),
        );
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
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?,
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
    // The file that EXISTS now. "original" covers the skipped case and the cleanup-failure
    // case, where the converted file was discarded. Never blindly output_path.
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
```

- [ ] **Step 4: Wire the two fire points in `converter.rs`**

Add two fields to `ConverterState`, both initialised in `new()`:
`pub hook_failure_notified: Mutex<bool>` (false) and
`pub hook_failure_count: std::sync::atomic::AtomicUsize` (zero). It lives here rather than as a `process_queue` local because one fire point is inside
`record_job_error_quiet`, a free function reached from nine places; threading `&mut` state into
it would force signature changes at every one, and the obvious shortcut — hoisting the fire out
to the nine call sites — recreates exactly the drift the single-entry-point design prevents.

At the top of `process_queue`, before the job loop, clear it:

```rust
    *ctx.converter.hook_failure_notified.lock().unwrap() = false;
```

At `converter.rs:1387`, immediately after the `job-status-changed` emit and **outside** the
`ctx.db` block that books the row:

```rust
                crate::hooks::fire_post_convert(ctx, &job.id);
```

In `record_job_error_quiet`, after the two `emit_t` calls and outside the scoped db block:

```rust
    crate::hooks::fire_post_convert(ctx, job_id);
```

Make `converter::is_in_place` visible to `hooks.rs` — it is already `pub(crate)`, so no change.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p convertbar-core`
Expected: PASS.

- [ ] **Step 6: Prove the tests are load-bearing**

On destructive and security-shaped paths this project requires confirming the test actually
fails when the behaviour is removed. Commit first — the restore step below reverts to the last
commit and would otherwise wipe uncommitted work.

```bash
cargo fmt && git add -A && git commit -m "feat(hooks): fire the per-file hook from both booking sites"
```

Then, one at a time, apply the mutation, run the named test, confirm RED, and
`git checkout crates/convertbar-core/src/`:

1. Change `Some("original") => Some(source_path.clone())` to `Some(output_path.clone())` →
   `skipped_job_fires_with_result_path_equal_to_the_source` must FAIL.
2. Move the `fire_post_convert` call from `record_job_error_quiet` into `record_job_error` →
   `error_job_fires_through_a_real_failure_arm` must FAIL (it drives the `:869` path).
3. Remove the `is_shutting_down` guard in `dispatch` → `shutdown_skips_the_hook_entirely` must FAIL.
4. Change `command_env(&payload)` to `command_env(&mapped)` →
   `command_hook_receives_unmapped_paths_while_the_webhook_is_mapped` must FAIL.

A mutation that does not compile reads as SURVIVED — check the build succeeded before believing
a green result, and check `git diff` confirmed the pattern actually matched.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "test(hooks): confirm per-file hook tests fail without the fix"
```

---

### Task 6: Fire the queue-drained hook

**Files:**
- Modify: `crates/convertbar-core/src/hooks.rs`
- Modify: `crates/convertbar-core/src/converter.rs` (`process_queue` breaks and the queue-done block)

**Interfaces:**
- Consumes: Task 5's `dispatch`, `read_hook_config`, `row_to_payload`.
- Produces: `hooks::fire_queue_drained(ctx: &Ctx)`, `hooks::load_jobs_since`.

**Background the engineer needs:** the existing queue-done block at `converter.rs:1524` is NOT a
drain signal. Two `break`s reach it: `get_next_job` returning `None` (a true drain) and
`take_pause_after_current` at `:1457` breaking at `:1470` — which is "pause after this job" AND
**every pause on Windows**, because `pause_conversion` falls back to `pause_after_current` when
`can_pause_process()` is false (`control.rs:46-53`). Two other paths (low-disk pause, shutdown)
`return` and never reach the block at all. So the hook must be gated on a true drain, and the
job set must come from a persisted watermark rather than an in-memory accumulator that a pause
or a restart would discard.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `converter.rs`:

```rust
    #[test]
    fn a_true_drain_fires_queue_drained_and_advances_the_watermark() {
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        set_setting(&ctx.db, "queue_drained_webhook_url", "http://receiver.invalid/hook");
        // ... queue two successful jobs, run the queue to completion ...
        let sent = hooks.webhooks.lock().unwrap();
        assert_eq!(sent.len(), 1, "exactly one drain payload per run");
        let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
        assert_eq!(body["event"], "queue-drained");
        assert_eq!(body["completed"], 2);
        assert_eq!(body["jobs"].as_array().unwrap().len(), 2);
        assert!(!setting_value(&ctx.db, "last_queue_drained_at").is_empty());
    }

    #[test]
    fn pause_after_current_fires_no_queue_drained() {
        // This is the Windows pause path (control.rs:46-53), so without the drained gate every
        // pause on Windows would emit a spurious drain mid-run. Drive the flag, not the platform.
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        set_setting(&ctx.db, "queue_drained_webhook_url", "http://receiver.invalid/hook");
        // ... queue TWO jobs, arm pause_after_current, run the queue ...
        assert!(
            hooks.webhooks.lock().unwrap().is_empty(),
            "a pause is not a drain — a job is still queued"
        );
    }

    #[test]
    fn jobs_completed_before_a_pause_appear_in_the_drain_that_follows() {
        // The regression test for the in-memory accumulator this design rejects: a run-local
        // Vec would lose everything completed before the pause.
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        set_setting(&ctx.db, "queue_drained_webhook_url", "http://receiver.invalid/hook");
        // ... run job 1, arm pause_after_current so the queue stops; then queue job 2 and run
        // again to a true drain ...
        let body: serde_json::Value =
            serde_json::from_str(&hooks.webhooks.lock().unwrap()[0].body).unwrap();
        assert_eq!(body["jobs"].as_array().unwrap().len(), 2, "both runs' jobs");
    }

    #[test]
    fn a_drain_with_nothing_new_fires_nothing() {
        let (ctx, _sink, _disp, hooks) = test_ctx_hooks(test_conn());
        set_setting(&ctx.db, "queue_drained_webhook_url", "http://receiver.invalid/hook");
        // ... run to a drain once, clear the recorder, then run process_queue again on an
        // empty queue ...
        assert!(hooks.webhooks.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_drain_hook_does_not_advance_the_watermark() {
        // Re-reporting is harmless for a rescan; silent loss is not.
        let (ctx, _sink, _disp) = test_ctx_with_hook_runner(
            test_conn(),
            std::sync::Arc::new(crate::hooks::FailingHookRunner),
        );
        set_setting(&ctx.db, "queue_drained_webhook_url", "http://receiver.invalid/hook");
        // ... run one successful job to a drain ...
        assert_eq!(setting_value(&ctx.db, "last_queue_drained_at"), "");
    }

    #[test]
    fn drain_output_dirs_exclude_error_jobs_and_dedupe() {
        // ... run one successful job and one failing job in the same directory ...
        let body: serde_json::Value = /* the sent body */ unimplemented!();
        assert_eq!(body["output_dirs"].as_array().unwrap().len(), 1);
        assert_eq!(body["errors"], 1);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p convertbar-core converter::tests`
Expected: FAIL — `fire_queue_drained` not found.

- [ ] **Step 3: Implement in `hooks.rs`**

```rust
const WATERMARK_KEY: &str = "last_queue_drained_at";

/// Jobs completed since the watermark, oldest first. Reading from the table rather than an
/// in-memory accumulator is what lets the payload survive a pause, a low-disk stop, or a
/// restart — a run-local Vec would silently drop everything completed before the interruption.
pub fn load_jobs_since(db: &rusqlite::Connection, watermark: &str) -> Vec<JobPayload> {
    let mut stmt = match db.prepare(
        "SELECT id, status, source_path, output_path, preset, kept_file, original_size, \
         converted_size, space_saved, error_message, failure_class, started_at, completed_at \
         FROM jobs WHERE completed_at IS NOT NULL AND completed_at > ?1 \
         ORDER BY completed_at ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map(rusqlite::params![watermark], |r| {
        Ok(row_to_payload(
            r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
            r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?,
        ))
    });
    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Fires once per TRUE drain. Advances the watermark only on success, so a failed hook
/// re-reports the same jobs next time rather than losing them.
pub fn fire_queue_drained(ctx: &crate::ctx::Ctx) {
    let (cfg, jobs) = {
        let db = match ctx.db.lock() {
            Ok(db) => db,
            Err(_) => return,
        };
        let cfg = read_hook_config(&db, Trigger::QueueDrained, ctx.hooks.allow_stored_command);
        if cfg.is_off() {
            return;
        }
        // An absent watermark sorts before every RFC3339 timestamp, so a first run reports
        // every completed job in History. That one-time burst is preferred over seeding the
        // watermark at migration time, which would make a fresh install and an upgrade behave
        // differently for no visible reason.
        let watermark = setting(&db, WATERMARK_KEY);
        let jobs = load_jobs_since(&db, &watermark);
        (cfg, jobs)
    }; // guard dropped before dispatch

    if jobs.is_empty() {
        return; // nothing new — an idle queue emits no empty payloads
    }
    let newest = jobs
        .iter()
        .filter_map(|j| j.completed_at.clone())
        .max();

    let failures_before = hook_failures_seen(ctx);
    dispatch(ctx, Trigger::QueueDrained, &cfg, queue_drained_payload(&jobs));
    if hook_failures_seen(ctx) != failures_before {
        return; // do not advance past jobs the receiver never heard about
    }

    if let (Some(newest), Ok(db)) = (newest, ctx.db.lock()) {
        let _ = db.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![WATERMARK_KEY, newest],
        );
    }
}
```

`hook_failures_seen` reads the counter `report_failure` already increments (added to
`ConverterState` in Task 5), so the watermark can tell whether *this* dispatch failed:

```rust
fn hook_failures_seen(ctx: &crate::ctx::Ctx) -> usize {
    ctx.converter
        .hook_failure_count
        .load(std::sync::atomic::Ordering::SeqCst)
}
```

- [ ] **Step 4: Gate the fire on a true drain in `converter.rs`**

Declare `let mut drained = false;` before the job loop. Set `drained = true` on the break taken
when `get_next_job` returns `None`. Leave it false on the `take_pause_after_current` break at
`:1470`. Then, in the queue-done block after the existing notification:

```rust
    if drained {
        crate::hooks::fire_queue_drained(ctx);
    }
```

Place it after the `ctx.db` guard used for `notifications_queue_done` has been dropped.

- [ ] **Step 5: Run the tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Prove the drain gate is load-bearing**

Commit first, then mutate and confirm RED, restoring after each:

1. Remove the `if drained` gate → `pause_after_current_fires_no_queue_drained` must FAIL.
2. Advance the watermark unconditionally → `a_failed_drain_hook_does_not_advance_the_watermark`
   must FAIL.

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add -A
git commit -m "feat(hooks): fire queue-drained on a true drain, driven by a watermark"
```

---

### Task 7: Desktop-only command hook commands

**Files:**
- Create: `src-tauri/src/commands/hooks.rs`
- Modify: `src-tauri/src/commands/mod.rs`, `src-tauri/src/lib.rs` (the `invoke_handler` list)

**Interfaces:**
- Consumes: `Ctx` (Task 4).
- Produces: `get_command_hooks() -> CommandHooks`, `set_command_hook(trigger: String, command: String)`.

**Background the engineer needs:** the command keys are deliberately absent from `ALLOWED_KEYS`,
so `update_setting` refuses them and the server's HTTP API cannot reach them. The desktop head
writes them through app-defined `#[tauri::command]` functions instead, which are local-only and
ACL-exempt — they need **no** entry in `src-tauri/capabilities/default.json` (CLAUDE.md,
"Permissions (ACL)"). Do not "fix" the refusal by adding the keys to `ALLOWED_KEYS`.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/commands/hooks.rs` (module created in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_round_trip_both_triggers() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        convertbar_core::db::init_db(&conn).unwrap();
        write_command_hook(&conn, "post_convert", "/a.sh").unwrap();
        write_command_hook(&conn, "queue_drained", "/b.sh").unwrap();
        let hooks = read_command_hooks(&conn);
        assert_eq!(hooks.post_convert, "/a.sh");
        assert_eq!(hooks.queue_drained, "/b.sh");
    }

    #[test]
    fn an_unknown_trigger_is_rejected() {
        // The trigger name selects the settings key; an unchecked name would let a caller
        // write ANY settings row through a command that bypasses ALLOWED_KEYS.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        convertbar_core::db::init_db(&conn).unwrap();
        assert!(write_command_hook(&conn, "preset", "Fast 1080p30").is_err());
        assert!(write_command_hook(&conn, "../../etc", "x").is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p convertbar --lib hooks::`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement**

```rust
//! Desktop-only access to the command hooks. These settings keys are absent from
//! `settings_ops::ALLOWED_KEYS` on purpose — that absence is what stops the server head's
//! HTTP API from turning ConvertBar into a remote shell. The desktop head is a local,
//! already-trusted context, so it reads and writes them here instead. App-defined
//! `#[tauri::command]`s are ACL-exempt and need no capabilities entry.

use serde::Serialize;

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CommandHooks {
    pub post_convert: String,
    pub queue_drained: String,
}

/// Maps a trigger name to its settings key. Anything else is rejected: without this the
/// command would be an unrestricted settings writer that bypasses ALLOWED_KEYS entirely.
fn key_for(trigger: &str) -> Result<&'static str, String> {
    match trigger {
        "post_convert" => Ok("post_convert_command"),
        "queue_drained" => Ok("queue_drained_command"),
        other => Err(format!("unknown hook trigger: {other}")),
    }
}

pub fn read_command_hooks(conn: &rusqlite::Connection) -> CommandHooks {
    let get = |key: &str| {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default()
    };
    CommandHooks {
        post_convert: get("post_convert_command"),
        queue_drained: get("queue_drained_command"),
    }
}

pub fn write_command_hook(
    conn: &rusqlite::Connection,
    trigger: &str,
    command: &str,
) -> Result<(), String> {
    let key = key_for(trigger)?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, command],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}
```

Add the two thin `#[tauri::command]` adapters that lock `ctx.db` and delegate, following the
shape of the adapters already in `commands/settings.rs`. Register both in the `invoke_handler!`
list in `src-tauri/src/lib.rs`, and add `pub mod hooks;` to `commands/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add -A
git commit -m "feat(hooks): desktop-only commands for the command hook"
```

---

### Task 8: Settings UI

**Files:**
- Modify: `src/pages/SettingsPage.tsx`
- Modify: `src/pages/SettingsPage.test.tsx`
- Modify: `src/hooks/useSettings.ts` if the settings type is declared there

**Interfaces:**
- Consumes: the eight settings fields from Task 3; `get_command_hooks` / `set_command_hook`
  from Task 7.
- Produces: no new exports.

**Background the engineer needs:** `isServerHead` from `src/lib/head.ts` distinguishes the two
builds and is already used this way in `TabBar.tsx`. The command-hook field must **not render
at all** on the server head — not disabled, not read-only. The server cannot serve its value, so
a field that always renders empty invites a bug report. Note that RTL's `render()` does not wrap
in `StrictMode`.

- [ ] **Step 1: Write the failing tests**

Add to `src/pages/SettingsPage.test.tsx`:

```tsx
it("renders the webhook fields for both trigger points", () => {
  render(<SettingsPage {...propsWith({ postConvertWebhookUrl: "http://a/", queueDrainedWebhookUrl: "http://b/" })} />);
  expect(screen.getByLabelText(/after each conversion.*url/i)).toHaveValue("http://a/");
  expect(screen.getByLabelText(/when the queue finishes.*url/i)).toHaveValue("http://b/");
});

it("shows the command hook field on the desktop head", () => {
  render(<SettingsPage {...propsWith({})} />);
  expect(screen.getByLabelText(/command to run/i)).toBeInTheDocument();
});

it("does not render the command hook field on the server head", async () => {
  // Not disabled, not read-only — absent. The server head cannot serve the value, so a field
  // that always renders empty would read as a bug.
  vi.doMock("../lib/head", () => ({ isServerHead: true }));
  vi.resetModules();
  const { default: ServerSettingsPage } = await import("./SettingsPage");
  render(<ServerSettingsPage {...propsWith({})} />);
  expect(screen.queryByLabelText(/command to run/i)).not.toBeInTheDocument();
  expect(screen.getByText(/set by environment variable/i)).toBeInTheDocument();
});

it("warns that path mapping does not apply to the command hook", () => {
  render(<SettingsPage {...propsWith({})} />);
  expect(screen.getByText(/applies to webhooks only/i)).toBeInTheDocument();
});

it("saves the timeout setting on change", async () => {
  const onUpdate = vi.fn();
  render(<SettingsPage {...propsWith({})} onUpdateSetting={onUpdate} />);
  await userEvent.clear(screen.getByLabelText(/timeout/i));
  await userEvent.type(screen.getByLabelText(/timeout/i), "60");
  expect(onUpdate).toHaveBeenCalledWith("hook_timeout_seconds", "60");
});
```

Match `propsWith` and the update-callback name to whatever `SettingsPage.test.tsx` already uses
— copy the arrangement from the nearest existing settings test rather than inventing one.

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/pages/SettingsPage.test.tsx`
Expected: FAIL — the fields do not exist.

- [ ] **Step 3: Implement**

Add a "Hooks" section to `SettingsPage.tsx` following the existing section markup exactly. It
contains, in order:

1. **After each conversion** — URL input, headers textarea (placeholder
   `ApiKey: your-key`), body textarea (placeholder showing the empty-body behaviour).
2. **When the queue finishes** — the same three fields, bound to the `queue_drained_*` keys.
3. **Path mapping** — textarea, placeholder `/media => /data`, with help text reading
   "One rule per line. Applies to webhooks only — a command hook receives raw paths."
4. **Timeout** — number input bound to `hook_timeout_seconds`, help text "Per hook. With both a
   webhook and a command configured, a dead receiver costs twice this per job."
5. **Command to run** — rendered only when `!isServerHead`, with a file picker matching the
   existing HandBrake-path picker, wired to `set_command_hook`. When `isServerHead`, render the
   note "Set by environment variable on the server head
   (`CONVERTBAR_POST_CONVERT_COMMAND`)." instead.

Every input needs a real `<label htmlFor>` so the tests above can find it by accessible name.

- [ ] **Step 4: Run the tests**

Run: `npx vitest run` and `npx tsc`
Expected: PASS.

- [ ] **Step 5: Prove one UI test is load-bearing**

Swap the `!isServerHead` condition to `isServerHead` and confirm
`does not render the command hook field on the server head` goes RED. Restore.

- [ ] **Step 6: Verify the real layout**

jsdom does no layout, so a textarea that overflows its panel passes every test above. Drive the
server head with Playwright from a scratch project outside the repo and assert the Hooks section
does not overflow: `scrollWidth <= clientWidth` on the settings panel.

**Before pointing any head at a real database, copy it and disable watchers, and force
`cleanup_mode=keep`** — a head started against a live-looking copy auto-registers watched folders
and can trash real files. Name the copy `convertbar.db`.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(hooks): add the Hooks section to Settings"
```

---

### Task 9: Documentation

**Files:**
- Modify: `README.md`, `docker-compose.example.yml`, `unraid-template.xml`, `CLAUDE.md`

**Interfaces:**
- Consumes: everything.
- Produces: nothing.

- [ ] **Step 1: README**

Add a "Post-convert hooks" section covering: the two trigger points; the webhook fields; the
templating rules including the JSON-escaping guarantee and the `_json` suffix; path mapping and
its webhook-only scope; the command hook and its environment variables; and this worked Stash
example, which is the driving use case:

````markdown
### Example: make Stash rescan after a batch

Set the **queue-drained** webhook to:

| Field | Value |
|---|---|
| URL | `http://stash:9999/graphql` |
| Headers | `ApiKey: your-stash-api-key` |
| Body | `{"query":"mutation { metadataScan(input: {paths: {{output_dirs_json}}}) }"}` |

If Stash mounts the same media at a different path, add a path-map rule — `/media => /data`.
````

State plainly that webhook headers are stored in plaintext in `convertbar.db` and readable by
any authenticated web-UI user, and that an authenticated user can aim the webhook at any address
the container can reach. Both are the same trust class as the auth token itself.

Note the first-drain behaviour: on the first drain after upgrading, `queue-drained` reports every
completed job already in History, because there is no watermark yet.

- [ ] **Step 2: docker-compose.example.yml**

Add, commented out alongside the existing optional variables:

```yaml
      # CONVERTBAR_POST_CONVERT_COMMAND: "/config/hooks/post-convert.sh"
      # CONVERTBAR_QUEUE_DRAINED_COMMAND: "/config/hooks/queue-drained.sh"
```

And a commented volume line: `# - ./hooks:/config/hooks   # scripts for the command hooks`.

Add a note that the image ships `bash` but **not** `curl`; a script can still reach the host over
raw TCP (`exec 3<>/dev/tcp/host/port`), or you can bake `curl` into a custom image.

- [ ] **Step 3: unraid-template.xml**

Add both command variables as optional `<Config>` entries with `Mode=""` and a description
matching the README wording, following the existing variable entries' shape.

- [ ] **Step 4: CLAUDE.md**

Add a short "Post-Convert Hooks" section recording the three invariants a future change is most
likely to break:

- `post-convert` has two fire points; the error one must stay on `record_job_error_quiet`, not
  the `record_job_error` wrapper, or the direct call at `converter.rs:869` is missed.
- `queue-drained` fires only on a true drain. The queue-done block is also reached by
  `pause_after_current`, which is every pause on Windows.
- `post_convert_command` / `queue_drained_command` are absent from `ALLOWED_KEYS` and from the
  `Settings` struct on purpose. That pair of absences is the entire boundary keeping the server
  head's HTTP API from being a remote shell.

- [ ] **Step 5: Verify and commit**

Run: `cargo test --workspace && npx vitest run && npx tsc`
Expected: PASS.

```bash
git add -A
git commit -m "docs(hooks): document post-convert hooks, Stash example and invariants"
```

---

## Done Criteria

- `cargo test --workspace`, `npx vitest run`, and `npx tsc` all pass.
- A webhook configured against a local receiver fires on conversion and on drain.
- No test performs real network I/O — every fixture uses a `RecordingHookRunner` or a sibling double.
- The four Task 5 and two Task 6 mutations each turned a named test red.
