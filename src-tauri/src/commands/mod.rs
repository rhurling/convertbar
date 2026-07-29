//! The desktop head's IPC adapters. Every fallible `#[tauri::command]` in these modules fails
//! with [`CommandError`] — the desktop twin of the server head's `{"error": …, "kind": …}` body.

pub mod converter;
pub mod files;
pub mod handbrake;
pub mod queue;
pub mod settings;
pub mod updater;
pub mod watch;

use serde::Serialize;

/// The one shape a command failure takes on the way to the frontend.
///
/// `kind` is absent for a failure the backend means (HandBrakeCLI missing, an id that matches no
/// row) and `"panic"` when the blocking task died. Without it both arrive as the same opaque
/// string and the frontend cannot tell a bug in ConvertBar from a condition the user can act on —
/// the desktop half of `docs/RECOMMENDATIONS.md` item 16, which fixed the HTTP head first because
/// its JSON body already had room for the field and `Result<T, String>` here did not.
///
/// The field is absent rather than null on a deliberate failure, matching the server byte for
/// byte, so one frontend helper reads both heads.
///
/// Build the panic variant only through [`blocking`]. A second hand-written copy is exactly how
/// the wording diverges — nine of the server's ten sites agreed and the tenth quietly did not —
/// so `command_modules_never_map_their_own_blocking_failures` holds that door shut.
#[derive(Debug, Serialize)]
pub struct CommandError {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
}

/// Lets a command body `?` straight through on a core `Result<_, String>`: every deliberate
/// failure in this crate is already a `String`, and all of them mean the same thing here.
impl From<String> for CommandError {
    fn from(error: String) -> Self {
        Self { error, kind: None }
    }
}

/// Runs `f` on the blocking pool and gives its two failure modes different shapes: `f`'s own
/// `Err` is a failure the backend means, a `JoinError` means `f` panicked and is a bug.
///
/// Commands call this instead of writing the `spawn_blocking` match out themselves, which is what
/// makes the distinction enforceable rather than merely applied: a command that cannot write the
/// join arm cannot disagree with it.
///
/// The panic detail stays in the message, as it did before this existed — the payload never
/// leaves the machine on this head, so debuggability wins over withholding it.
///
/// A `JoinError` can in principle mean "cancelled" rather than "panicked". Nothing here aborts
/// these handles — every one is awaited inline at its own call site — so in practice this is
/// always a panic, and a runtime-shutdown cancellation would land in the same arm. Reporting both
/// as `panic` is accepted: the frontend's conclusion (a bug, not a condition to handle) is the
/// same either way.
pub async fn blocking<T, F>(f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(result) => result.map_err(CommandError::from),
        // Worded identically to the server head's `join_err`, so a panic reads the same on both.
        Err(join) => Err(CommandError {
            error: format!("task panicked: {join}"),
            kind: Some("panic"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deliberate_failure_and_a_panic_are_told_apart_by_kind() {
        // Both go through the same helper, so this is the distinction as a command actually
        // produces it — not two hand-built structs that happen to differ.
        let deliberate = tauri::async_runtime::block_on(blocking(|| -> Result<(), String> {
            Err("HandBrakeCLI not found".to_string())
        }))
        .expect_err("a failing task must fail");
        let panicked =
            tauri::async_runtime::block_on(blocking(|| -> Result<(), String> { panic!("boom") }))
                .expect_err("a panicking task must fail");

        let deliberate = serde_json::to_value(&deliberate).expect("serializable");
        let panicked = serde_json::to_value(&panicked).expect("serializable");

        // Absent, not null: the frontend's `kind === "panic"` test and the server's JSON body
        // both depend on the field simply not being there for an ordinary failure.
        assert_eq!(
            deliberate,
            serde_json::json!({ "error": "HandBrakeCLI not found" }),
            "a failure the backend means must carry no discriminator at all"
        );

        assert_eq!(panicked["kind"], "panic");
        let message = panicked["error"].as_str().expect("a message");
        assert!(
            message.starts_with("task panicked: "),
            "the panic message shape is shared with the server head, got {message:?}"
        );
        // The detail is the whole reason the message survives the mapping; a bare
        // "task panicked" would be honest about the kind and useless for fixing it.
        assert!(
            message.contains("boom"),
            "the panic payload must reach the frontend, got {message:?}"
        );
    }

    #[test]
    fn command_modules_never_map_their_own_blocking_failures() {
        // Comments are stripped before matching, so these checks read code rather than prose:
        // a module may well mention `spawn_blocking` in its docs precisely to say it no longer
        // calls it. Stripping also means the bare identifier can be matched, which a turbofish
        // (`spawn_blocking::<_>(`) would otherwise slip past.
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

        // Assembled rather than written out, so this test's own source does not read as a call
        // site — `mod.rs`'s count below would otherwise include the needles searched for here.
        let spawn = concat!("spawn_", "blocking");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
        // Only THIS file is exempt, matched by full path rather than by name: `Path::ends_with`
        // matches a trailing component, so a later `queue/mod.rs` would silently inherit an
        // exemption meant for the file holding the helper.
        let helpers = root.join("mod.rs");

        let mut checked = 0;
        let mut pending = vec![root.clone()];
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
                let source = code_only(&source);
                let module = path.file_name().expect("named file").to_string_lossy();

                assert!(
                    !source.contains("task panicked"),
                    "{module} builds the panic message itself; go through `blocking` so every \
                     join failure keeps one definition"
                );
                assert!(
                    !source.contains(spawn),
                    "{module} runs its own blocking task; call `blocking` instead, so the \
                     panic-vs-deliberate-failure distinction cannot be spelled a second way"
                );
                checked += 1;
            }
        }

        // `mod.rs` is exempt because the helper itself must spawn. Pin how many times, so a
        // command added to THIS file cannot quietly bring its own join arm.
        let helper_source =
            code_only(&std::fs::read_to_string(&helpers).expect("mod.rs must be readable"));
        assert_eq!(
            helper_source.matches(spawn).count(),
            1,
            "mod.rs should spawn in exactly one place (`blocking`) — a second means something \
             here maps a blocking failure without going through the helper"
        );

        // Guards the walk itself: a walk that matched nothing would pass every assertion above
        // while checking no module at all.
        assert!(
            checked >= 7,
            "expected to scan the command modules, only reached {checked} — did the walk break?"
        );
    }
}
