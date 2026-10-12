//! All ssh I/O for remote roots (ADRs 0005 and 0006). Nothing else in the crate builds a remote
//! command: the queue thread, the routes, cancel and recovery call these functions.
//!
//! Every remote script travels on stdin to `sh -s`, its arguments single-quote-escaped after it,
//! and is wrapped in a function called only on its last line, so a stream cut off part-way runs
//! nothing. The push is the one exception: its stdin is the file, so its fixed one-line script is
//! the remote command itself. Each remote command starts with `: convertbar-<step>;`, a no-op that
//! names the step for the test shim and for anyone reading the host's process list.
//!
//! ssh's own exit status 255 means the host could not be reached (refused, timed out, failed
//! authentication or host key). The scripts never exit 255 themselves.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One `host:dir` entry of `CONVERTBAR_REMOTE_ROOTS`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RemoteRoot {
    pub host: String,
    pub dir: String,
}

/// The server head's remote configuration, set once between construction and boot.
#[derive(Debug, Clone)]
pub struct RemoteSetup {
    pub roots: Vec<RemoteRoot>,
    /// Local staging directory. Holds at most one job's source and result.
    pub staging: PathBuf,
    /// The ssh program: the system `ssh`, or the test shim.
    pub ssh: PathBuf,
}

impl RemoteSetup {
    pub fn knows_host(&self, host: &str) -> bool {
        self.roots.iter().any(|r| r.host == host)
    }

    fn roots_of<'a>(&'a self, host: &'a str) -> impl Iterator<Item = &'a RemoteRoot> + 'a {
        self.roots.iter().filter(move |r| r.host == host)
    }

    /// Where a job's source is pulled to.
    pub fn staged_source(&self, source_path: &str) -> PathBuf {
        self.staging.join("source").join(basename(source_path))
    }

    /// Where a job's result is encoded to. A separate directory from the staged source: an
    /// in-place job's result has the source's own name.
    pub fn staged_result(&self, output_path: &str) -> PathBuf {
        self.staging.join("result").join(basename(output_path))
    }

    /// Empties staging. Best-effort. Only the two subdirectories ConvertBar writes are removed:
    /// `CONVERTBAR_STAGING_DIR` may name a directory that holds anything else.
    pub fn empty_staging(&self) {
        for sub in ["source", "result"] {
            let _ = std::fs::remove_dir_all(self.staging.join(sub));
        }
    }
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string())
}

/// A host alias that can never be read as an ssh option: `[A-Za-z0-9._-]+`, not starting with `-`.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Parses `CONVERTBAR_REMOTE_ROOTS`: comma-separated `host:dir`. The error names the entry.
pub fn parse_roots(raw: &str) -> Result<Vec<RemoteRoot>, String> {
    let mut roots = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let (host, dir) = entry
            .split_once(':')
            .ok_or_else(|| format!("{entry} (expected host:/absolute/dir)"))?;
        if !valid_host(host) {
            return Err(format!(
                "{entry} (the host must be an ssh alias of letters, digits, '.', '_' or '-', \
                 not starting with '-')"
            ));
        }
        if !dir.starts_with('/') {
            return Err(format!("{entry} (the directory must be absolute)"));
        }
        roots.push(RemoteRoot {
            host: host.to_string(),
            dir: dir.to_string(),
        });
    }
    Ok(roots)
}

/// Single-quote-escapes one argument for the remote shell.
fn quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// Why a remote step did not do what it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    /// ssh exited 255: the host could not be reached.
    Unreachable { host: String, step: &'static str },
    /// Anything else, with a message for the job's history entry.
    Failed(String),
}

impl RemoteError {
    pub fn message(&self) -> String {
        match self {
            RemoteError::Unreachable { host, step } => format!("{host} is unreachable ({step})"),
            RemoteError::Failed(m) => m.clone(),
        }
    }
}

const SSH_UNREACHABLE: i32 = 255;

fn ssh(setup: &RemoteSetup, host: &str, remote_command: &str) -> Command {
    let mut cmd = Command::new(&setup.ssh);
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=20",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=4",
        "--",
        host,
        remote_command,
    ]);
    cmd
}

fn script_command(step: &str, args: &[&str]) -> String {
    let mut command = format!(": convertbar-{step}; sh -s --");
    for arg in args {
        command.push(' ');
        command.push_str(&quote(arg));
    }
    command
}

/// The output of a finished remote script.
struct ScriptOutput {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

/// Runs `script` on `host` and waits. Only exit 255 is turned into an error here; every other
/// status is the caller's to interpret.
fn run_script(
    setup: &RemoteSetup,
    host: &str,
    step: &'static str,
    script: &str,
    args: &[&str],
) -> Result<ScriptOutput, RemoteError> {
    let mut child = ssh(setup, host, &script_command(step, args))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RemoteError::Failed(format!("Could not run ssh: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A host that refuses the connection closes the pipe early; the exit status says why.
        let _ = stdin.write_all(script.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|e| RemoteError::Failed(format!("ssh to {host} failed: {e}")))?;
    let code = output.status.code();
    if code == Some(SSH_UNREACHABLE) {
        return Err(RemoteError::Unreachable {
            host: host.to_string(),
            step,
        });
    }
    Ok(ScriptOutput {
        code,
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Splits a NUL-delimited answer into fields.
fn nul_fields(bytes: &[u8]) -> Vec<String> {
    let mut fields: Vec<String> = bytes
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    // The answer ends with a NUL, which leaves one empty trailing piece.
    if fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }
    fields
}

/// `secs.mmm` from `stat -c %.3Y`, as milliseconds — the unit local fingerprints use.
fn parse_mtime_ms(s: &str) -> Option<i64> {
    let (secs, frac) = s.split_once('.')?;
    let secs: i64 = secs.parse().ok()?;
    let millis: i64 = format!("{frac:0<3}")[..3].parse().ok()?;
    Some(secs * 1000 + millis)
}

/// Size and mtime of a remote file, as a fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteStat {
    pub size: i64,
    pub mtime_ms: i64,
}

/// Parses `"<size> <secs.mmm>[ <type>]"`. Returns the stat and the file type, if printed.
fn parse_stat_line(line: &str) -> Option<(RemoteStat, String)> {
    let mut parts = line.splitn(3, ' ');
    let size = parts.next()?.trim().parse().ok()?;
    let mtime_ms = parse_mtime_ms(parts.next()?.trim())?;
    let kind = parts.next().unwrap_or("").trim().to_string();
    Some((RemoteStat { size, mtime_ms }, kind))
}

/// Whether `path` equals or lies under `dir`, compared by path components.
fn is_within(path: &str, dir: &str) -> bool {
    Path::new(path).starts_with(Path::new(dir))
}

/// Lexical containment against the configured roots, before anything runs on the host.
pub fn check_named_path(setup: &RemoteSetup, host: &str, path: &str) -> Result<(), String> {
    if !setup.knows_host(host) {
        return Err(format!("{host} is not a configured remote host"));
    }
    if !path.starts_with('/') {
        return Err(format!("{path} is not an absolute path on {host}"));
    }
    // `starts_with` compares components without collapsing `..`, so a path holding one could
    // name anything; the host is never asked about it.
    let dotted = Path::new(path).components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    });
    if dotted || !setup.roots_of(host).any(|r| is_within(path, &r.dir)) {
        return Err(format!("{path} is outside the remote roots of {host}"));
    }
    Ok(())
}

/// What the host says about one named path at add time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspected {
    pub path: String,
    /// `None` when the host could not resolve it (missing, or unreadable on the way).
    pub stat: Option<RemoteStat>,
    pub is_file: bool,
}

const INSPECT_SCRIPT: &str = r#"inspect() {
  for p in "$@"; do
    realpath -e -z -- "$p" 2>/dev/null || printf '\0'
    printf '%s\0' "$(stat -c '%s %.3Y %F' -- "$p" 2>/dev/null)"
  done
}
inspect "$@"
"#;

/// Resolves the named paths and the host's root dirs in one round trip, refusing any path that
/// is outside every resolved root of `host` or whose resolved form differs from the path as named
/// (a symlink anywhere along it — finalize must never rename over a link). Missing paths are
/// returned without a stat; the caller's skip rules decide what they mean.
pub fn inspect_paths(
    setup: &RemoteSetup,
    host: &str,
    paths: &[String],
) -> Result<Vec<Inspected>, RemoteError> {
    for p in paths {
        check_named_path(setup, host, p).map_err(RemoteError::Failed)?;
    }
    let root_dirs: Vec<&str> = setup.roots_of(host).map(|r| r.dir.as_str()).collect();
    let mut args: Vec<&str> = root_dirs.clone();
    args.extend(paths.iter().map(String::as_str));
    let out = run_script(setup, host, "inspect", INSPECT_SCRIPT, &args)?;
    if out.code != Some(0) {
        return Err(RemoteError::Failed(format!(
            "Could not inspect paths on {host}: {}",
            out.stderr
        )));
    }
    let fields = nul_fields(&out.stdout);
    if fields.len() != args.len() * 2 {
        return Err(RemoteError::Failed(format!(
            "Unexpected answer from {host} while inspecting paths"
        )));
    }
    // (named, resolved) per root that resolved. A root may itself be a symlink (a share reached
    // through one); only what lies below it must resolve to exactly the path as named.
    let roots: Vec<(&str, &str)> = root_dirs
        .iter()
        .enumerate()
        .map(|(i, named)| (*named, fields[i * 2].as_str()))
        .filter(|(_, real)| !real.is_empty())
        .collect();
    let mut inspected = Vec::with_capacity(paths.len());
    for (i, path) in paths.iter().enumerate() {
        let resolved = &fields[(root_dirs.len() + i) * 2];
        let stat_line = &fields[(root_dirs.len() + i) * 2 + 1];
        if resolved.is_empty() {
            inspected.push(Inspected {
                path: path.clone(),
                stat: None,
                is_file: false,
            });
            continue;
        }
        let resolves_as_named = roots.iter().any(|(named, real)| {
            Path::new(path)
                .strip_prefix(named)
                .is_ok_and(|rest| Path::new(real).join(rest) == Path::new(resolved))
        });
        if !resolves_as_named {
            return Err(RemoteError::Failed(
                if roots.iter().any(|(_, real)| is_within(resolved, real)) {
                    format!(
                        "{path} on {host} is or passes through a symlink; add the real path \
                         instead"
                    )
                } else {
                    format!("{path} is outside the remote roots of {host}")
                },
            ));
        }
        let (stat, kind) = match parse_stat_line(stat_line) {
            Some((s, k)) => (Some(s), k),
            None => (None, String::new()),
        };
        inspected.push(Inspected {
            path: path.clone(),
            stat,
            is_file: kind == "regular file" || kind == "regular empty file",
        });
    }
    Ok(inspected)
}

const NAMES_SCRIPT: &str = r#"names() {
  for d in "$@"; do
    printf '%s\0' "$d"
    find -H "$d" -mindepth 1 -maxdepth 1 -printf '%f\0' 2>/dev/null
    printf '\0'
  done
}
names "$@"
"#;

/// The names present in each of `dirs` on the host, in one round trip.
pub fn list_names(
    setup: &RemoteSetup,
    host: &str,
    dirs: &[String],
) -> Result<std::collections::HashMap<String, std::collections::HashSet<String>>, RemoteError> {
    let args: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let out = run_script(setup, host, "names", NAMES_SCRIPT, &args)?;
    if out.code != Some(0) {
        return Err(RemoteError::Failed(format!(
            "Could not list directories on {host}: {}",
            out.stderr
        )));
    }
    let mut result = std::collections::HashMap::new();
    let mut fields = nul_fields(&out.stdout).into_iter();
    while let Some(dir) = fields.next() {
        let mut names = std::collections::HashSet::new();
        // A name is never empty, so an empty field ends this directory's list.
        for name in fields.by_ref() {
            if name.is_empty() {
                break;
            }
            names.insert(name);
        }
        result.insert(dir, names);
    }
    Ok(result)
}

/// What the host says about a job's source just before the pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceState {
    Present(RemoteStat),
    /// Definitely gone: its parent exists and is readable, and the name is not in it.
    Missing,
    /// Anything else: the host could not look (permissions, a vanished parent, not a file).
    Unknown(String),
}

const PROBE_SCRIPT: &str = r#"probe() {
  p=$1
  if [ -e "$p" ] || [ -L "$p" ]; then
    s=$(stat -c '%s %.3Y %F' -- "$p") || exit 11
    printf '%s\n' "$s"
    exit 0
  fi
  d=$(dirname -- "$p")
  if [ -d "$d" ] && [ -r "$d" ] && [ -x "$d" ]; then
    exit 10
  fi
  echo "cannot look into $d" >&2
  exit 11
}
probe "$@"
"#;

pub fn probe_source(
    setup: &RemoteSetup,
    host: &str,
    path: &str,
    step: &'static str,
) -> Result<SourceState, RemoteError> {
    let out = run_script(setup, host, step, PROBE_SCRIPT, &[path])?;
    Ok(match out.code {
        Some(0) => {
            let line = String::from_utf8_lossy(&out.stdout);
            match parse_stat_line(line.trim_end_matches('\n')) {
                Some((stat, kind)) if kind == "regular file" || kind == "regular empty file" => {
                    SourceState::Present(stat)
                }
                Some((_, kind)) => SourceState::Unknown(format!("{path} on {host} is a {kind}")),
                None => SourceState::Unknown(format!("Unexpected stat answer from {host}")),
            }
        }
        Some(10) => SourceState::Missing,
        _ => SourceState::Unknown(if out.stderr.is_empty() {
            format!("Could not look at {path} on {host}")
        } else {
            format!("Could not look at {path} on {host}: {}", out.stderr)
        }),
    })
}

const PULL_SCRIPT: &str = r#"pull() {
  exec cat -- "$1"
}
pull "$@"
"#;

/// A started transfer's step and host, kept to interpret its exit status once the child — which
/// runs as the job's current child, so Pause can stop it and Cancel can kill it — is done.
pub struct Transfer {
    step: &'static str,
    host: String,
}

impl Transfer {
    /// Maps the transfer's exit status. `Ok(None)` means a cancel or a shutdown killed it, as
    /// `killed_by_us` says, and the caller must not book. Any other death is a failure: left
    /// unbooked, the row would stay `encoding` until the next restart.
    pub fn finish(
        &self,
        status: std::io::Result<std::process::ExitStatus>,
        stderr: &str,
        killed_by_us: bool,
    ) -> Result<Option<()>, RemoteError> {
        let what = if self.step == "pull" {
            "Download from"
        } else {
            "Upload to"
        };
        match status {
            Ok(s) if s.success() => Ok(Some(())),
            Ok(s) if s.code() == Some(SSH_UNREACHABLE) => Err(RemoteError::Unreachable {
                host: self.host.clone(),
                step: self.step,
            }),
            Ok(s) if s.code().is_none() && killed_by_us => Ok(None),
            Err(_) if killed_by_us => Ok(None),
            Ok(s) if s.code().is_none() => Err(RemoteError::Failed(format!(
                "{what} {} was interrupted ({s})",
                self.host
            ))),
            Err(e) => Err(RemoteError::Failed(format!(
                "{what} {} was lost: {e}",
                self.host
            ))),
            Ok(_) => Err(RemoteError::Failed(format!(
                "{what} {} failed: {}",
                self.host,
                stderr.trim()
            ))),
        }
    }
}

/// Starts streaming `path` from `host` into `dest`.
pub fn start_pull(
    setup: &RemoteSetup,
    host: &str,
    path: &str,
    dest: &Path,
) -> Result<(std::process::Child, Transfer), RemoteError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| RemoteError::Failed(format!("Could not prepare staging: {e}")))?;
    }
    let file = std::fs::File::create(dest)
        .map_err(|e| RemoteError::Failed(format!("Could not write to staging: {e}")))?;
    let mut child = ssh(setup, host, &script_command("pull", &[path]))
        .stdin(Stdio::piped())
        .stdout(Stdio::from(file))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RemoteError::Failed(format!("Could not run ssh: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(PULL_SCRIPT.as_bytes());
    }
    Ok((
        child,
        Transfer {
            step: "pull",
            host: host.to_string(),
        },
    ))
}

/// The marked temp a result is pushed to, beside its target: `.{stem}.convertbar-tmp.{ext}`.
pub fn remote_temp_path(output_path: &str) -> String {
    crate::converter::in_place_temp_path(output_path)
        .to_string_lossy()
        .into_owned()
}

/// Starts streaming the local `result` into `remote_temp` on `host`, overwriting any leftover.
pub fn start_push(
    setup: &RemoteSetup,
    host: &str,
    result: &Path,
    remote_temp: &str,
) -> Result<(std::process::Child, Transfer), RemoteError> {
    let file = std::fs::File::open(result)
        .map_err(|e| RemoteError::Failed(format!("Could not read the staged result: {e}")))?;
    let command = format!(": convertbar-push; cat > {}", quote(remote_temp));
    let child = ssh(setup, host, &command)
        .stdin(Stdio::from(file))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RemoteError::Failed(format!("Could not run ssh: {e}")))?;
    Ok((
        child,
        Transfer {
            step: "push",
            host: host.to_string(),
        },
    ))
}

/// SHA-256 of a local file, lower-case hex, as `sha256sum` prints it.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Everything finalize needs about the job.
pub struct FinalizeRequest<'a> {
    pub source_path: &'a str,
    pub output_path: &'a str,
    pub remote_temp: &'a str,
    pub result_size: i64,
    pub result_sha256: &'a str,
    pub baseline: RemoteStat,
    pub delete_original: bool,
}

const FINALIZE_SCRIPT: &str = r#"finalize() {
  orig=$1; tmp=$2; target=$3; want_size=$4; want_sha=$5; base_size=$6; base_mtime=$7; delete_original=$8
  cur=$(stat -c '%s %.3Y' -- "$orig") || exit 20
  cur_size=${cur% *}
  cur_m=${cur#* }
  cur_mtime=${cur_m%.*}${cur_m#*.}
  if [ "$cur_size" != "$base_size" ] || [ "$cur_mtime" != "$base_mtime" ]; then
    exit 21
  fi
  got_size=$(stat -c '%s' -- "$tmp") || exit 22
  got_sha=$(sha256sum < "$tmp") || exit 22
  got_sha=${got_sha%% *}
  if [ "$got_size" != "$want_size" ] || [ "$got_sha" != "$want_sha" ]; then
    rm -f -- "$tmp"
    exit 23
  fi
  chown --reference="$orig" -- "$tmp" || exit 24
  chmod --reference="$orig" -- "$tmp" || exit 24
  if [ "$orig" = "$target" ]; then
    mv -f -T -- "$tmp" "$orig" || exit 25
  else
    if ! mv -T --update=none-fail -- "$tmp" "$target"; then
      if [ -e "$target" ] || [ -L "$target" ]; then
        exit 26
      fi
      exit 27
    fi
    if [ "$delete_original" = 1 ]; then
      rm -f -- "$orig" || exit 28
    fi
  fi
  stat -c '%s %.3Y' -- "$target"
}
finalize "$@"
"#;

/// Verify, take over owner/group/mode, then replace or publish. Returns the stat of the file now
/// at the output path, which is an in-place job's new fingerprint.
pub fn finalize(
    setup: &RemoteSetup,
    host: &str,
    req: &FinalizeRequest,
) -> Result<RemoteStat, RemoteError> {
    let size = req.result_size.to_string();
    let base_size = req.baseline.size.to_string();
    let base_mtime = req.baseline.mtime_ms.to_string();
    let out = run_script(
        setup,
        host,
        "finalize",
        FINALIZE_SCRIPT,
        &[
            req.source_path,
            req.remote_temp,
            req.output_path,
            &size,
            req.result_sha256,
            &base_size,
            &base_mtime,
            if req.delete_original { "1" } else { "0" },
        ],
    )?;
    let temp = req.remote_temp;
    let fail = |m: String| Err(RemoteError::Failed(m));
    match out.code {
        Some(0) => {
            let line = String::from_utf8_lossy(&out.stdout);
            match parse_stat_line(line.trim_end_matches('\n')) {
                Some((stat, _)) => Ok(stat),
                None => fail(format!("Unexpected answer from {host} after finalizing")),
            }
        }
        Some(20) => fail(format!(
            "The original on {host} is gone; the result was left as {temp}"
        )),
        Some(21) => fail(format!(
            "The original on {host} changed since it was pulled; both it and {temp} were left"
        )),
        Some(22) => fail(format!("The uploaded result {temp} is missing on {host}")),
        Some(23) => fail(format!(
            "The uploaded result did not match the local one (size or SHA-256); {temp} was removed"
        )),
        Some(24) => fail(format!(
            "Could not give {temp} the original's owner, group and mode on {host}; both were left"
        )),
        Some(25) => fail(format!(
            "Could not replace the original on {host}; both it and {temp} were left"
        )),
        Some(26) => fail(format!(
            "{} already exists on {host}; nothing was overwritten, {temp} was left",
            req.output_path
        )),
        Some(27) => fail(format!(
            "Could not publish {} on {host}; {temp} was left",
            req.output_path
        )),
        Some(28) => fail(format!(
            "Published {}, but could not delete the original on {host}; both remain",
            req.output_path
        )),
        _ => fail(format!("Finalizing on {host} failed: {}", out.stderr)),
    }
}

const REMOVE_TEMP_SCRIPT: &str = r#"remove_temp() {
  case $1 in
    */.*.convertbar-tmp.*) rm -f -- "$1" ;;
  esac
}
remove_temp "$@"
"#;

/// Best-effort removal of a job's remote temp. Refuses anything not shaped like a marked temp.
pub fn remove_temp(setup: &RemoteSetup, host: &str, remote_temp: &str) {
    let _ = run_script(setup, host, "cleanup", REMOVE_TEMP_SCRIPT, &[remote_temp]);
}

/// An ssh stand-in for tests: it drops the options and the host and runs the remote command
/// locally, with GNU coreutils and findutils first on `PATH`. Control files in its directory:
/// `fail-255` (one step per line answers exit 255), `hook-<step>` (run before that step), and
/// `bin-<step>/` (prepended to `PATH` for that step). Every invocation's step is appended to `log`.
#[cfg(all(unix, any(test, feature = "test-support")))]
pub mod testing {
    use super::*;

    /// Directories holding GNU coreutils ≥ 9.5 and GNU findutils, joined for `PATH`. Panics with
    /// install instructions when there are none: these tests must never silently pass.
    pub fn gnu_path() -> String {
        if let Ok(p) = std::env::var("CONVERTBAR_TEST_GNUBIN") {
            return p;
        }
        let candidates = [
            vec![
                "/opt/homebrew/opt/coreutils/libexec/gnubin",
                "/opt/homebrew/opt/findutils/libexec/gnubin",
            ],
            vec![
                "/usr/local/opt/coreutils/libexec/gnubin",
                "/usr/local/opt/findutils/libexec/gnubin",
            ],
            vec!["/home/linuxbrew/.linuxbrew/bin"],
            vec!["/usr/bin"],
        ];
        for dirs in candidates {
            let joined = dirs.join(":");
            let mv_ok = Command::new("sh")
                .arg("-c")
                .arg("mv --help 2>/dev/null | grep -q none-fail && find . -maxdepth 0 -printf ''")
                .env("PATH", format!("{joined}:/usr/bin:/bin"))
                .status()
                .is_ok_and(|s| s.success());
            if mv_ok {
                return joined;
            }
        }
        panic!(
            "remote-script tests need GNU coreutils >= 9.5 and GNU findutils: \
             `brew install coreutils findutils`, or set CONVERTBAR_TEST_GNUBIN"
        );
    }

    /// Writes the shim under `base` and returns a setup whose roots are `roots` (host, dir).
    /// Staging is `base/staging`.
    pub fn shim_setup(base: &Path, roots: &[(&str, &str)]) -> RemoteSetup {
        use std::os::unix::fs::PermissionsExt;
        let dir = base.join("shim");
        std::fs::create_dir_all(&dir).unwrap();
        let staging = base.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let script = format!(
            r#"#!/bin/sh
dir={dir}
while [ $# -gt 0 ]; do
  case $1 in --) shift; break ;; *) shift ;; esac
done
shift
cmd=$*
step=${{cmd#: convertbar-}}
step=${{step%%;*}}
echo "$step" >> "$dir/log"
if [ -f "$dir/fail-255" ] && grep -qx "$step" "$dir/fail-255"; then
  exit 255
fi
if [ -x "$dir/hook-$step" ]; then
  "$dir/hook-$step" </dev/null || exit $?
fi
PATH={gnu}:$PATH
if [ -d "$dir/bin-$step" ]; then
  PATH="$dir/bin-$step:$PATH"
fi
export PATH
exec sh -c "$cmd"
"#,
            dir = quote(&dir.to_string_lossy()),
            gnu = quote(&gnu_path()),
        );
        let ssh = dir.join("ssh");
        std::fs::write(&ssh, script).unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        RemoteSetup {
            roots: roots
                .iter()
                .map(|(h, d)| RemoteRoot {
                    host: h.to_string(),
                    dir: d.to_string(),
                })
                .collect(),
            staging,
            ssh,
        }
    }

    /// The steps the shim has run, in order.
    pub fn shim_log(setup: &RemoteSetup) -> Vec<String> {
        let log = setup.ssh.parent().unwrap().join("log");
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Makes `step` answer as an unreachable host.
    pub fn fail_unreachable(setup: &RemoteSetup, step: &str) {
        let path = setup.ssh.parent().unwrap().join("fail-255");
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str(step);
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    /// Ends every outage `fail_unreachable` set up.
    pub fn reachable_again(setup: &RemoteSetup) {
        let _ = std::fs::remove_file(setup.ssh.parent().unwrap().join("fail-255"));
    }

    /// Runs `script` (a POSIX sh body) just before `step` reaches the host.
    pub fn hook_before(setup: &RemoteSetup, step: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = setup.ssh.parent().unwrap().join(format!("hook-{step}"));
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Replaces `program` with one that fails, for `step` only.
    pub fn failing_program(setup: &RemoteSetup, step: &str, program: &str) {
        use std::os::unix::fs::PermissionsExt;
        let dir = setup.ssh.parent().unwrap().join(format!("bin-{step}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(program);
        std::fs::write(&path, "#!/bin/sh\necho \"$0: refused\" >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_entry_is_refused_unless_it_is_a_safe_alias_and_an_absolute_dir() {
        assert_eq!(
            parse_roots("unraid:/mnt/user/DATA, hetzner:/srv/media").unwrap(),
            vec![
                RemoteRoot {
                    host: "unraid".into(),
                    dir: "/mnt/user/DATA".into()
                },
                RemoteRoot {
                    host: "hetzner".into(),
                    dir: "/srv/media".into()
                },
            ]
        );
        assert_eq!(parse_roots("").unwrap(), vec![]);
        for bad in [
            "-oProxyCommand=touch:/x",
            "unraid",
            "unraid:relative/dir",
            "un raid:/x",
            "host;rm:/x",
            ":/x",
        ] {
            let err = parse_roots(bad).expect_err(bad);
            assert!(err.contains(bad), "the error names the entry: {err}");
        }
    }

    #[test]
    fn a_quoted_argument_survives_the_remote_shell_whatever_it_contains() {
        let nasty = "it's a \"film\"\n$(touch /tmp/x) `id`.mp4";
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("printf '%s' {}", quote(nasty)))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), nasty);
    }

    #[test]
    fn mtimes_are_milliseconds_like_local_fingerprints() {
        assert_eq!(parse_mtime_ms("1791834858.480"), Some(1_791_834_858_480));
        assert_eq!(parse_mtime_ms("1791834858.048"), Some(1_791_834_858_048));
        assert_eq!(
            parse_mtime_ms("1791834858.4809988110"),
            Some(1_791_834_858_480)
        );
        assert_eq!(parse_mtime_ms("garbage"), None);
    }
}
