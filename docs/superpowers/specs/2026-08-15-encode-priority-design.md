# Encode CPU Priority — Design

## Problem

A HandBrake encode saturates every core it is given. On the desktop head that
means a menu-bar app whose entire premise is "leave it running in the
background" makes the machine unpleasant to use while it does so. There is no
knob for this today: the only lever ConvertBar exposes is pausing, which is
all-or-nothing.

The server head has an answer already — `--cpu-shares` on the container — but
that answer does not exist on a desktop, and it does not exist for a server head
run outside a container either (a plain binary under systemd on a VPS or NAS).

What is wanted is proportional share, not a ceiling: yield to whatever else the
user is doing when the CPU is contended, and still use the whole machine when
nothing else wants it. That is precisely what process niceness expresses, which
makes it the in-process equivalent of `--cpu-shares`.

## Goal

One setting, `encode_priority`, that lowers the scheduling priority of the
HandBrake child process. Available on both heads, defaulting to the behavior
each head's users already have, and honest in the UI about the two things it is
not: a CPU cap, and a live control over the running job.

## Decisions (settled with the user)

| Topic | Decision |
|---|---|
| Levels | Three: `normal` / `low` / `idle`. Not a raw nice integer — the scale is meaningless to users and does not exist on Windows |
| Semantics | Proportional share, never a cap. An idle machine is still fully used |
| Default, desktop | `low` on **fresh installs only**. Existing installs keep `normal` |
| Default, server head | `normal`, fresh or existing |
| Why the split | The desktop shares a machine with the user's actual work; a server head's box usually exists to encode, and has `--cpu-shares` besides |
| Why fresh-only | The defaults list is applied with `INSERT OR IGNORE` on every boot, so a new key reaches existing databases too. An auto-update must not silently change how fast anyone's encodes run |
| Mid-encode change | Applies to the **next** encode. The running child keeps the priority it was spawned with |
| Why not renice live | A non-root process may lower its priority but not raise it (`RLIMIT_NICE`), so `idle → normal` would fail on a running child while `normal → idle` succeeded. A one-way setting is worse than a next-job-only one |
| Exposed on server head? | Yes. Not the same category as the settings the server hides |
| Docker caveat | Surfaced as a note in the server head's UI, shown only when the process is detected to be containerized |
| Unrecognized value | Normalizes to `normal`, mirroring `normalize_cleanup_mode` |

## Why the Docker note exists

Under Docker the setting is close to a no-op, and the reason is worth recording
because it is not obvious: Linux CFS uses *group* scheduling. The container is
its own cgroup, and `nice` weights a task only **within** its cgroup. Nicing
HandBrake inside the container reorders it against the other processes in that
container — of which there is one, the ConvertBar server, sitting idle. It does
not make the container yield to the host's VMs, media server, or parity check.
`--cpu-shares` sets the container's weight in the parent hierarchy, which is
where the contention actually is.

The setting is still exposed on the server head because that head is a plain
binary. Run under systemd with no container, its processes sit in the root
cgroup and `nice` behaves exactly as it does on the desktop — and then the app
setting is the only knob there is.

This is a different category from the settings the server head already hides.
Menu bar, notifications, and launch-at-login are hidden because they are
*meaningless* there — no menu bar exists. Encode priority is meaningful on the
server; it is merely usually better solved one layer up.

## Rejected alternatives

- **Two levels (`normal` / `low`).** Simpler, but `idle` is genuinely different
  on macOS, where it maps to a background QoS class rather than a nice value.
- **A raw nice value, 0–19.** Maximum control over a scale most users cannot
  interpret, and one that does not exist on Windows — the mapping to priority
  classes would have to be invented anyway, making the precision partly
  fictional.
- **`low` as a global default for everyone.** Better out-of-box behavior, at the
  cost of every existing user's encodes silently slowing after an auto-update.
- **Renice the running child.** See the `RLIMIT_NICE` asymmetry above.
- **A static, always-shown Docker note.** No detection code and impossible to
  get wrong, but it makes every bare-metal server user read a caveat that does
  not apply to them.
- **Documenting the wrapper-script workaround instead of building anything.**
  Pointing `handbrake_path` at a `#!/bin/sh` / `exec nice -n 19 HandBrakeCLI
  "$@"` wrapper works today, but the `exec` is load-bearing in a way nobody
  should have to know: pause and cancel signal the direct child PID only
  (`converter.rs`), so a wrapper that forks instead of exec'ing would freeze the
  shell while HandBrake kept encoding, and cancel would orphan a live encode.

## Architecture

### Core: the setting

New settings key `encode_priority`, values `normal | low | idle`, read through
`settings_ops::read_encode_priority` returning an `EncodePriority` enum.
Absent-or-unrecognized normalizes to `normal`. This mirrors
`read_cleanup_mode`, and it is why an absent row is safe: `get_settings`
already seeds every field with an in-code default before overwriting it from
the query, so a missing key needs no special handling.

`Settings` gains `encode_priority: String`, normalized in `get_settings` the way
`cleanup_mode` is — not stored-raw-and-coerced-in-the-shell like `update_mode`,
whose raw storage exists because the coercion is desktop-updater policy. There
is no head-specific policy here, so one normalization point is enough and the UI
can never be handed a value it cannot render. `update_setting` validates on
write.

### Core: applying it

One call site — the `Command` builder in `converter::process_queue` that spawns
HandBrake.

| | `normal` | `low` | `idle` |
|---|---|---|---|
| Linux | nice 0 | nice 10 | nice 19 |
| macOS | nice 0 | nice 10 | `PRIO_DARWIN_BG` |
| Windows | `NORMAL_PRIORITY_CLASS` | `BELOW_NORMAL_PRIORITY_CLASS` | `IDLE_PRIORITY_CLASS` |

`PRIO_DARWIN_BG` is not merely nice 19: it places the process in a background
QoS class, which on Apple Silicon parks it on efficiency cores and throttles its
disk I/O. That is the intent of "idle" on macOS.

- Unix: `std::os::unix::process::CommandExt::pre_exec` calling
  `libc::setpriority`. `pre_exec` requires the closure be async-signal-safe;
  `setpriority` is a bare syscall, so it qualifies.
- Windows: `std::os::windows::process::CommandExt::creation_flags`. No new
  dependency — the priority-class constants are plain `u32`.

Both behind `#[cfg]` **attributes**, never the `cfg!()` macro. This is the
existing cross-platform rule in CLAUDE.md and it is load-bearing here for the
same reason it is for the SIGSTOP call sites: `cfg!()` only skips code at
runtime and would still require linking `libc` on every platform.

`normal` applies **nothing at all** — no `pre_exec`, no creation flag. Not an
optimization: it makes the default path byte-identical to today's spawn, which
is what lets the "existing users see no change" promise rest on the absence of
code rather than on a syscall that happens to be a no-op.

Values clamp to ≥ 0, so the code can never attempt a privileged raise and can
never hit `EPERM`. A failed `setpriority` is logged and the encode proceeds — a
priority hint must not fail a conversion.

Because this is a spawn-time attribute it is inherited by HandBrake itself and
leaves the child PID untouched, so pause (`SIGSTOP`), resume (`SIGCONT`), and
cancel (`Child::kill()`) keep signalling exactly the process they do today.

### Core: the per-head default

`db::init_db` changes its return type from `Result<()>` to
`Result<DbInit>`, where `DbInit` is `Fresh | Existing`, determined by probing
`sqlite_master` for the `settings` table *before* migrating. The ~30 existing
`init_db(&conn).unwrap();` call sites in tests continue to compile unchanged,
since they discard the value.

The desktop head seeds `encode_priority = low` only on `DbInit::Fresh`. The
server head seeds nothing and inherits `normal` from the normalizer. Core's
static defaults list does not gain an `encode_priority` entry — the whole point
is that the default is head-dependent, and core is head-agnostic.

### Server head: container detection

`AppInfo` in `routes/info.rs` gains `containerized: bool`, true when any of:

- `/.dockerenv` exists (Docker)
- `/run/.containerenv` exists (Podman)
- the `container` environment variable is set and non-empty

This trio covers Docker, Podman, and most LXC without parsing `/proc/1/cgroup`,
whose v1 and v2 layouts differ. Detection fails **safe**: anything ambiguous
reports `false` and shows no note, because a false "use `--cpu-shares`" on bare
metal steers someone away from a setting that would have worked for them.

The desktop head's `AppInfo` does not gain the field. Each head builds its own
struct, and the note is build-time gated to the server head anyway.

### Frontend

A three-option control in Settings, on both heads, with help text stating that
it applies to the next encode and is not a CPU cap.

The Docker note renders when `isServerHead && appInfo.containerized`. This
respects the gating rule stated in `src/lib/head.ts`: build-time `isServerHead`
for UI presence, runtime `getAppInfo()` for data. Which head is running is
build-time; whether that head sits in a container is runtime.

## Testing

The load-bearing test is behavioral, not a mock. A stub `HandBrakeCLI` shell
script records its own nice value and exits; the test points `handbrake_path` at
it — the pattern already used throughout `converter.rs` tests — runs a job per
tier, and asserts the recorded value. `#[cfg(unix)]`, since Windows priority
classes need a different probe.

Also:

- `read_encode_priority` normalization: absent row, empty string, unrecognized
  value, and each of the three valid values.
- `DbInit` reports `Fresh` for a new database and `Existing` for one that has
  already been initialized.
- The desktop head seeds `low` on `Fresh` and leaves an `Existing` database
  alone.
- The server info route reports `containerized`.
- The Docker note renders only when the flag is set, and never on the desktop
  build.

Then the mutation check required for behavior that can silently no-op: delete
the `setpriority` call and confirm the nice test goes red. A test that passes
with the feature removed is not a test of the feature.

## Non-goals

- **A CPU cap.** On an otherwise idle machine, encodes still use every core.
  Anyone wanting a ceiling needs `--cpus` or cgroup limits, which the app cannot
  set for itself without privileges.
- **Changing the running job's priority.**
- **Raising priority above normal.** Requires privileges, and there is no reason
  to want it.
- **I/O priority (`ionice`) as a separate control.** macOS `idle` gets I/O
  throttling for free via `PRIO_DARWIN_BG`; a separate Linux `ionice` knob is
  not justified by any stated need.
