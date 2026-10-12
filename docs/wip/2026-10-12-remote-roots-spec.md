# Remote roots over ssh — spec

Ephemeral: deleted by /finish. Cycle 3 of 3 for the Mac mini's re-encoding (dotfiles spec "ConvertBar moves from Unraid to the Mac mini", contract items 3–5). It builds on cycle 2 (preset import, container from the preset), whose output naming it uses unchanged. Cycle 1 is the signed server asset.

## Problem

The mini's headless server must re-encode files that stay on other machines: Unraid's DATA share now (ssh alias `unraid`, root, LAN) and the VPS's media later (`hetzner`). An SMB mount is ruled out (dotfiles ADR 0120). Jobs are picked by hand in the web UI, and the dotfiles tool `send` queues collections over the API. ConvertBar today only knows local paths, and every safety gate in its queue thread stats, reads, deletes or fingerprints `source_path`/`output_path` as local files.

## Solution

Remote roots configured by env (ADR 0002). A remote job pulls its source into local staging, runs the existing encode-and-decide part of the pipeline on the copy, and finalizes on the host: verify, take over owner/group/mode, then replace or publish-and-delete, only if the original is unchanged since the pull (ADR 0005). Transfers are ssh streams verified by SHA-256 (ADR 0006). An unreachable host books an `unreachable` error and pauses the queue (ADR 0003).

## Behavior

### Configuration

- `CONVERTBAR_REMOTE_ROOTS`: a comma-separated list of `host:dir`, e.g. `unraid:/mnt/user/DATA,hetzner:/srv/media`. `host` is an ssh host alias from the account's ssh config. It must match `[A-Za-z0-9._-]+` and must not start with `-`; anything else refuses startup, so no host can smuggle an ssh option such as `-oProxyCommand=…`. `dir` must be absolute. Several roots may share a host; a path is matched to a root by containment. Unset or empty means no remote roots, which is today's behaviour.
- `CONVERTBAR_STAGING_DIR`: the local staging directory, defaulting to a `staging` directory beside the database. It is created at startup and emptied at startup. Jobs run one at a time, and staging is emptied after every remote job, so it holds at most one job's source and result. The staged source keeps the remote file's basename.
- The desktop head has neither. Its behaviour is unchanged, apart from two new nullable job fields it ignores.
- ssh is the system `ssh` binary, run with the account's config plus `BatchMode=yes`, a connect timeout and a keepalive interval. The host always follows `--`, so it can never be read as an option. ssh never prompts.
- Remote hosts must provide a POSIX shell and GNU coreutils ≥ 9.5 (`stat -c` with `%.3Y`, `realpath -z`, `sha256sum`, `chown`/`chmod --reference`, `find -printf`, and `mv --update=none-fail`). Unraid has 9.11, probed 2026-10-12.

### Talking to the host safely

- Remote scripts are sent on stdin to `sh -s` with their arguments after it. The one exception is the push, whose stdin is the file: its fixed one-line script travels as an argument instead. Every path argument is single-quote-escaped, and no path is ever interpolated unescaped into a remote command.
- Every multi-path answer from the host (listings, the add-time stat, realpath) is NUL-delimited, so a name containing a newline or a quote cannot corrupt the parse.
- Every finalize script is wrapped in a function that is called only on its last line. A stream cut off part-way therefore runs nothing.
- Remote mtimes are taken with millisecond precision, the same unit as local fingerprints. The unchanged-since-pull check compares size and that mtime.

### Containment

- Every remote path a request names is resolved on the host (`realpath -e -z`). It must equal or lie under one of that host's configured root dirs (also resolved), compared by path components. This holds for browse, scan and add; finalize re-checks the stored row's paths.
- A path whose resolved form differs from the path as named (a symlink anywhere along it) is refused at add, so finalize can never rename over a link. Listings show symlinks as nothing.
- An unknown host, or a path outside every root of that host, is refused before anything else runs on the host.

### Browsing and queueing (web UI and API)

- `/api/info` lists the remote roots as `[{host, dir}]` beside `browse_roots`. The file browser offers them as additional roots.
- The browse, folder-scan, add-files and add-folder routes take an optional `remote` field naming the host alias. Paths are absolute paths on that host, e.g. `{"remote":"unraid","paths":["/mnt/user/DATA/x.wmv"]}`. Without `remote` everything is as today.
- **Adding never starts the queue**, for remote paths as for local ones today. A client adds and then starts the queue (`POST /api/converter/start`), as the web UI already does. This is the contract `send` builds against.
- A remote listing comes from one `find` round trip, which excludes marker temps.
- A remote add or folder add makes one round trip for every candidate file's size and mtime. Those set the job's original size and fingerprint, and feed the existing cheap skips:
  - not a video, or a marker temp;
  - already queued, keyed by host + path;
  - already converted, by fingerprint;
  - the suffix check.

  Skip-by-source-media is not applied at add time for remote paths; it runs after the pull.
- The output name follows cycle 2's rule (`{stem}{suffix}.{ext}`). Whether a candidate name is taken is checked on the host (one round trip per batch), against other job rows keyed by host + path, and within the batch, and never against the local disk. A taken name is renumbered as locally. An in-place job under `keep` is skipped at add, as locally.
- When the host is unreachable during browse, scan or add, the request fails with a message naming the host (502), and nothing is queued. An unknown host, a path outside the roots, or a symlinked path gets 400.

### One remote job

The queue thread branches for a remote row where today it runs the vanished-source check and the low-disk gate, both before the claim.

1. **Low-disk gate, before the claim**, on the staging volume, needing the configured floor plus twice the source size (the original size recorded at add). A shortfall stops the run exactly as today's low-disk stop does: the job stays `queued`, and the low-disk reason names the staging directory.
2. **Claim** as today.
3. **Pull:**
   - test for the source on the host. Only a definite "does not exist", from a parent directory that itself exists and is readable, books today's quiet "Source file no longer exists" environment error. Any other failure to look (permissions, a vanished parent) is an ordinary `environment` error with the host's message. This matches the local check's policy, where anything but a clean "not there" is not a vanished source.
   - stat the source: this is the pull baseline (size, mtime in milliseconds);
   - stream the source into staging, then compare the local size with the baseline;
   - re-stat. If the source changed during the pull, or the local copy is short, the job errors (`environment`) and the queue continues.

   Phase label: "Pulling…".
4. **Skip-by-source-media**, when the setting is on: probe the staged copy, with the same decision as at local add time. If the file cannot improve, book `skipped` with `kept_file = original`, no converted size, `space_saved = 0` and `skip_reason = already_at_target`. Fire the hook and continue with the next job: nothing is encoded or pushed. A per-file notification for such a job says it was already at the target, not "converted was larger".
5. **Encode and decide** on the staged copy: the HandBrake spawn, progress, the empty-output and truncation guards, and the keep-the-smaller decision. The output goes straight to a staging file named like the result, so there is no local in-place temp, whether or not the remote job is in place. Which paths each step reads:
   - **staged copy:** the spawn's input, the readability check behind failure classification, the original size compared against;
   - **remote baseline:** the condemned-identity stamp (bad source, truncated) and every fingerprint written. A stat of a staging file never becomes a fingerprint, because its mtime is the pull time;
   - **remote row paths:** the booked row, the hook payload, and the file name shown in the UI. They are never touched on the local disk.
6. **Original wins** (the result is not smaller): nothing is pushed. Book `skipped` as today, with `skip_reason` null.
7. **Result wins:** push the result into a marked temp beside the target, `.{output stem}.convertbar-tmp.{ext}`, overwriting any leftover with that name. Phase label: "Pushing…".
8. **Re-read the cleanup mode**, as today's success arm does. If it has become `keep` and the job is in place, discard the result as today's in-place-keep backstop does: remove the remote temp, delete the row, leave the original. Otherwise:
9. **Finalize**, one remote script. Phase label: "Verifying…".
   1. if the original's size or mtime differs from the pull baseline, fail with original and temp left;
   2. if the temp's size or SHA-256 differs from the local result, fail and remove the temp;
   3. give the temp the original's owner, group and mode (`--reference`); on failure, fail with original and temp left;
   4. in place: rename the temp over the original. New name: if the target exists, fail with original and temp left ("name taken"); otherwise move the temp to the target with `mv --update=none-fail` (refuses rather than overwrites if the name appears in between), then remove the original unless the cleanup mode is `keep`.

   The script exits with a distinct status per failure, so the message names what happened.
10. **Book** `done` as today. For an in-place job, the fingerprint becomes the remote stat (milliseconds) of the replaced file.
11. **Staging is emptied** after every remote job, whatever the outcome, by the queue thread.

The cleanup mode keeps its meaning. The server never uses `trash` (it is forced to `delete` at boot, as today). Under `keep`, a new-name result is published and the original kept; an in-place job is never queued.

### Unreachable host

- ssh's own exit status 255 at any remote step of a running job (test, stat, pull, push, finalize) means unreachable. That covers refused connections, timeouts, authentication and host-key failures.
- The job is booked `error` with failure class `unreachable` and a message naming the host and the step, through the notifying error path like any other failure; its post-convert hook fires. Staging is emptied. The queue then takes a persisted pause, set as a user pause is set, and the job's id is remembered as the pending re-queue, persisted the same way. The pass then ends the way the low-disk stop does: no "Queue complete" notification and no queue-drained hook, because it is not a drain.
- The UI shows a banner naming the host and the step. Its reason is held in memory like the low-disk reason, with a route and an event of its own. After a restart the banner is gone, while the pause, the remembered id and the error row remain.
- `/api/status` reads `stopped` while the pause holds (work queued, queue not running). No status value changes meaning.
- **The next queue start** (`POST /api/converter/start`, which the web UI's Start/Resume and API clients use, and which a local watched folder's intake also calls) re-queues the remembered job and forgets it. The job goes back to `queued` with its error message, failure class, `completed_at` and `started_at` cleared, keeping its place in the order. If its host + path already has an active row, it is left as an error instead. A re-queued job starts again from the pull. Clearing the queue also forgets the remembered id; the error row then stays in history.
- Accepted: a local watched folder's intake lifts the pause too, and runs the remote job first. On the mini there are no watched folders.
- Accepted edge: a connection that drops after the finalize script has completed is still booked as unreachable. An in-place job re-queued that way pulls its own result; skip-by-source-media, which the mini runs with, skips it, and with that setting off the file would be re-encoded once. A new-name job re-queued that way finds its source gone and books the quiet "no longer exists" error, although its result was published: the result is on the host, but no done row or hook for it exists, so the Stash rescan must come from Stash's own scan.

### Interruption, pause and cancel

- **Restart:** crash recovery reads each interrupted row's host. A remote row is re-queued (it starts again from the pull), and nothing is deleted locally for it: neither its `output_path` nor any path derived from it. Staging is emptied at startup. A leftover remote temp is overwritten by the next push.
- **Pause:** during a pull or push, the transfer's ssh process is the job's current child, so Pause stops it and Resume continues it (on unix, as for HandBrake). Between transfers (test, stat, probe, finalize: seconds to minutes) there is no child to stop, and Pause arms pause-after-current instead of doing nothing. A paused transfer whose connection has dropped by the time it resumes fails as unreachable.
- **Cancel:** the request handler only kills the current child, if any, and books the job as a cancel is booked today. It runs no ssh and deletes nothing locally, so the in-place-aware partial cleanup is skipped for remote rows. The queue thread's failure arm, which runs after a cancel, empties staging and removes the remote temp best-effort. While finalize runs, Cancel is refused with "finishing on <host>; try again in a moment". Finalize is short, and a script cut off part-way runs nothing, but one already past its last line must not be booked as cancelled.

### Phase visibility

The encoding row and the tray carry a phase label: "Pulling…", then HandBrake's own progress while encoding, then "Pushing…" and "Verifying…". There is no transfer percentage. Local jobs show no label.

### Bad sources and purge

A remote source condemned as bad appears in the bad-sources list with its host. Purge refuses it in its first, database-only step, before any local stat, with "delete by hand on <host>", and destroys nothing locally or remotely. The row stays in the review list.

### Hooks

- The post-convert and queue-drained payloads gain `remote_host` (null for a local job) and `skip_reason`. For a remote job, `source_path`, `output_path`, `result_path` and `output_dir` are the paths on that host. The path map applies to them as to any path.
- Command hooks receive `CONVERTBAR_REMOTE_HOST` and `CONVERTBAR_SKIP_REASON` through the existing scalar-to-environment rule.
- `failure_class` carries `unreachable` for the job that hit an outage.
- Because a re-queue clears `completed_at`, a re-queued row is not picked up by the queue-drained batch until it finishes again.

## Implementation decisions

- **Data.** `jobs` gains two nullable text columns, `remote_host` and `skip_reason`, as additive migrations. Both cross to the frontends in the job info and to the hooks in the payload. The remembered unreachable job id is a backend-only settings row, read with a default and never in the allowed settings keys, like `queue_paused`. No other schema change.
- **Context.** The context shared by both heads carries the remote roots, the staging directory, the ssh program, and the in-memory unreachable reason, all empty or defaulted, so existing construction sites need no change. The server sets them after building the context and before boot.
- **One module owns all ssh I/O:** listing, test/stat, the realpath containment, pull, push, finalize, and the classification of exit 255. The queue thread, the routes, cancel and recovery call it and never build a remote command themselves.
- **Staging.** The queue thread branches for a remote row ahead of the local vanished-source check and low-disk gate. It shares the spawn-to-decision part of the pipeline with local jobs, with the path roles listed in step 5, and replaces the local trash/delete/rename arms with push and finalize. No local gate is asked about a remote path (ADR 0005).
- **Dedupe keys.** The already-queued set and the "output name taken" check key remote rows by host + path, so a remote path never collides with an identical local path string.
- **Fingerprints.** Remote fingerprints go into the same path-independent identity set as local ones, in the same unit (milliseconds), as today's semantics already are.
- **Re-queueing** the remembered unreachable job happens inside the queue-start path, after it has claimed the queue slot (the claim-before-clear ordering stays as it is).
- **API.** New routes, and new fields on existing routes, are added to the route contract and the frontend transport, with the contract tests passing.
- **Docs.** The server documentation (README environment table, compose example, Unraid template) gains the two variables. The Unraid template does not advertise remote roots, because the container has no ssh setup.

## Testing decisions

The seams, the only places tests drive this cycle:

- **The remote seam.** The ssh program is swapped for a shim script that drops the options and the host and runs the remote script locally against temp directories. The shim can inject exit 255 at a named step, change the original mid-job, fail the chown step, or cut a stream short. The real remote scripts run under it, so these tests need GNU coreutils ≥ 9.5. They are ignored with that reason off Linux and run in the required Linux CI leg; the ignore count stays visible in the output.
- **The queue thread** (shim + the recording fake HandBrake scripts), one test per row:
  - in place replaces the original, keeping its mode;
  - a new name is published and the original deleted; under `keep`, the original is kept;
  - switching to `keep` mid-encode for an in-place job: the result is discarded, the row deleted, the original untouched;
  - original wins: nothing is pushed and the row is `skipped` with no `skip_reason`;
  - skip-by-media after the pull: `skipped`, `already_at_target`, nothing encoded;
  - the original changed since the pull: both left, and the message names the temp;
  - a size or SHA mismatch: the temp is removed and the original untouched;
  - a chown failure: both left;
  - the name was taken at publish: nothing overwritten;
  - a source the host cannot look at (permission denied) is an `environment` error, not "no longer exists";
  - the low-disk gate on staging leaves the job `queued`;
  - unreachable at the pull, the push and the finalize: class, persisted pause, remembered id, hook payload, staging emptied, and no queue-drained hook;
  - Start re-queues only the remembered job, with its timestamps cleared, and not when its host + path is active; clearing the queue forgets it;
  - a restart mid-job: recovery re-queues, empties staging and deletes nothing locally;
  - cancel during the pull: the ssh process is killed, nothing local is deleted, the queue thread empties staging; Cancel during finalize is refused;
  - a fingerprint written for a remote job is the remote stat, never a staging stat.
- **HTTP routes** (the injected-environment server):
  - remote add and folder add, including refusal outside the root, through a symlink, for an unknown host, and for an unreachable host;
  - add without start leaves the queue idle;
  - remote browse, including a name with a newline;
  - `/api/info` roots;
  - `/api/status` reading `stopped` after an unreachable stop;
  - purge of a remote bad source refused before any local stat;
  - the route-contract test for any new route.
- **Hook payload and command environment:** `remote_host` and `skip_reason` present, and null for local jobs.
- **Startup config:** each malformed `CONVERTBAR_REMOTE_ROOTS` entry refuses startup, including a host beginning with `-`.
- **Recorded artifact** (`#[ignore]`d, run by hand against `unraid` in a scratch directory under DATA): an `.mp4` replaced in place and a `.wmv` converted beside it with the original deleted. Remote `stat` shows owner, group and mode kept, ffprobe shows the tracks, and the hook payload is captured.

## Out of scope

- The signed server asset (cycle 1) and the preset/container work (cycle 2).
- Watched folders on remote roots.
- Purge over ssh.
- Resumable transfers and a transfer percentage.
- Keeping an encoded result across an unreachable pause.
- Remote roots in the desktop head.
- Non-GNU remote hosts.
- The dotfiles side: install.sh, the LaunchAgent and its env, the hook, the drainer, alerts, `send`.

## Open questions

- **Local network privacy on the mini is unverified there.** TN3179 says launchd agents are subject to it and the grant follows the code signature, hence the Developer ID-signed asset (ADR 0001). Still open: whether the alert appears for an ssh spawned by the agent, and that the first LAN ssh then needs someone at the GUI session to allow it. One live run on the mini after installing the signed asset settles it, and the dotfiles side should expect that one-time alert.
- **`mv --update=none-fail` on Unraid's user-share filesystem** has not been exercised (the probe was read-only). The recorded artifact's new-name case covers it before the dotfiles cutover.
- The remote shim tests do not run on macOS developer machines without GNU coreutils ≥ 9.5. Accepted; they run in the Linux CI leg.

## Contract notes for the dotfiles side

- Queueing is two requests: add (with `remote`), then `POST /api/converter/start`.
- An unreachable outage reads `stopped` on `/api/status`; the hook payload's `failure_class: unreachable` is the signal.
- `CONVERTBAR_REMOTE_ROOTS` and `CONVERTBAR_STAGING_DIR` go into the LaunchAgent's environment, not the API (ADR 0002).

## ADRs

- 0002: remote roots and the preset file are configured by env, not the API
- 0003: an unreachable host books an error and pauses the queue
- 0005: a remote job runs the local pipeline on a staging copy
- 0006: remote transfers are ssh streams verified by SHA-256
- 0001: the macOS server asset is Developer ID-signed and notarized (cycle 1; it is why the LaunchAgent's LAN access survives updates)
