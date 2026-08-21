//! The desktop head's IPC adapters. Every fallible `#[tauri::command]` in these modules fails
//! with [`CommandError`] — the desktop twin of the server head's `{"error": …, "kind": …}` body.

mod error;

pub mod converter;
pub mod files;
pub mod handbrake;
pub mod hooks;
pub mod queue;
pub mod settings;
pub mod updater;
pub mod watch;

pub use error::{blocking, CommandError};

#[cfg(test)]
mod tests {
    /// Comments are stripped before matching, so these checks read code rather than prose: a
    /// module may well mention a banned name in its docs precisely to say it no longer calls it.
    /// Stripping also means the bare identifier can be matched, which a turbofish
    /// (`spawn_blocking::<_>(`) would otherwise slip past.
    ///
    /// It strips line comments only. A `/* */` block or a `//` inside a string literal is not
    /// understood, which can produce a loud false positive (a block comment quoting a banned
    /// name) or hide code that follows a string containing `//` on the same line. Both are
    /// accepted: this is a backstop against drift, not a parser.
    fn code_only(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every command module, plus `mod.rs` itself — only `error.rs`, which owns the mapping, is
    /// exempt. Matched by full path rather than by name: `Path::ends_with` matches a trailing
    /// component, so a later `queue/error.rs` would silently inherit an exemption meant for the
    /// file holding the helper.
    fn command_modules() -> (Vec<(String, String)>, std::path::PathBuf) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
        let helpers = root.join("error.rs");

        let mut modules = Vec::new();
        let mut pending = vec![root];
        while let Some(dir) = pending.pop() {
            let entries =
                std::fs::read_dir(&dir).expect("command module directory must be readable");
            for entry in entries {
                let path = entry.expect("readable dir entry").path();
                // Splitting a grown module into a directory is a routine refactor; a
                // non-recursive walk would drop it from coverage without saying a word.
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path == helpers {
                    continue;
                }
                let source =
                    std::fs::read_to_string(&path).expect("command module must be readable");
                let name = path
                    .file_name()
                    .expect("named file")
                    .to_string_lossy()
                    .into_owned();
                modules.push((name, code_only(&source)));
            }
        }
        (modules, helpers)
    }

    #[test]
    fn command_modules_never_map_their_own_blocking_failures() {
        // Assembled rather than written out — and named so that neither the needles nor these
        // bindings read as a call site, because this file is scanned like any other module.
        let pool_needle = concat!("spawn_", "blocking");
        let panic_needle = concat!("task ", "panicked");
        // Both other ways to get work off this thread, and neither reaches `blocking`: a join
        // failure from either would be mapped by hand and land as an ordinary failure again.
        let async_needle = concat!("async_runtime::", "spawn");
        let thread_needle = concat!("thread::", "spawn");

        let (modules, helpers) = command_modules();
        for (module, source) in &modules {
            assert!(
                !source.contains(panic_needle),
                "{module} builds the panic message itself; go through `blocking` so every join \
                 failure keeps one definition"
            );
            for banned in [pool_needle, async_needle, thread_needle] {
                assert!(
                    !source.contains(banned),
                    "{module} runs its own task via `{banned}`; call `blocking` instead, so the \
                     panic-vs-deliberate-failure distinction cannot be spelled a second way"
                );
            }
            // A new command cannot opt back out of the shared shape. Without this, declaring
            // `-> Result<(), String>` compiles, registers, and reinstates exactly the gap this
            // module closed — for that one command, silently.
            assert!(
                !source.contains(concat!(", String", "> {")),
                "{module} has a command failing with a bare String; fail with `CommandError` so \
                 the frontend can still tell a bug from a condition"
            );
        }

        // `error.rs` is exempt because the helper itself must spawn. Pin how many times, so a
        // command added to THAT file cannot quietly bring its own join arm.
        let helper_source =
            code_only(&std::fs::read_to_string(&helpers).expect("error.rs must be readable"));
        assert_eq!(
            helper_source.matches(pool_needle).count(),
            1,
            "error.rs should spawn in exactly one place (`blocking`) — a second means something \
             there maps a blocking failure without going through the helper"
        );

        // Guards the walk itself: a walk that matched nothing would pass every assertion above
        // while checking no module at all. Kept well below the current count so that merging or
        // deleting a module is not reported as a broken walk.
        assert!(
            modules.len() >= 4,
            "expected to scan the command modules, only reached {} — did the walk break?",
            modules.len()
        );
    }

    /// True when `body` calls the helper itself, rather than merely containing the word — a
    /// sibling `fn scan_blocking(…)` would satisfy a plain `contains` while running the work
    /// synchronously on the caller's thread. `.blocking(` (a method), `self.blocking(` and
    /// `other::blocking(` are rejected for the same reason: only the imported free function is
    /// the helper this module vouches for.
    ///
    /// Two known limits, both consistent with these tests being a backstop rather than a parser:
    /// a string literal containing the call text vouches for a body that never calls it (only
    /// `//` comments are stripped), and a turbofish `blocking::<T, _>(…)` reads as a miss and
    /// would fail loudly. Neither occurs today.
    fn calls_the_helper(body: &str) -> bool {
        body.match_indices("blocking(").any(|(at, _)| {
            body[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '.' && c != ':')
        })
    }

    #[test]
    fn every_command_that_blocks_hands_its_work_to_the_helper() {
        // The tripwire above bans the *wrong* spellings; this one requires the right one.
        // Without it, a command can drop `blocking` for a direct call — which compiles, names no
        // banned identifier, and costs two things at once: the panic taxonomy, and the
        // off-main-thread guarantee the probe-hazard fix bought at four entry points (a deep
        // folder scan would freeze the UI again).
        //
        // Named rather than inferred, because inferring from `async` lets a command escape
        // coverage simply by dropping the keyword: rewritten sync, it stops matching, stops
        // being counted, and reinstates the freeze silently. Every entry here reaches a
        // subprocess, an unbounded walk, or a per-file probe.
        //
        // The three `files.rs` entries are here for the blocking round trip alone: `exists()`,
        // `metadata()` and `canonicalize()` each wait on the mount, which is the whole hazard
        // even though none of them touches HandBrake.
        //
        // This holds the line for the commands already off the main thread; it is NOT an
        // inventory of every main-thread hazard. The `watch.rs` commands are still sync and
        // canonicalize a user-chosen path before registering an OS watch, and `cancel_conversion`
        // removes a partial output — both block the main thread on a dead mount, and both are
        // recorded under item 16 in docs/RECOMMENDATIONS.md rather than fixed here.
        const MUST_BLOCK: [&str; 13] = [
            "add_files",
            "scan_folder",
            "confirm_folder_add",
            "purge_bad_sources",
            "classify_paths",
            "detect_handbrake",
            "list_handbrake_presets",
            "generate_preset_suffix",
            "validate_handbrake",
            "pick_folder",
            "check_paths_exist",
            "open_path",
            "reveal_in_dir",
        ];
        // Async for a reason other than blocking work: both await genuinely async updater work.
        const NOT_BLOCKING_WORK: [&str; 2] = ["check_for_update", "install_update"];

        let (modules, _) = command_modules();
        let mut seen = Vec::new();
        let mut exempt_seen = Vec::new();
        for (module, source) in &modules {
            // Split on `async fn` rather than `pub async fn`: a `pub(crate) async fn` registers
            // in `generate_handler` exactly the same and would otherwise be silently exempt.
            // Assembled so this file's own source is not one of its own matches.
            for chunk in source.split(concat!("async ", "fn ")).skip(1) {
                let name = chunk.split(['(', '<']).next().expect("a name").trim();
                if NOT_BLOCKING_WORK.contains(&name) {
                    exempt_seen.push(name);
                    continue;
                }
                // Stop at the next item so a later command's call cannot vouch for this one.
                let body = chunk.split("\n#[").next().unwrap_or(chunk);
                assert!(
                    calls_the_helper(body),
                    "{module}::{name} is async but never reaches `blocking`; either hand its \
                     work to the helper or add it to NOT_BLOCKING_WORK with a reason"
                );
            }

            for name in MUST_BLOCK {
                let Some((_, chunk)) = source.split_once(&format!("fn {name}(")) else {
                    continue;
                };
                assert!(
                    calls_the_helper(chunk.split("\n#[").next().unwrap_or(chunk)),
                    "{module}::{name} does work that must not run on the calling thread, but no \
                     longer reaches `blocking`"
                );
                seen.push(name);
            }
        }

        // Every named command must still exist somewhere. A rename that skipped this list would
        // otherwise drop it from coverage without a word.
        let mut missing: Vec<_> = MUST_BLOCK.iter().filter(|n| !seen.contains(n)).collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "MUST_BLOCK names commands that no longer exist: {missing:?} — renamed, or removed?"
        );

        // The same check for the exemptions, or a rename leaves one dangling forever and a later
        // command reusing the name inherits an exemption nobody granted it.
        let mut stale: Vec<_> = NOT_BLOCKING_WORK
            .iter()
            .filter(|n| !exempt_seen.contains(n))
            .collect();
        stale.sort();
        assert!(
            stale.is_empty(),
            "NOT_BLOCKING_WORK exempts commands that no longer exist: {stale:?}"
        );
    }

    #[test]
    fn every_command_lives_where_the_tripwires_look() {
        // Both tripwires above walk `src/commands` only. A `#[tauri::command]` defined anywhere
        // else in this crate registers in `generate_handler` just the same and would be exempt
        // from every check on this page without anyone deciding that.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let commands = src.join("commands");

        let mut stray = Vec::new();
        let mut pending = vec![src];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("src must be readable") {
                let path = entry.expect("readable dir entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path.starts_with(&commands) {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("readable");
                // Prefix only, no closing bracket: `#[tauri::command(rename_all = "…")]` is a
                // spelling Tauri documents, and matching the bare form would wave it through.
                if code_only(&source).contains(concat!("#[tauri::", "command")) {
                    stray.push(path.to_string_lossy().into_owned());
                }
            }
        }

        assert!(
            stray.is_empty(),
            "commands defined outside src/commands escape both tripwires: {stray:?}"
        );
    }
}
