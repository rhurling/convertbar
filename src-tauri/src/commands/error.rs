//! The failure shape every command shares, and the one helper allowed to build its panic variant.
//!
//! This lives in its own module so that a command module cannot construct it by hand. Rust
//! privacy reaches *descendants*: with these fields declared in `commands`, `commands::queue`
//! could write `CommandError { error: …, kind: Some("panic") }` — mislabelling in either
//! direction — and the tripwire, which reads source text rather than types, would not see it. As
//! a sibling module, `commands::queue` can name the type and cannot fill it in.

use serde::Serialize;

const PANIC: &str = "panic";

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
#[derive(Debug, Serialize)]
pub struct CommandError {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
}

/// Lets a command body `?` straight through on a core `Result<_, String>`: every deliberate
/// failure in this crate is already a `String`, and all of them mean the same thing here.
///
/// Note what this cannot see: a `PoisonError` stringified upstream arrives as an ordinary
/// failure, so a panic's *aftermath* still reads as deliberate even though the panic itself
/// would not. That limit is recorded in RECOMMENDATIONS item 16 rather than papered over.
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
/// The panic detail stays in the message — the payload never leaves the machine on this head, so
/// debuggability wins over withholding it.
pub async fn blocking<T, F>(f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(result) => result.map_err(CommandError::from),
        // Worded identically to the server head's `join_err`, so a panic reads the same on both.
        Err(join) => Err(CommandError {
            error: format!("task panicked: {}", panic_detail(join)),
            kind: Some(PANIC),
        }),
    }
}

/// The message the task actually panicked with, rather than the runtime's description of it.
///
/// `JoinError`'s own `Display` is `task 12 panicked with message "boom"`, where the number is a
/// tokio task counter: the same crash reads differently on every run, so two users reporting one
/// bug produce two strings that do not group. Reaching past it to the payload also drops a second
/// "panicked" from a message the frontend already labels as a panic.
fn panic_detail(error: tauri::Error) -> String {
    let join = match error {
        tauri::Error::JoinError(join) => join,
        // `spawn_blocking` cannot fail any other way; if that ever changes, say what happened
        // rather than claiming a panic detail we do not have.
        other => return other.to_string(),
    };

    match join.try_into_panic() {
        // `panic!("literal")` yields a `&str`; anything formatted yields a `String`.
        Ok(payload) => payload
            .downcast_ref::<&'static str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a non-string panic payload".to_string()),
        // Not a panic, so the task was cancelled. Nothing here aborts a handle — every one is
        // awaited inline at its own call site — so this is runtime shutdown, and reporting it as
        // a panic is accepted: the frontend's conclusion (a bug, not a condition to handle) is
        // the same either way.
        Err(join) => join.to_string(),
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

        // Pinned whole rather than by prefix: the payload must survive (it is the only thing
        // that makes the bug reportable) and the runtime's task id must not (it changes every
        // run, so the same crash would never group across two bug reports).
        assert_eq!(
            panicked,
            serde_json::json!({ "error": "task panicked: boom", "kind": "panic" })
        );
    }

    #[test]
    fn a_formatted_panic_keeps_its_message() {
        // `panic!("{}", x)` boxes a String where `panic!("literal")` boxes a &str; reading only
        // one of the two would silently degrade half of all real panics to the fallback text.
        let detail = tauri::async_runtime::block_on(blocking(|| -> Result<(), String> {
            panic!("{} went wrong", "something")
        }))
        .expect_err("a panicking task must fail");

        assert_eq!(
            serde_json::to_value(&detail).expect("serializable")["error"],
            "task panicked: something went wrong"
        );
    }
}
