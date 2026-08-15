# Encode CPU Priority — Design

## Problem

A HandBrake encode saturates every core it is given. On the desktop head that
means a menu-bar app whose entire premise is "leave it running in the
background" makes the machine unpleasant to use while it does so. There is no
knob for this today: the only lever ConvertBar exposes is pausing, which is
all-or-nothing.

The server head has an answer already — `--cpu-shares` on the container — but
that answer does not exist on a desktop.

What is wanted is proportional share, not a ceiling: yield to whatever else the
user is doing when the CPU is contended, and still use the whole machine when
nothing else wants it. That is what process priority expresses, which makes it
the in-process equivalent of `--cpu-shares`.

## Goal

One setting, `encode_priority`, that lowers the scheduling priority of the
HandBrake child process. Available on both heads, defaulting to the behavior
each head's users already have, and honest in the UI about the three things it
is not: a CPU cap, a live control over the running job, and — on Linux —
reliably effective at all.

## Where this actually works

This is the single most important fact about the feature, and it is not
symmetric across platforms:

| Platform | Effective? |
|---|---|
| macOS | **Yes.** No cgroups, no autogroups. `idle` additionally buys efficiency-core placement and I/O throttling |
| Windows | **Yes.** Priority classes are system-wide |
| Linux | **Largely no.** Confined to a scheduling group — in a container *and* out of one |

The Linux limitation has two independent causes, and either alone is enough:

1. **Autogrouping.** With `sched_autogroup_enabled = 1` (the default on most
   distributions), the kernel places each session in its own scheduling group,
   and `sched(7)` states that a nice value then affects scheduling *only
   relative to other processes in the same autogroup*. HandBrake inherits
   ConvertBar's autogroup, so nicing it competes it against the idle ConvertBar
   process and nothing else.
2. **cgroup CPU controller.** Under Docker the container is its own cgroup;
   under systemd a service lives in `system.slice/<unit>.service`, not the root
   cgroup. Where the cpu controller is enabled on that path, nice is confined
   within it for the same reason.

So the honest scope is *Linux*, not *Docker*. An earlier draft of this design
attributed the caveat to containers alone and argued that a server head run bare
under systemd would behave like the desktop. That was wrong on both counts, and
it is recorded here because the mistake is easy to repeat: the natural mental
model of `nice` — a global dial — has not been accurate on Linux for over a
decade.

On Linux the tool that works is the one at the group level: `--cpu-shares` on a
Docker container, `CPUWeight=` on a systemd unit.

## Decisions (settled with the user)

| Topic | Decision |
|---|---|
| Levels | Three: `normal` / `low` / `idle`. Not a raw nice integer — the scale is meaningless to users and does not exist on Windows |
| Semantics | Proportional share, never a cap. An idle machine is still fully used |
| Default, desktop | `low` on **fresh installs only**. Existing installs keep `normal` |
| Default, server head | `normal`, fresh or existing |
| Why the split | The desktop shares a machine with the user's actual work; a server head's box usually exists to encode, and has `--cpu-shares` besides |
| Why fresh-only | The defaults list is applied with `INSERT OR IGNORE` on every boot, so a new key would reach existing databases too. An auto-update must not silently change how fast anyone's encodes run |
| Mid-encode change | Applies to the **next** encode. The running child keeps the priority it was spawned with |
| Why not renice live | A non-root process may lower its priority but not raise it (`RLIMIT_NICE`), so `idle → normal` would fail on a running child while `normal → idle` succeeded. A one-way setting is worse than a next-job-only one |
| Shown on Linux? | Yes, with a note. It is not a hard no-op there — autogrouping can be disabled, and a process with no cpu controller active on its path does get real host-wide nice |
| Note trigger | The **Linux build**, both heads. Not container detection |
| Unrecognized value | Normalizes to `normal`, mirroring `normalize_cleanup_mode` |

## Rejected alternatives

- **Two levels (`normal` / `low`).** Simpler, but `idle` is genuinely different
  on macOS, where it maps to a background QoS class rather than a nice value.
  The tiers do not collapse elsewhere either: Linux nice 10 vs 19 is a CFS
  weight of 110 vs 15, and Windows `BELOW_NORMAL` and `IDLE` are distinct
  classes.
- **A raw nice value, 0–19.** Maximum control over a scale most users cannot
  interpret, and one that does not exist on Windows — the mapping to priority
  classes would have to be invented anyway, making the precision partly
  fictional.
- **`low` as a global default for everyone.** Better out-of-box behavior, at the
  cost of every existing user's encodes silently slowing after an auto-update.
- **Renice the running child.** See the `RLIMIT_NICE` asymmetry above.
- **Detecting the container to target the note.** Discriminates on the wrong
  axis now that the caveat is known to be Linux-wide: it would stay silent for a
  Linux desktop user, where the setting is equally ineffective, and it costs
  `/.dockerenv` and `/run/.containerenv` probing plus Podman and LXC edge cases
  to answer a question that is not the one being asked.
- **Hiding the setting on Linux.** Honest, but it is not a hard no-op there, so
  this removes a working knob from the users whose systems do not confine it.
- **Setting the priority in `pre_exec`.** See "Applying it" — the parent-side
  call is simpler and testable.
- **Documenting the wrapper-script workaround instead of building anything.**
  Pointing `handbrake_path` at a `#!/bin/sh` / `exec nice -n 19 HandBrakeCLI
  "$@"` wrapper works today, but the `exec` is load-bearing in a way nobody
  should have to know: pause and cancel signal the direct child PID only
  (`converter.rs:284`, `:291`), so a wrapper that forks instead of exec'ing
  would freeze the shell while HandBrake kept encoding, and cancel would orphan
  a live encode. It also does nothing about the Linux grouping problem.

## Architecture

### Core: the setting

New settings key `encode_priority`, values `normal | low | idle`, read through
`settings_ops::read_encode_priority` returning an `EncodePriority` enum.
Absent-or-unrecognized normalizes to `normal`.

Two deliberate divergences from the `cleanup_mode` precedent it otherwise
follows:

- `read_cleanup_mode` returns `String`; `read_encode_priority` returns an enum,
  because its three values are consumed by a platform `match` rather than
  compared as strings, and the project prefers enums over string constants.
- No validation on write. `update_setting` validates the *key* against
  `ALLOWED_KEYS` and nothing else (`settings_ops.rs:191`) — no setting in this
  codebase validates its value on write, including `cleanup_mode`. The
  normalizer already makes an invalid stored value harmless, so adding
  write-side validation here would be a lone exception to a consistent pattern.

`Settings` gains `encode_priority: String`, normalized in `get_settings` the way
`cleanup_mode` is (`settings_ops.rs:141`) — not stored-raw like `update_mode`,
whose raw storage exists because the coercion is desktop-updater policy. There
is no head-specific policy here, so one normalization point is enough.

A missing row needs no special handling: `get_settings` already seeds every
field with an in-code default before overwriting it from the query
(`settings_ops.rs:104-125`), and every other reader goes through the
normalizer.

### Core: applying it

The spawn site is the `Command` builder in `converter::process_queue`
(`converter.rs:1004`). `handbrake_path` is already re-read each loop iteration,
so a per-encode priority read gives the "applies to the next encode" semantics
for free.

| | `normal` | `low` | `idle` |
|---|---|---|---|
| Linux | nice 0 | nice 10 | nice 19 |
| macOS | nice 0 | nice 10 | `PRIO_DARWIN_BG` |
| Windows | `NORMAL_PRIORITY_CLASS` | `BELOW_NORMAL_PRIORITY_CLASS` | `IDLE_PRIORITY_CLASS` |

The macOS `idle` cell is **not a nice value and does not use the same call**.
`PRIO_DARWIN_BG` is a flag passed with `which = PRIO_DARWIN_PROCESS`, where the
other two cells use `PRIO_PROCESS`. It places the process in a background QoS
class, which on Apple Silicon parks it on efficiency cores and throttles its
disk I/O — the reason `idle` exists as a separate tier.

**Unix: set from the parent, immediately after spawn** —
`libc::setpriority(which, child.id(), value)`. Not `pre_exec`. The parent-side
call needs no `unsafe`, requires no argument about async-signal-safety between
`fork` and `execve`, and avoids the open question of whether Darwin's background
task policy survives `execve` at all. It is also what makes the macOS test
possible: the parent can read the child's state back with
`getpriority(PRIO_DARWIN_PROCESS, pid)`. The cost is a few microseconds of
normal-priority startup, which is irrelevant for a hint that governs a
multi-minute encode.

**Windows: `CommandExt::creation_flags`** at spawn. No new dependency — the
priority-class constants are plain `u32` — and nothing in the codebase currently
sets `creation_flags`, so there is no existing flag to merge with.

Both behind `#[cfg]` **attributes**, never the `cfg!()` macro. This is the
existing cross-platform rule in CLAUDE.md and it is load-bearing here for the
same reason it is for the SIGSTOP call sites: `cfg!()` only skips code at
runtime and would still require linking `libc` on every platform.

`normal` applies **nothing at all** — no `setpriority`, no creation flag. Not an
optimization: it makes the default path byte-identical to today's spawn, which
is what lets the "existing users see no change" promise rest on the absence of
code rather than on a call that happens to be a no-op.

Failures are logged and the encode proceeds; a priority hint must not fail a
conversion. Two are expected in normal operation and are not bugs:

- `EACCES`/`EPERM` when ConvertBar itself is already running above nice 0 (a
  systemd `Nice=`, or launched under `nice`). `setpriority` sets an *absolute*
  value, so `low` is then a **raise**, which `RLIMIT_NICE` forbids — the same
  asymmetry that rules out live renicing, applied at spawn time.
- `ESRCH` if the child has already exited, e.g. HandBrake failing instantly on a
  bad input.

Because the priority is set on the spawned PID and never changes it, pause
(`SIGSTOP`), resume (`SIGCONT`), and cancel (`Child::kill()`) keep signalling
exactly the process they do today (`control.rs:77`, `:164`; `converter.rs:284`,
`:291`).

**Probes are deliberately excluded.** `probe.rs:95` (source `--scan --json`) and
`handbrake.rs:154`, `:171`, `:236` (version check, preset metadata) also spawn
HandBrakeCLI. They stay at normal priority: they are short, they run on paths a
user is actively waiting on (intake, settings), and delaying them would make the
UI feel slow to buy nothing measurable.

### Core: the per-head default

`db::init_db` changes its return type from `Result<()>` to `Result<DbInit>`,
where `DbInit` is `Fresh | Existing`, determined by probing `sqlite_master` for
the `settings` table *before* migrating.

There are **74 existing call sites** plus the definition. All of them —
including the three in production (`convertbar-server/src/main.rs:53`,
`server/src/routes/mod.rs:281`, `src-tauri/src/lib.rs:89`) — discard the value
via `.unwrap()`/`.expect()`, so they continue to compile untouched. This holds
only if `DbInit` is **not** `#[must_use]`; marking it would produce 74 warnings.

The desktop head seeds `encode_priority = low` only on `DbInit::Fresh`, at
`src-tauri/src/lib.rs:89`, which runs `init_db` inside `.setup()` before `Ctx`
is built and long before anything can spawn an encode. The server head seeds
nothing and inherits `normal` from the normalizer.

Core's static defaults list does **not** gain an `encode_priority` entry — the
whole point is that the default is head-dependent, and core is head-agnostic.

One accepted consequence: deleting `convertbar.db` — a real troubleshooting step
in this project's history — makes the next desktop launch a "fresh install" and
silently switches the user to `low`. This is not worth new state to defend
against; the setting's help text makes the current value visible, which is
enough.

### Heads: reporting the Linux caveat

`AppInfo` gains `priority_is_group_scoped: bool`, set from
`cfg!(target_os = "linux")` in **both** heads
(`convertbar-server/src/routes/info.rs:36`, `src-tauri/src/commands/converter.rs:57`).

This mirrors the existing `can_pause_process: cfg!(unix)` field exactly, and it
must be runtime data rather than a build-time frontend flag: the frontend bundle
is built per *head*, not per *OS*, so the desktop bundle runs on macOS, Windows,
and Linux alike and cannot know which from `import.meta.env`. That is precisely
the split `src/lib/head.ts` mandates — build-time `isServerHead` for UI
presence, runtime `getAppInfo()` for data.

`AppInfo` is a single shared TypeScript interface (`src/lib/transport/types.ts:210`)
implemented by both transports, so both must supply the field;
`src/lib/transport/tauri.ts` constructs the desktop one.

### Frontend

A three-option control in Settings, on both heads, with help text stating that
it applies to the next encode and is not a CPU cap.

When `appInfo.priority_is_group_scoped` is true, an additional note explains
that Linux confines priority to a process group and points at `--cpu-shares`
(Docker) and `CPUWeight=` (systemd).

## Testing

The load-bearing test observes the **child's actual priority from the parent**,
which the parent-side design makes straightforward: point `handbrake_path` at a
stub script that sleeps (the `fake_handbrake_script` + `set_setting` pattern at
`converter.rs:2162`, used around `:2047`), run a job per tier, and read the
child's priority back while it lives. That pattern bypasses the locator
entirely, so the `PanickingLocator` rule in CLAUDE.md does not apply
(`converter.rs:2802` documents that the configured-path branch never consults
it).

The probe must be per-OS, and this is the detail an earlier draft got wrong:

- Linux: `getpriority(PRIO_PROCESS, pid)` → 0 / 10 / 19.
- macOS: `getpriority(PRIO_PROCESS, pid)` for `normal` and `low`, but
  `getpriority(PRIO_DARWIN_PROCESS, pid)` for `idle`, which returns
  `PRIO_DARWIN_BG` and leaves the nice value at 0. A test that reads only the
  nice value would assert nothing for the one tier whose macOS behavior is the
  argument for having three tiers.

PR CI's rust matrix is ubuntu-only (macOS runs on `main`), so a Darwin-only
defect in this test lands after merge. The macOS assertions must therefore be
written from the documented `PRIO_DARWIN_*` contract rather than by iterating
against CI.

Also:

- `read_encode_priority` normalization: absent row, empty string, unrecognized
  value, and each of the three valid values.
- `DbInit` reports `Fresh` for a new database and `Existing` for one already
  initialized.
- The desktop head seeds `low` on `Fresh` and leaves an `Existing` database
  alone.
- Both heads report `priority_is_group_scoped` matching `cfg!(target_os = "linux")`.
- The Linux note renders only when the flag is set.

Then the mutation check required for behavior that can silently no-op: delete
the `setpriority` call and confirm the priority test goes red. A test that
passes with the feature removed is not a test of the feature.

## Non-goals

- **A CPU cap.** On an otherwise idle machine, encodes still use every core.
  Anyone wanting a ceiling needs `--cpus` or cgroup limits, which the app cannot
  set for itself without privileges.
- **Making it work on Linux.** Writing to `/proc/self/autogroup` would address
  autogrouping but not the cgroup case, and the group-level tools that do work
  (`--cpu-shares`, `CPUWeight=`) are outside the app's control by design. The
  note points at them instead.
- **Changing the running job's priority.**
- **Raising priority above normal.** Requires privileges, and there is no reason
  to want it.
- **Prioritizing the probe/scan spawns.** See "Probes are deliberately
  excluded".
- **I/O priority (`ionice`) as a separate control.** macOS `idle` gets I/O
  throttling for free via `PRIO_DARWIN_BG`; a separate Linux knob is not
  justified by any stated need, least of all on the platform where the CPU half
  already does not work.
