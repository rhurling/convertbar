# Encode CPU Priority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `encode_priority` setting (`normal` / `low` / `idle`) that lowers the scheduling priority of the HandBrake child process, so a background encode yields to whatever else the user is doing.

**Architecture:** A new `convertbar-core/src/priority.rs` owns the platform mechanism (an `EncodePriority` enum, a parent-side `setpriority` call on Unix, a `creation_flags` value on Windows) and nothing else. `settings_ops` owns storage and normalization, as it already does for `cleanup_mode`. `converter::process_queue` reads the setting per job and applies it immediately after spawn. Both heads report a `priority_is_group_scoped` capability flag so the UI can tell Linux users the truth about why it may not work for them.

**Tech Stack:** Rust (workspace: `convertbar-core`, `convertbar-server`, `src-tauri`), `libc` (already a `cfg(unix)` dependency), React + TypeScript frontend, Pest-style Rust `#[test]`s and Vitest + React Testing Library.

## Global Constraints

- Platform-specific code is gated with `#[cfg]` **attributes**, never the `cfg!()` macro. `cfg!()` only skips code at runtime and would still require linking `libc` on every platform. This is a standing rule in CLAUDE.md.
- `libc` is declared under `[target.'cfg(unix)'.dependencies]` in `crates/convertbar-core/Cargo.toml:24-30`. Do not move it or add it as an unconditional dependency.
- Never emit an event while holding `ctx.db`'s lock. All settings reads in this plan happen inside existing lock scopes that emit nothing.
- Never fail an encode because a priority call failed. `EACCES`/`EPERM` (ConvertBar itself already niced) and `ESRCH` (child already exited) are expected in normal operation.
- `normal` must apply **nothing** — no `setpriority`, no creation flag — so the default path stays byte-identical to today's spawn.
- Test fixtures must declare their HandBrake world. Tests in this plan use the `handbrake_path` setting with a stub script, which bypasses the locator entirely (`converter.rs:2802` documents this), so `PanickingLocator` is not reached.
- Run the full suite with `cargo test --workspace`.
- Commit messages use conventional commits (`feat:`, `test:`, `fix:`).

**Reference spec:** `docs/superpowers/specs/2026-08-15-encode-priority-design.md`

## File Structure

| File | Responsibility |
|---|---|
| `crates/convertbar-core/src/priority.rs` **(new)** | The `EncodePriority` enum, its string normalizer, and the platform application mechanism. No database access, no settings knowledge. |
| `crates/convertbar-core/src/lib.rs` | Register the new module. |
| `crates/convertbar-core/src/settings_ops.rs` | Storage: `ALLOWED_KEYS`, `read_encode_priority`, the `get_settings` field. |
| `crates/convertbar-core/src/types.rs` | `Settings.encode_priority`. |
| `crates/convertbar-core/src/db.rs` | `DbInit` return type so a head can tell a fresh database from an existing one. |
| `crates/convertbar-core/src/converter.rs` | Read the setting per job; apply it at the spawn site. |
| `src-tauri/src/lib.rs` | Seed `low` on a fresh desktop database only. |
| `src-tauri/src/commands/converter.rs` | `PlatformCapabilities.priority_is_group_scoped`. |
| `crates/convertbar-server/src/routes/info.rs` | `AppInfo.priority_is_group_scoped`. |
| `src/lib/transport/types.ts` | `AppInfo` and `AppSettings` field declarations. |
| `src/lib/transport/tauri.ts` | Desktop synthesizes `AppInfo` from `get_platform_capabilities`. |
| `src/pages/SettingsPage.tsx` | The three-option control and the Linux note. |

`crates/convertbar-server/routes.json` needs **no** change: it maps commands to method and path only, and this plan adds no route.

---

### Task 1: The `EncodePriority` type and platform mechanism

Pure mechanism, isolated from settings and from the converter. It is testable against a child process the test itself spawns, which is what makes the macOS `idle` behavior observable at all.

**Files:**
- Create: `crates/convertbar-core/src/priority.rs`
- Modify: `crates/convertbar-core/src/lib.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub enum EncodePriority { Normal, Low, Idle }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn normalize_encode_priority(value: &str) -> EncodePriority`
  - `pub fn as_str(self) -> &'static str` on `EncodePriority`
  - `#[cfg(unix)] pub fn apply_to_child(pid: u32, priority: EncodePriority) -> std::io::Result<()>`
  - `#[cfg(windows)] pub fn creation_flags(priority: EncodePriority) -> u32`

- [ ] **Step 1: Write the failing tests**

Create `crates/convertbar-core/src/priority.rs` with only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_maps_known_values_and_defaults_everything_else_to_normal() {
        assert_eq!(normalize_encode_priority("low"), EncodePriority::Low);
        assert_eq!(normalize_encode_priority("idle"), EncodePriority::Idle);
        assert_eq!(normalize_encode_priority("normal"), EncodePriority::Normal);
        // A corrupted, empty, or future value must read as Normal — the tier that
        // changes nothing — rather than silently slowing every encode.
        assert_eq!(normalize_encode_priority(""), EncodePriority::Normal);
        assert_eq!(normalize_encode_priority("LOW"), EncodePriority::Normal);
        assert_eq!(normalize_encode_priority("banana"), EncodePriority::Normal);
    }

    #[test]
    fn as_str_round_trips_through_normalize() {
        for p in [
            EncodePriority::Normal,
            EncodePriority::Low,
            EncodePriority::Idle,
        ] {
            assert_eq!(
                normalize_encode_priority(p.as_str()),
                p,
                "{:?} must survive a store/read round trip",
                p
            );
        }
    }

    /// Spawns a real child, applies a tier, and reads the kernel's answer back. A stub that
    /// reports its own nice value cannot serve here: on macOS `Idle` is a background QoS
    /// class, not a nice value, so the nice-based probe would assert nothing for exactly the
    /// tier that justifies having three tiers.
    #[cfg(unix)]
    fn spawn_sleeper() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep")
    }

    #[cfg(unix)]
    fn nice_of(pid: u32) -> i32 {
        // Our values are 0, 10, 19 — never -1 — so getpriority's -1-means-either-error-or-
        // priority-minus-one ambiguity cannot bite, and no errno dance is needed.
        unsafe { libc::getpriority(libc::PRIO_PROCESS as _, pid as _) }
    }

    #[test]
    #[cfg(unix)]
    fn normal_leaves_the_child_untouched() {
        let mut child = spawn_sleeper();
        let before = nice_of(child.id());
        apply_to_child(child.id(), EncodePriority::Normal).unwrap();
        assert_eq!(
            nice_of(child.id()),
            before,
            "Normal must be a no-op so the default spawn path is unchanged"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    #[cfg(unix)]
    fn low_sets_nice_ten() {
        let mut child = spawn_sleeper();
        apply_to_child(child.id(), EncodePriority::Low).unwrap();
        assert_eq!(nice_of(child.id()), 10);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn idle_sets_nice_nineteen() {
        let mut child = spawn_sleeper();
        apply_to_child(child.id(), EncodePriority::Idle).unwrap();
        assert_eq!(nice_of(child.id()), 19);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// On macOS, `Idle` is `PRIO_DARWIN_BG` on `PRIO_DARWIN_PROCESS`: it puts the process in
    /// a background QoS class (efficiency cores, throttled disk I/O) and leaves the nice
    /// value alone. Both halves are asserted, because a regression to plain nice 19 would
    /// still "work" under a nice-only probe while losing the behavior we chose it for.
    #[test]
    #[cfg(target_os = "macos")]
    fn idle_sets_darwin_background_class_and_not_nice() {
        let mut child = spawn_sleeper();
        apply_to_child(child.id(), EncodePriority::Idle).unwrap();
        let bg = unsafe { libc::getpriority(libc::PRIO_DARWIN_PROCESS as _, child.id() as _) };
        assert_eq!(bg, libc::PRIO_DARWIN_BG, "child must be in the background class");
        assert_eq!(nice_of(child.id()), 0, "PRIO_DARWIN_BG is not a nice value");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    #[cfg(windows)]
    fn creation_flags_map_to_distinct_priority_classes() {
        assert_eq!(creation_flags(EncodePriority::Normal), 0);
        assert_eq!(creation_flags(EncodePriority::Low), 0x0000_4000);
        assert_eq!(creation_flags(EncodePriority::Idle), 0x0000_0040);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core priority:: 2>&1 | tail -20`
Expected: FAIL — compile errors, `cannot find type EncodePriority in this scope`. (The module is not registered yet, so this may report that `priority.rs` is not part of the crate; that is the same failure.)

- [ ] **Step 3: Register the module**

In `crates/convertbar-core/src/lib.rs`, add `pub mod priority;` in the existing alphabetical run of `pub mod` declarations.

- [ ] **Step 4: Write the implementation**

Prepend to `crates/convertbar-core/src/priority.rs`, above the test module:

```rust
//! Scheduling priority for the HandBrake child process.
//!
//! Proportional share, not a cap: a lowered process still uses every idle core, it just
//! loses to anything else that wants the CPU. Effective on macOS and Windows. On Linux it
//! is largely confined to ConvertBar's own scheduling group — see the spec at
//! `docs/superpowers/specs/2026-08-15-encode-priority-design.md` — which is why the UI
//! carries a note there rather than the setting being hidden.

/// The three tiers the user can choose between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodePriority {
    Normal,
    Low,
    Idle,
}

impl EncodePriority {
    /// The stored representation. Round-trips through [`normalize_encode_priority`].
    pub fn as_str(self) -> &'static str {
        match self {
            EncodePriority::Normal => "normal",
            EncodePriority::Low => "low",
            EncodePriority::Idle => "idle",
        }
    }
}

/// Coerce a stored `encode_priority` to a known tier. Anything other than an exact "low" or
/// "idle" reads as `Normal`: a corrupted, empty, or future value must never silently slow
/// the user's encodes. Sibling of `settings_ops::normalize_cleanup_mode`.
pub fn normalize_encode_priority(value: &str) -> EncodePriority {
    match value {
        "low" => EncodePriority::Low,
        "idle" => EncodePriority::Idle,
        _ => EncodePriority::Normal,
    }
}

/// Lower a spawned child's scheduling priority.
///
/// Called from the parent on the child's pid, deliberately not from `pre_exec`: the parent
/// side needs no `unsafe` fork/exec reasoning, makes no async-signal-safety claim, and
/// dodges the question of whether Darwin's background task policy survives `execve`. It is
/// also what lets the tests read the result back with `getpriority`. The few microseconds
/// the child spends at normal priority before this lands are irrelevant to a multi-minute
/// encode.
///
/// `Normal` is a no-op by construction, not by writing a zero.
#[cfg(unix)]
pub fn apply_to_child(pid: u32, priority: EncodePriority) -> std::io::Result<()> {
    let (which, value) = match priority {
        EncodePriority::Normal => return Ok(()),
        EncodePriority::Low => (libc::PRIO_PROCESS, 10),
        #[cfg(target_os = "macos")]
        EncodePriority::Idle => (libc::PRIO_DARWIN_PROCESS, libc::PRIO_DARWIN_BG),
        #[cfg(not(target_os = "macos"))]
        EncodePriority::Idle => (libc::PRIO_PROCESS, 19),
    };

    // `as _` on both: `setpriority`'s first parameter is `c_int` on macOS but `c_uint` on
    // glibc, and `who` is `id_t`. Inference picks the right one per target.
    let rc = unsafe { libc::setpriority(which as _, pid as _, value as _) };
    if rc == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `BELOW_NORMAL_PRIORITY_CLASS`. Declared here rather than pulled from a crate: it is a
/// plain `u32` and adding a Windows API dependency for two constants is not worth it.
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

/// `IDLE_PRIORITY_CLASS`.
#[cfg(windows)]
const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;

/// The `CreateProcess` flag for a tier, to be OR'd into `CommandExt::creation_flags`.
///
/// Windows sets priority at spawn rather than on a live pid, so unlike the Unix path this
/// runs *before* the process exists. `Normal` yields 0, which changes nothing.
#[cfg(windows)]
pub fn creation_flags(priority: EncodePriority) -> u32 {
    match priority {
        EncodePriority::Normal => 0,
        EncodePriority::Low => BELOW_NORMAL_PRIORITY_CLASS,
        EncodePriority::Idle => IDLE_PRIORITY_CLASS,
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core priority:: 2>&1 | tail -20`
Expected: PASS — 4 tests on Linux, 4 on macOS (different `idle` test selected by `cfg`).

**If `idle_sets_darwin_background_class_and_not_nice` fails with `EPERM`:** `setpriority(PRIO_DARWIN_PROCESS, …)` on a *different* process may be restricted on some macOS versions. This is the one platform behavior in this plan that could not be verified from documentation alone. If it fails, stop and report it — the fallback is mapping macOS `Idle` to nice 19 like Linux, which loses the efficiency-core and I/O-throttling benefit and therefore weakens the case for a third tier. That is a spec-level decision, not an implementation detail to paper over.

- [ ] **Step 6: Commit**

```bash
git add crates/convertbar-core/src/priority.rs crates/convertbar-core/src/lib.rs
git commit -m "feat(core): add EncodePriority and its platform application"
```

---

### Task 2: Store and read the setting

**Files:**
- Modify: `crates/convertbar-core/src/types.rs:26-46` (the `Settings` struct)
- Modify: `crates/convertbar-core/src/settings_ops.rs:39-59` (`ALLOWED_KEYS`), and `get_settings`
- Test: `crates/convertbar-core/src/settings_ops.rs` (its existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::priority::{EncodePriority, normalize_encode_priority}` from Task 1.
- Produces:
  - `pub fn read_encode_priority(conn: &rusqlite::Connection) -> EncodePriority`
  - `Settings.encode_priority: String` (always one of `"normal"`, `"low"`, `"idle"`)

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/convertbar-core/src/settings_ops.rs`, next to `read_cleanup_mode_normalizes_what_it_reads`:

```rust
#[test]
fn read_encode_priority_defaults_to_normal_when_the_row_is_absent() {
    let conn = test_conn();
    // init_db deliberately does NOT seed this key: the default is head-dependent, and core
    // is head-agnostic. An absent row must therefore be the safe tier, not a panic.
    assert_eq!(
        crate::priority::EncodePriority::Normal,
        read_encode_priority(&conn)
    );
}

#[test]
fn read_encode_priority_normalizes_what_it_reads() {
    let conn = test_conn();
    for (stored, expected) in [
        ("low", crate::priority::EncodePriority::Low),
        ("idle", crate::priority::EncodePriority::Idle),
        ("normal", crate::priority::EncodePriority::Normal),
        ("", crate::priority::EncodePriority::Normal),
        ("nice-19", crate::priority::EncodePriority::Normal),
    ] {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('encode_priority', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            rusqlite::params![stored],
        )
        .unwrap();
        assert_eq!(
            expected,
            read_encode_priority(&conn),
            "stored {:?} must read as {:?}",
            stored,
            expected
        );
    }
}

#[test]
fn get_settings_returns_a_normalized_encode_priority() {
    let (ctx, _sink, _d) = test_ctx(test_conn());

    // No row yet — the snapshot the UI renders must still be a value it can render.
    assert_eq!(get_settings(&ctx).unwrap().encode_priority, "normal");

    update_setting(&ctx, "encode_priority", "idle").unwrap();
    assert_eq!(get_settings(&ctx).unwrap().encode_priority, "idle");

    // A value written by a newer version must not reach the frontend as-is.
    ctx.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE settings SET value = 'ultra-idle' WHERE key = 'encode_priority'",
            [],
        )
        .unwrap();
    assert_eq!(get_settings(&ctx).unwrap().encode_priority, "normal");
}

#[test]
fn encode_priority_is_an_allowed_key() {
    let (ctx, _sink, _d) = test_ctx(test_conn());
    // update_setting validates the key against ALLOWED_KEYS and nothing else; a key missing
    // from that list is rejected outright, which would make the setting unwritable.
    assert!(update_setting(&ctx, "encode_priority", "low").is_ok());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core settings_ops::tests::read_encode 2>&1 | tail -20`
Expected: FAIL — `cannot find function read_encode_priority in this scope`.

- [ ] **Step 3: Write the implementation**

In `crates/convertbar-core/src/types.rs`, add to the `Settings` struct after `history_show_duration`:

```rust
    /// Always normalized on read — `"normal"`, `"low"`, or `"idle"`, never a raw column value.
    pub encode_priority: String,
```

In `crates/convertbar-core/src/settings_ops.rs`, add `"encode_priority",` to the end of `ALLOWED_KEYS` (after `"history_show_duration",`).

Add the reader next to `read_cleanup_mode`:

```rust
/// The stored `encode_priority`, normalized. The single read path, so no call site ever
/// string-compares a raw column. An absent row reads as `Normal`: core seeds no default for
/// this key, because the default is head-dependent (fresh desktop installs get `low`, the
/// server head gets `normal`) and core is head-agnostic.
pub fn read_encode_priority(conn: &rusqlite::Connection) -> crate::priority::EncodePriority {
    let raw: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'encode_priority'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    crate::priority::normalize_encode_priority(&raw)
}
```

In `get_settings`, add the local default alongside the others (near `let mut history_show_duration = true;`):

```rust
    let mut encode_priority = String::from("normal");
```

Add the match arm before the `_ => {}` arm:

```rust
            "encode_priority" => {
                encode_priority = crate::priority::normalize_encode_priority(&value).as_str().to_string()
            }
```

And add `encode_priority,` to the `Ok(Settings { … })` construction.

**Do not add value validation to `update_setting`.** It validates the key against `ALLOWED_KEYS` and nothing else (`settings_ops.rs:191`); no setting in this codebase validates its value on write, `cleanup_mode` included. The normalizer already makes an invalid stored value harmless.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core settings_ops 2>&1 | tail -20`
Expected: PASS. The `Settings` struct gained a field, so any other construction site will fail to compile — fix those by adding `encode_priority: "normal".to_string()` if they are test fixtures.

- [ ] **Step 5: Run the whole workspace to catch struct-construction breakage**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/convertbar-core/src/settings_ops.rs crates/convertbar-core/src/types.rs
git commit -m "feat(core): store and normalize the encode_priority setting"
```

---

### Task 3: Apply the priority at the spawn site

**Files:**
- Modify: `crates/convertbar-core/src/converter.rs:833-849` (the per-job settings read), `:1004-1014` (the spawn), `:1033` (immediately after `let pid = child.id();`)
- Test: `crates/convertbar-core/src/converter.rs` (its existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::priority::{EncodePriority, apply_to_child, creation_flags}` (Task 1), `crate::settings_ops::read_encode_priority` (Task 2).
- Produces: no new public API.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/convertbar-core/src/converter.rs`:

```rust
/// A stand-in for HandBrakeCLI that records the nice value it was given, then exits.
/// `ps -o nice=` is used rather than /proc so the same script works on macOS and Linux.
#[cfg(unix)]
fn nice_recording_fake_handbrake_script(
    dir: &std::path::Path,
    record_to: &std::path::Path,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("hb-nice.sh");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\nps -o nice= -p $$ | tr -d ' ' > {}\nexit 0\n",
            record_to.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Proves `process_queue` actually reads the setting and applies it to the process it
/// spawned — Task 1's unit tests only prove the mechanism works when called directly.
///
/// `low` is asserted rather than `idle` because `low` is nice 10 on both Unix platforms,
/// while `idle` is a non-nice background class on macOS that this probe cannot see.
#[test]
#[cfg(unix)]
fn process_queue_applies_the_configured_priority_to_the_child() {
    let (ctx, _sink, _disposer) = test_ctx(test_conn());

    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("nice.txt");
    let script = nice_recording_fake_handbrake_script(dir.path(), &record);
    set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
    set_setting(&ctx.db, "encode_priority", "low");

    let src = real_source(dir.path(), "a.mp4");
    let out = dir.path().join("out.mp4");
    queue_job(
        &ctx.db,
        "j1",
        src.to_str().unwrap(),
        out.to_str().unwrap(),
        1000,
    );

    process_queue(&ctx);

    let recorded = std::fs::read_to_string(&record)
        .expect("the stub ran and recorded its nice value")
        .trim()
        .to_string();
    assert_eq!(
        recorded, "10",
        "the encode must run at the configured priority, not the parent's"
    );
}

/// The default path must be untouched: a user who never opens Settings gets exactly the
/// spawn behavior they had before this feature existed.
#[test]
#[cfg(unix)]
fn process_queue_leaves_the_child_at_the_parent_priority_by_default() {
    let (ctx, _sink, _disposer) = test_ctx(test_conn());

    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("nice.txt");
    let script = nice_recording_fake_handbrake_script(dir.path(), &record);
    set_setting(&ctx.db, "handbrake_path", script.to_str().unwrap());
    // No encode_priority row at all — core seeds none.

    let src = real_source(dir.path(), "a.mp4");
    let out = dir.path().join("out.mp4");
    queue_job(
        &ctx.db,
        "j1",
        src.to_str().unwrap(),
        out.to_str().unwrap(),
        1000,
    );

    process_queue(&ctx);

    let recorded = std::fs::read_to_string(&record).unwrap().trim().to_string();
    let parent_nice = unsafe { libc::getpriority(libc::PRIO_PROCESS as _, 0) };
    assert_eq!(
        recorded,
        parent_nice.to_string(),
        "with no setting stored, the child must inherit the parent's priority untouched"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p convertbar-core process_queue_applies_the_configured 2>&1 | tail -20`
Expected: FAIL — the recorded nice value is the parent's (usually `0`), not `10`.

- [ ] **Step 3: Write the implementation**

In `converter.rs`, inside the existing `{ let db = ctx.db.lock().unwrap(); … }` block around line 833, declare the binding alongside `handbrake_path_opt` and `low_disk_min_gb`:

```rust
        let encode_priority;
```

and inside the lock scope, after `low_disk_min_gb = get_low_disk_min_gb(&db);`:

```rust
            // Read per job, not once per queue run: "applies to the next encode" is the
            // documented semantics, and handbrake_path is already re-read the same way.
            encode_priority = crate::settings_ops::read_encode_priority(&db);
```

Replace the spawn at line 1004 with a builder so Windows can set its flag before the process exists:

```rust
        // Spawn HandBrakeCLI
        let mut cmd = Command::new(&handbrake_path);
        cmd.arg("-Z")
            .arg(&job.preset)
            .arg("-O")
            .arg("-i")
            .arg(&job.source_path)
            .arg("-o")
            .arg(&encode_target)
            .stderr(Stdio::piped())
            .stdout(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(crate::priority::creation_flags(encode_priority));
        }
        let child = cmd.spawn();
```

Immediately after the existing `let pid = child.id();` (line 1033), add:

```rust
        // Best-effort by design: EACCES when ConvertBar is itself already niced (setpriority
        // sets an absolute value, so `low` is then a *raise*, which RLIMIT_NICE forbids), and
        // ESRCH if HandBrake already died on a bad input. Neither is a reason to fail an
        // encode that is otherwise fine.
        #[cfg(unix)]
        if let Err(e) = crate::priority::apply_to_child(pid, encode_priority) {
            eprintln!("could not set encode priority for pid {}: {}", pid, e);
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core process_queue 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Verify the test is load-bearing (mutation check)**

Behavior that can silently no-op needs proof its test would notice. Commit first — the restore step below reverts to the last commit and would otherwise destroy uncommitted work.

```bash
git add -A && git commit -m "feat(core): apply encode priority to the HandBrake child"
```

Now neuter the feature and confirm the test goes red:

```bash
# Make apply_to_child a no-op
perl -0pi -e 's/(pub fn apply_to_child\(pid: u32, priority: EncodePriority\) -> std::io::Result<\(\)> \{)/$1\n    return Ok(());/' crates/convertbar-core/src/priority.rs
cargo test -p convertbar-core process_queue_applies_the_configured 2>&1 | tail -15
```

Expected: **FAIL** with `assertion `left == right` failed` comparing `"0"` to `"10"`. A PASS here means the test proves nothing — stop and fix the test before continuing.

Restore:

```bash
git checkout crates/convertbar-core/src/priority.rs
cargo test -p convertbar-core process_queue 2>&1 | tail -5
```

Expected: PASS again.

- [ ] **Step 6: Run the full workspace**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: PASS.

- [ ] **Step 7: Commit**

The implementation was committed in Step 5. If the mutation check required test fixes, commit those now:

```bash
git add -A
git commit -m "test(core): prove the encode-priority test fails without the feature"
```

---

### Task 4: `DbInit` and the fresh-install desktop default

**Files:**
- Modify: `crates/convertbar-core/src/db.rs:78` (`init_db` signature and body)
- Modify: `src-tauri/src/lib.rs:89` (the `.setup()` call site)
- Test: `crates/convertbar-core/src/db.rs` (its existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub enum DbInit { Fresh, Existing }` (derives `Debug, Clone, Copy, PartialEq, Eq`), and `init_db` now returns `Result<DbInit>`.

**Critical:** `DbInit` must **not** be `#[must_use]`. There are 74 existing `init_db(&conn).unwrap();` call sites that discard the value; marking the type would produce 74 warnings.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/convertbar-core/src/db.rs`:

```rust
#[test]
fn init_db_reports_fresh_only_the_first_time() {
    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(
        init_db(&conn).unwrap(),
        DbInit::Fresh,
        "a database with no settings table has never been initialized"
    );
    assert_eq!(
        init_db(&conn).unwrap(),
        DbInit::Existing,
        "re-running init_db on the same connection must not look like a new install"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p convertbar-core init_db_reports_fresh 2>&1 | tail -20`
Expected: FAIL — `cannot find type DbInit in this scope`.

- [ ] **Step 3: Write the implementation**

In `crates/convertbar-core/src/db.rs`, above `init_db`:

```rust
/// Whether [`init_db`] found an existing database or created one.
///
/// A head uses this to seed a default that must apply to new installs only. Deliberately
/// **not** `#[must_use]`: 74 call sites legitimately discard it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbInit {
    Fresh,
    Existing,
}
```

Change the signature and open the body with the probe — it must run **before** `execute_batch` creates the tables:

```rust
pub fn init_db(conn: &Connection) -> Result<DbInit> {
    // Probed before any CREATE TABLE below, which is what makes the answer meaningful.
    let state = if conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'settings'",
        [],
        |row| row.get::<_, i64>(0),
    )? == 0
    {
        DbInit::Fresh
    } else {
        DbInit::Existing
    };

    let preset = default_preset();
```

Change the final `Ok(())` of `init_db` to `Ok(state)`.

In `src-tauri/src/lib.rs`, at the `init_db` call inside `.setup()` (line 89), capture the result and seed:

```rust
            let db_state = convertbar_core::db::init_db(&conn).expect("init db");
            // Fresh desktop installs start at `low`: a menu-bar app shares the machine with
            // the user's actual work. Existing installs are left alone — an auto-update must
            // not silently change how fast anyone's encodes run. The server head seeds
            // nothing and inherits `normal`.
            if db_state == convertbar_core::db::DbInit::Fresh {
                conn.execute(
                    "INSERT OR IGNORE INTO settings (key, value) VALUES ('encode_priority', 'low')",
                    [],
                )
                .expect("seed encode_priority");
            }
```

Adjust the surrounding binding name to whatever `lib.rs:89` currently uses for the connection.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-core db:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Verify no call site broke**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: PASS, with no `unused_must_use` warnings. If warnings appear, `DbInit` was marked `#[must_use]` — remove the attribute.

- [ ] **Step 6: Commit**

```bash
git add crates/convertbar-core/src/db.rs src-tauri/src/lib.rs
git commit -m "feat(desktop): default fresh installs to low encode priority"
```

---

### Task 5: Report the Linux caveat through both heads

**Files:**
- Modify: `src-tauri/src/commands/converter.rs:49-58` (`PlatformCapabilities`)
- Modify: `crates/convertbar-server/src/routes/info.rs:12-36` (`AppInfo`)
- Modify: `src/lib/transport/types.ts:210-218` (`AppInfo`)
- Modify: `src/lib/transport/tauri.ts:112-124` (desktop synthesis)
- Test: `crates/convertbar-server/src/routes/mod.rs` (the existing app-info route test near `:1076`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `AppInfo.priority_is_group_scoped: boolean` in TypeScript, available to Task 6.

The desktop has no `AppInfo` struct in Rust — `src/lib/transport/tauri.ts:112-124` synthesizes it from `getVersion()` plus the `get_platform_capabilities` command. So the flag goes into `PlatformCapabilities` on desktop and into `AppInfo` on the server, and both surface in the one shared TypeScript `AppInfo` interface.

- [ ] **Step 1: Write the failing test**

In `crates/convertbar-server/src/routes/mod.rs`, next to the existing `assert_eq!(json["can_pause_process"], cfg!(unix));` around line 1076:

```rust
        // Linux confines scheduling priority to a cgroup/autogroup, so the encode-priority
        // setting largely does not reach the rest of the host there. The UI needs to know at
        // runtime, not build time: one frontend bundle per head serves every OS.
        assert_eq!(json["priority_is_group_scoped"], cfg!(target_os = "linux"));
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p convertbar-server app_info 2>&1 | tail -20`
Expected: FAIL — the JSON has no `priority_is_group_scoped` key, so the comparison is against `Null`.

- [ ] **Step 3: Write the implementation**

In `crates/convertbar-server/src/routes/info.rs`, add to the `AppInfo` struct after `can_pause_process`:

```rust
    /// True on Linux, where the kernel confines scheduling priority to an autogroup or
    /// cgroup, so `encode_priority` largely cannot yield CPU to the rest of the host. Runtime
    /// data rather than a build-time frontend flag: the bundle is built per head, not per OS.
    pub priority_is_group_scoped: bool,
```

and to its construction after `can_pause_process: cfg!(unix),`:

```rust
            priority_is_group_scoped: cfg!(target_os = "linux"),
```

In `src-tauri/src/commands/converter.rs`, add the same field to `PlatformCapabilities` and its constructor:

```rust
pub struct PlatformCapabilities {
    pub can_pause_process: bool,
    pub priority_is_group_scoped: bool,
}

#[tauri::command]
pub fn get_platform_capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        can_pause_process: ConverterState::can_pause_process(),
        priority_is_group_scoped: cfg!(target_os = "linux"),
    }
}
```

In `src/lib/transport/types.ts`, add to the `AppInfo` interface after `can_pause_process`:

```typescript
  // True on Linux, where the kernel confines scheduling priority to an autogroup or cgroup —
  // in a container and out of one — so encode_priority largely cannot yield CPU to the rest
  // of the host. Runtime data, not a build-time flag: one bundle per head serves every OS.
  priority_is_group_scoped: boolean;
```

and to `AppSettings`, after `history_show_duration`:

```typescript
  // Narrowed like bad_source_action: get_settings normalizes the raw stored string before
  // returning it, so only these three ever reach the frontend.
  encode_priority: "normal" | "low" | "idle";
```

Also add `priority_is_group_scoped: boolean;` to the `PlatformCapabilities` interface in the same file if one is declared there.

In `src/lib/transport/tauri.ts:112-124`, add to the synthesized object:

```typescript
      priority_is_group_scoped: caps.priority_is_group_scoped,
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p convertbar-server app_info 2>&1 | tail -20` — expected PASS.
Run: `npm run build 2>&1 | tail -20` — expected: succeeds. TypeScript will flag any other `AppInfo` literal (test fixtures, mocks) that now lacks the field; add `priority_is_group_scoped: false` to each.

- [ ] **Step 5: Commit**

```bash
git add crates/convertbar-server/src/routes src-tauri/src/commands/converter.rs src/lib/transport
git commit -m "feat: report whether scheduling priority is group-scoped"
```

---

### Task 6: The Settings control and the Linux note

**Files:**
- Modify: `src/pages/SettingsPage.tsx` (the `useEffect` at :53-60, and a new setting group after the "After conversion" group ending at :307)
- Test: `src/pages/SettingsPage.test.tsx`

**Interfaces:**
- Consumes: `AppSettings.encode_priority` and `AppInfo.priority_is_group_scoped` (Task 5), `updateSetting` from the existing `useSettings()` destructure.
- Produces: no new API.

- [ ] **Step 1: Write the failing tests**

Add to `src/pages/SettingsPage.test.tsx`, following the file's existing render/mock setup:

```tsx
it("writes the chosen encode priority", async () => {
  renderSettings({ encode_priority: "normal" });

  const idle = await screen.findByLabelText(/only when the machine is idle/i);
  fireEvent.click(idle);

  expect(updateSetting).toHaveBeenCalledWith("encode_priority", "idle");
});

it("shows the Linux caveat only when priority is group-scoped", async () => {
  // The setting is offered on Linux rather than hidden — autogrouping can be disabled, and a
  // process with no cpu controller on its path does get real host-wide nice — so the note is
  // what keeps it honest for the users where it does nothing.
  renderSettings({ encode_priority: "low" }, { priority_is_group_scoped: true });
  expect(await screen.findByText(/--cpu-shares/)).toBeInTheDocument();
});

it("shows no caveat where priority works normally", async () => {
  renderSettings({ encode_priority: "low" }, { priority_is_group_scoped: false });
  await screen.findByLabelText(/only when the machine is idle/i);
  expect(screen.queryByText(/--cpu-shares/)).not.toBeInTheDocument();
});
```

Adapt `renderSettings` to the helper this test file already uses; its second argument must control what `commands.getAppInfo()` resolves to.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/pages/SettingsPage.test.tsx 2>&1 | tail -20`
Expected: FAIL — `Unable to find a label with the text of: /only when the machine is idle/i`.

- [ ] **Step 3: Write the implementation**

In `src/pages/SettingsPage.tsx`, extend the existing app-info effect (:53-60) so it also runs on desktop and captures the flag:

```tsx
  const [groupScoped, setGroupScoped] = useState(false);

  // getAppInfo() works on both heads (desktop composes it from getVersion() internally,
  // server hits /api/info). The version is only rendered on the server head, but the
  // priority caveat is per-OS and so is needed on both.
  useEffect(() => {
    commands
      .getAppInfo()
      .then((info) => {
        setAppVersion(info.version);
        setGroupScoped(info.priority_is_group_scoped);
      })
      .catch(() => {});
  }, []);
```

Add a new setting group after the "After conversion" group (which ends at :307):

```tsx
      <div className="setting-group">
        <label className="setting-label">Encode priority</label>
        <div className="setting-radios">
          <label className="radio-label">
            <input
              type="radio"
              name="encode-priority"
              checked={settings.encode_priority === "normal"}
              onChange={() => updateSetting("encode_priority", "normal")}
            />
            Normal — compete equally with other apps
          </label>
          <label className="radio-label">
            <input
              type="radio"
              name="encode-priority"
              checked={settings.encode_priority === "low"}
              onChange={() => updateSetting("encode_priority", "low")}
            />
            Low — yield to other apps when the CPU is busy
          </label>
          <label className="radio-label">
            <input
              type="radio"
              name="encode-priority"
              checked={settings.encode_priority === "idle"}
              onChange={() => updateSetting("encode_priority", "idle")}
            />
            Idle — encode only when the machine is idle
          </label>
        </div>
        <p className="setting-hint">
          This is not a CPU limit: encodes still use every core nothing else wants. It applies
          to the next encode, not one already running.
        </p>
        {groupScoped && (
          <p className="setting-hint">
            On Linux this often has little effect — the kernel confines priority to a process
            group, so the encode yields to ConvertBar itself rather than to the rest of the
            machine. To free CPU for other work, use <code>--cpu-shares</code> on the Docker
            container or <code>CPUWeight=</code> on the systemd unit.
          </p>
        )}
      </div>
```

The `label`/`input` nesting means the accessible name is the full option text, which is what the tests query by.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npx vitest run src/pages/SettingsPage.test.tsx 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Run the full frontend suite and build**

Run: `npx vitest run 2>&1 | tail -20` — expected PASS.
Run: `npm run build 2>&1 | tail -10` — expected: succeeds.

- [ ] **Step 6: Commit**

```bash
git add src/pages/SettingsPage.tsx src/pages/SettingsPage.test.tsx
git commit -m "feat(ui): add the encode priority control and its Linux caveat"
```

---

### Task 7: Full verification

**Files:** none modified unless a failure surfaces.

- [ ] **Step 1: Run everything**

```bash
cargo test --workspace 2>&1 | tail -30
npx vitest run 2>&1 | tail -20
npm run build 2>&1 | tail -10
```

Expected: all green. `frontend` and `rust (ubuntu-22.04)` are the required CI checks, so these are the gates that matter.

- [ ] **Step 2: Check formatting drift**

```bash
cargo fmt --all && git diff --stat
```

Expected: no changes. If `cargo fmt` rewrote anything, commit it — CI does not gate formatting, but the tree is currently fmt-clean.

- [ ] **Step 3: Confirm the macOS `idle` path actually ran**

On a macOS machine:

```bash
cargo test -p convertbar-core idle_sets_darwin 2>&1 | tail -10
```

Expected: PASS. PR CI's Rust matrix is ubuntu-only (macOS runs on `main`), so this is the one assertion that will not be checked before merge. If it cannot be run locally, say so explicitly rather than reporting the feature as verified — `PRIO_DARWIN_BG` on another process is the single behavior in this plan that documentation alone could not confirm.

- [ ] **Step 4: Report**

State plainly which of the three platforms were actually exercised and which were only compiled. Do not describe the feature as verified on a platform whose tests did not run.

---

## Self-Review Notes

**Spec coverage.** Every section of the spec maps to a task: the setting and its normalizer (Task 2), the platform table and parent-side application (Task 1), the spawn wiring and per-job read semantics (Task 3), `DbInit` and fresh-install seeding (Task 4), the capability flag through both heads (Task 5), the control and Linux note (Task 6).

**Two deliberate deviations from the spec, both noted at the point of use:**

1. The spec says failures are "logged". `convertbar-core` has no logging crate; `eprintln!` is used sparingly (three times, all in `watcher.rs`). Task 3 uses `eprintln!` to match that convention rather than introducing a logging dependency for one call site.
2. The spec refers to "`AppInfo` in both heads". Only the server head has an `AppInfo` struct in Rust; the desktop synthesizes its `AppInfo` in TypeScript from `get_platform_capabilities`. Task 5 puts the flag where each head actually builds its capability data.

**Excluded by the spec, and by this plan:** the probe and preset spawns at `probe.rs:95` and `handbrake.rs:154`/`:171`/`:236` stay at normal priority.
