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
    let value = match priority {
        EncodePriority::Normal => return Ok(()),
        EncodePriority::Low => 10,
        EncodePriority::Idle => 19,
    };

    // `as _` on each: `setpriority`'s first parameter is `c_int` on macOS but `c_uint` on
    // glibc, and `who` is `id_t`. Inference picks the right one per target.
    let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS as _, pid as _, value as _) };
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

    /// Spawns a real child, applies a tier, and reads the kernel's answer back — the effect
    /// is observed from the kernel, not inferred from the call returning Ok.
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
    #[cfg(unix)]
    fn idle_sets_nice_nineteen() {
        let mut child = spawn_sleeper();
        apply_to_child(child.id(), EncodePriority::Idle).unwrap();
        assert_eq!(nice_of(child.id()), 19);
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
