//! The desktop head's IPC adapters. Every fallible `#[tauri::command]` in these modules fails
//! with [`CommandError`] — the desktop twin of the server head's `{"error": …, "kind": …}` body.

mod error;

pub mod converter;
pub mod files;
pub mod handbrake;
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

    #[test]
    fn every_async_command_hands_its_work_to_the_blocking_helper() {
        // The tripwire above bans the *wrong* spellings; this one requires the right one.
        // Without it, a command can drop `blocking` for a direct synchronous call — which
        // compiles, names no banned identifier, and costs two things at once: the panic
        // taxonomy, and the off-main-thread guarantee that the probe-hazard fix bought at four
        // entry points (a deep folder scan would freeze the UI again).
        //
        // These three are async for a reason other than blocking work, so they are named here
        // rather than silently tolerated: `pick_folder` must stay async because
        // `blocking_pick_folder` dispatches to the main thread and would deadlock it, and the two
        // updater commands await genuinely async work.
        const NOT_BLOCKING_WORK: [&str; 3] = ["pick_folder", "check_for_update", "install_update"];

        let (modules, _) = command_modules();
        let mut checked = 0;
        for (module, source) in &modules {
            for chunk in source.split("pub async fn ").skip(1) {
                let name = chunk
                    .split(['(', '<'])
                    .next()
                    .expect("a command name")
                    .trim();
                if NOT_BLOCKING_WORK.contains(&name) {
                    continue;
                }
                // Stop at the next item so a later command's call cannot vouch for this one.
                let body = chunk.split("\n#[").next().unwrap_or(chunk);
                assert!(
                    body.contains("blocking("),
                    "{module}::{name} is async but never reaches `blocking`; either hand its \
                     work to the helper or add it to NOT_BLOCKING_WORK with a reason"
                );
                checked += 1;
            }
        }

        assert!(
            checked >= 8,
            "expected to scan the async commands, only reached {checked} — did the split break?"
        );
    }
}
