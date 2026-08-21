//! The environment-variable source for command hooks — `CONVERTBAR_POST_CONVERT_COMMAND` and
//! `CONVERTBAR_QUEUE_DRAINED_COMMAND`.
//!
//! This is the ONLY way to configure a command hook on the server head, i.e. the whole
//! Docker/Unraid deployment the feature was built for, and nothing else in the suite touched
//! that branch of `read_hook_config`: a wrong variable name or an inverted emptiness filter
//! would have shipped green.
//!
//! **Why a separate integration-test binary, and why ONE test.** `std::env::set_var` is
//! process-global while cargo's harness runs tests multi-threaded. Setting either variable
//! inside the lib-test binary would hand a command hook to every `converter::tests` hook test
//! running concurrently, several of which assert on exactly how many commands were run; an
//! integration test file is its own process, so nothing here can reach those. The same hazard
//! then applies between tests in THIS file — a first draft split this in two and the split
//! failed immediately, the post-convert test's variable leaking into the queue-drained test's
//! cross-check. So it is one test covering every case sequentially, which needs no mutex and
//! no `--test-threads` flag that a future reader could drop without noticing.

use convertbar_core::hooks::{read_hook_config, Trigger};
use rusqlite::Connection;

const PC_VAR: &str = "CONVERTBAR_POST_CONVERT_COMMAND";
const QD_VAR: &str = "CONVERTBAR_QUEUE_DRAINED_COMMAND";

fn seeded_db() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory db");
    convertbar_core::db::init_db(&conn).expect("schema");
    for (key, value) in [
        ("post_convert_command", "/stored/post-convert.sh"),
        ("queue_drained_command", "/stored/queue-drained.sh"),
    ] {
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .expect("seed the stored command row");
    }
    conn
}

fn command(db: &Connection, trigger: Trigger, allow_stored_command: bool) -> String {
    read_hook_config(db, trigger, allow_stored_command).command
}

/// Removes `var` even if the body panics, so one failing assertion cannot leak the variable
/// into the assertions that follow it.
fn with_env<T>(var: &str, value: &str, body: impl FnOnce() -> T) -> T {
    std::env::set_var(var, value);
    // AssertUnwindSafe: a body only reads an immutable Connection and asserts.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    std::env::remove_var(var);
    match result {
        Ok(v) => v,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[test]
fn command_hook_env_vars_are_honoured_and_beat_the_stored_row() {
    let db = seeded_db();

    // Baseline with no variable set: the stored row is what a permissive head reads. Without
    // it, "the environment won" could equally be "the row was never read either way".
    assert_eq!(
        command(&db, Trigger::PostConvert, true),
        "/stored/post-convert.sh"
    );
    assert_eq!(
        command(&db, Trigger::QueueDrained, true),
        "/stored/queue-drained.sh"
    );

    with_env(PC_VAR, "/env/post-convert.sh", || {
        // Honoured at all — the branch nothing else in the suite reached, and the only way a
        // container gets a command hook (`allow_stored_command` is false on the server head).
        assert_eq!(
            command(&db, Trigger::PostConvert, false),
            "/env/post-convert.sh",
            "the server head configures a command hook ONLY this way"
        );
        // And it wins over a stored row even where the row IS an accepted source, so an
        // operator's compose file is not silently overridden by a copied convertbar.db.
        assert_eq!(
            command(&db, Trigger::PostConvert, true),
            "/env/post-convert.sh",
            "the environment wins over the stored row"
        );
        // The other trigger reads its OWN variable, unset here, so it falls back to its own
        // row: proof the two hand-written literals in `Trigger::command_env_var` are not
        // crossed, which a copy-paste would otherwise leave invisible.
        assert_eq!(
            command(&db, Trigger::QueueDrained, true),
            "/stored/queue-drained.sh",
            "post-convert's variable must not configure queue-drained"
        );
    });

    with_env(QD_VAR, "/env/queue-drained.sh", || {
        assert_eq!(
            command(&db, Trigger::QueueDrained, false),
            "/env/queue-drained.sh"
        );
        assert_eq!(
            command(&db, Trigger::QueueDrained, true),
            "/env/queue-drained.sh"
        );
        assert_eq!(
            command(&db, Trigger::PostConvert, true),
            "/stored/post-convert.sh",
            "queue-drained's variable must not configure post-convert"
        );
    });

    // The variables are gone again, so a later assertion cannot pass on a leftover value.
    assert_eq!(
        command(&db, Trigger::PostConvert, true),
        "/stored/post-convert.sh"
    );

    // Blank is not a configuration: it must fall through to the row rather than reading as
    // "a command is set". `is_off` would mask the empty string, but not whitespace — that
    // would spawn an empty command and fail on every single conversion.
    with_env(PC_VAR, "   ", || {
        assert_eq!(
            command(&db, Trigger::PostConvert, true),
            "/stored/post-convert.sh"
        );
        // And on the server head, where the row is refused, a blank variable means "no hook",
        // never the row.
        assert_eq!(command(&db, Trigger::PostConvert, false), "");
    });
}
