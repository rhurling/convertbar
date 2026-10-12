# Remote roots over ssh — spec

Ephemeral: deleted by /finish. Cycle 3 of 3 for the Mac mini's re-encoding (dotfiles spec "ConvertBar moves from Unraid to the Mac mini", contract items 3–5). It builds on cycle 2 (preset import, container from the preset), whose output naming it uses unchanged. Cycle 1 is the signed server asset.

## Problem

The mini's headless server must re-encode files that stay on other machines: Unraid's DATA share now (ssh alias `unraid`, root, LAN) and the VPS's media later (`hetzner`). An SMB mount is ruled out (dotfiles ADR 0120). Jobs are picked by hand in the web UI, and the dotfiles tool `send` queues collections over the API. ConvertBar today only knows local paths, and every safety gate in its queue thread stats, reads, deletes or fingerprints `source_path`/`output_path` as local files.

## Solution

Remote roots configured by env (ADR 0002). A remote job pulls its source into local staging, runs the existing local pipeline on the copy, and finalizes on the host: verify, take over owner/group/mode, then replace or publish-and-delete, only if the original is unchanged since the pull (ADR 0005). Transfers are ssh streams verified by SHA-256 (ADR 0006). An unreachable host books an `unreachable` error and pauses the queue (ADR 0003).

## Behavior

### Configuration

- `CONVERTBAR_REMOTE_ROOTS`: a comma-separated list of `host:dir`, e.g. `unraid:/mnt/user/DATA,hetzner:/srv/media`. `host` is an ssh host alias from the account's ssh config. Several roots may share a host; a path is matched to a root by containment. Unset or empty means no remote roots, which is today's behaviour. A malformed entry (no colon, or a non-absolute dir) refuses startup.
- `CONVERTBAR_STAGING_DIR`: the local staging directory, defaulting to a `staging` directory beside the database. It is created at startup.
- The desktop head has neither. Its behaviour is unchanged, apart from two new nullable job fields it ignores.
- ssh is the system `ssh` binary, run with the account's config plus `BatchMode=yes`, a connect timeout and a keepalive interval. It never prompts.
- Remote hosts must provide a POSIX shell and GNU coreutils (`stat -c`, `realpath`, `sha256sum`, `chown`/`chmod --reference`, `find -printf`).
- Remote scripts are sent on stdin to `sh -s`. Every path argument is single-quote-escaped. No path is ever interpolated unescaped into a remote command line.

### Containment

- Every remote path a request names is resolved on the host (`realpath -e`). It must equal or lie under one of that host's configured root dirs (also resolved), compared by path components. This holds for browse, scan, add, and every finalize step, whose paths come from the stored row.
- An unknown host, or a path outside every root of that host, is refused before anything else runs on the host.

### Browsing and queueing (web UI and API)

- `/api/info` lists the remote roots as `[{host, dir}]` beside `browse_roots`. The file browser offers them as additional roots.
- The browse, folder-scan, add-files and add-folder routes take an optional `remote` field naming the host alias. Paths are absolute paths on that host, e.g. `{"remote":"unraid","paths":["/mnt/user/DATA/x.wmv"]}`. Without `remote` everything is as today.
- A remote listing comes from one `find` round trip. It excludes marker temps and lists symlinks as nothing (never queueable).
- A remote add or folder add makes one round trip for every candidate file's size and mtime. Those feed the existing cheap skips:
  - not a video, or a marker temp;
  - already queued, keyed by host + path;
  - already converted, by fingerprint;
  - the suffix check.

  Skip-by-source-media is not applied at add time for remote paths; it runs after the pull.
- The output name follows cycle 2's rule (`{stem}{suffix}.{ext}`). Whether a candidate name is taken is checked on the host (one round trip per batch), against other job rows keyed by host + path, and within the batch. A taken name is renumbered as locally. In place under `keep` is refused at add, as locally.
- When the host is unreachable during browse, scan or add, the request fails with a message naming the host (502), and nothing is queued. An unknown host or a path outside the roots gets 400.
- An add through the API with work behind it starts the queue as today, which lifts an unreachable pause (below).

### One remote job

1. **Claim** as today.
2. **Low-disk gate** on the staging volume, needing the configured floor plus twice the source size, as locally. A shortfall stops the run exactly as today's low-disk stop does.
3. **Pull:**
   - stat the source on the host. If it is gone, book the quiet "Source file no longer exists" environment error, as locally.
   - stream it into staging, then compare the local size with the stat;
   - re-stat. If the source changed during the pull, or the local copy is short, the job errors (`environment`) and the queue continues.

   The stat is the job's pull baseline (size, mtime).
4. **Skip-by-source-media**, when the setting is on: probe the staged copy, with the same decision as at local add time. If the file cannot improve, book `skipped` with `kept_file = original`, no converted size, `space_saved = 0` and `skip_reason = already_at_target`. Fire the hook and stop: nothing is encoded or pushed.
5. **The local pipeline** runs on the staged copy: the vanished-source check, encode, the empty-output and truncation guards, failure classification and the condemned-identity stamp, and the keep-the-smaller decision. A condemned remote source keeps the remote stat from the pull as its fingerprint, never a stat of the staging path.
6. **Original wins** (the result is not smaller): nothing is pushed. Book `skipped` as today, with `skip_reason` null.
7. **Result wins:** push the result into a marked temp beside the target, `.{output stem}.convertbar-tmp.{ext}`, overwriting any leftover with that name. Phase label: "Pushing…".
8. **Finalize**, one remote script, not interruptible:
   1. if the original's size or mtime differs from the pull baseline, fail with original and temp left;
   2. if the temp's size or SHA-256 differs from the local result, fail and remove the temp;
   3. give the temp the original's owner, group and mode (`--reference`); on failure, fail with original and temp left;
   4. in place: rename the temp over the original. New name: hard-link the temp to the target name (which fails if the name exists; then fail with original and temp left), remove the temp, then remove the original unless the cleanup mode is `keep`.

   Phase label: "Verifying…".
9. **Book** `done` as today. For an in-place job, the fingerprint becomes the remote stat of the replaced file.
10. **Staging is cleared** after every remote job, whatever the outcome.

The cleanup mode keeps its meaning. The server never uses `trash` (it is forced to `delete` at boot, as today). Under `keep`, a new-name result is published and the original kept; in place is never queued.

### Unreachable host

- ssh's own exit status 255 at any remote step of a running job (stat, pull, push, finalize) means unreachable. That covers refused connections, timeouts, authentication and host-key failures.
- The job is booked `error` with failure class `unreachable` and a message naming the host and the step, and its post-convert hook fires. Staging is cleared. The queue takes a persisted pause (as a user pause does) and the pass stops. The pass does not count as a drain, so no queue-drained hook fires.
- The UI shows a banner naming the host and the step. Its reason is held in memory like the low-disk reason, with a route and an event of its own. After a restart the banner is gone, while the pause and the error row remain.
- `/api/status` reads `paused`. No new state value is introduced.
- Start, Resume, or an add that starts the queue lifts the pause and re-queues every `unreachable` row (back to `queued`, error fields cleared), except one whose host + path already has an active row. A re-queued job starts again from the pull.
- Accepted edge: a connection that drops after the finalize script has completed is still booked as unreachable. An in-place job re-queued that way pulls its own result. Skip-by-source-media, which the mini runs with, skips it; with that setting off, the file would be re-encoded once.

### Interruption, pause and cancel

- **Restart:** crash recovery re-queues an interrupted remote row (it starts again from the pull) and clears staging. It never deletes a remote row's `output_path`, or any path derived from it, on the local disk. A leftover remote temp is overwritten by the next push.
- **Pause and cancel:** during a pull or push, the transfer's ssh process is the job's current child, so Pause stops it and Resume continues it (on unix, as for HandBrake), and Cancel kills it. Cancel clears staging, removes the remote temp best-effort, and books the job as a cancel is booked today. A paused transfer whose connection has dropped by the time it resumes fails as unreachable (above).

### Phase visibility

The encoding row and the tray carry a phase label: "Pulling…", then HandBrake's own progress while encoding, then "Pushing…" and "Verifying…". There is no transfer percentage. Local jobs show no label.

### Bad sources and purge

A remote source condemned as bad appears in the bad-sources list with its host. Purge refuses it with "delete by hand on <host>", and destroys nothing locally or remotely.

### Hooks

- The post-convert and queue-drained payloads gain `remote_host` (null for a local job) and `skip_reason`. For a remote job, `source_path`, `output_path`, `result_path` and `output_dir` are the paths on that host. The path map applies to them as to any path.
- Command hooks receive `CONVERTBAR_REMOTE_HOST` and `CONVERTBAR_SKIP_REASON` through the existing scalar-to-environment rule.
- `failure_class` carries `unreachable` for the job that hit an outage.

## Implementation decisions

- **Data.** `jobs` gains two nullable text columns, `remote_host` and `skip_reason`, as additive migrations. Both cross to the frontends in the job info and to the hooks in the payload. No other schema change.
- **Context.** The context shared by both heads carries the remote roots, the staging directory and the ssh program, all empty or defaulted, so existing construction sites need no change. The server sets them after building the context and before boot.
- **One module owns all ssh I/O:** listing, stat, the realpath containment, pull, push, finalize, and the classification of exit 255. The queue thread, the routes and recovery call it and never build a remote command themselves.
- **Staging.** The queue thread branches for a remote row before the local gates. From then on the local pipeline sees staging paths, and the remote finalize replaces the local trash/delete/rename arms. No local gate learns about remote paths (ADR 0005).
- **Dedupe keys.** The already-queued set and the "output name taken" check key remote rows by host + path, so a remote path never collides with an identical local path string.
- **Fingerprints.** Remote fingerprints go into the same path-independent identity set as local ones, as today's semantics already are.
- **Re-queueing** unreachable rows is one function called from Start, Resume and the add-start path. It never re-queues a row whose host + path is already active.
- **API.** New routes, and new fields on existing routes, are added to the route contract and the frontend transport, with the contract tests passing.
- **Docs.** The server documentation (README environment table, compose example, Unraid template) gains the two variables. The Unraid template does not advertise remote roots, because the container has no ssh setup.

## Testing decisions

The seams, the only places tests drive this cycle:

- **The remote seam.** The ssh program is swapped for a shim script that ignores the host argument and runs the remote script locally against temp directories. The shim can inject exit 255 at a named step, change the original mid-job, or fail the chown step. The real remote scripts run under it, so these tests need GNU coreutils. They are ignored with that reason off Linux and run in the required Linux CI leg; the ignore count stays visible in the output.
- **The queue thread** (shim + the recording fake HandBrake scripts), one test per row:
  - in place replaces the original, keeping its mode;
  - a new name is published and the original deleted; under `keep`, the original is kept;
  - original wins: nothing is pushed and the row is `skipped` with no `skip_reason`;
  - skip-by-media after the pull: `skipped`, `already_at_target`, nothing encoded;
  - the original changed since the pull: both left, and the message names the temp;
  - a size or SHA mismatch: the temp is removed and the original untouched;
  - a chown failure: both left;
  - the name was taken at publish: nothing overwritten;
  - unreachable at the pull, the push and the finalize: class, persisted pause, hook payload, staging cleared;
  - Resume re-queues without duplicating an active row;
  - a restart mid-job: recovery re-queues, clears staging and deletes nothing locally;
  - cancel during the pull: the ssh process is killed and staging cleared.
- **HTTP routes** (the injected-environment server):
  - remote add and folder add, including refusal outside the root, an unknown host, and an unreachable host;
  - remote browse;
  - `/api/info` roots;
  - `/api/status` reading `paused` after an unreachable stop;
  - the route-contract test for any new route.
- **Hook payload and command environment:** `remote_host` and `skip_reason` present, and null for local jobs.
- **Startup config:** each malformed `CONVERTBAR_REMOTE_ROOTS` entry refuses startup.
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
- The remote shim tests do not run on macOS developer machines without GNU coreutils. Accepted; they run in the Linux CI leg.

## ADRs

- 0002: remote roots and the preset file are configured by env, not the API
- 0003: an unreachable host books an error and pauses the queue
- 0005: a remote job runs the local pipeline on a staging copy
- 0006: remote transfers are ssh streams verified by SHA-256
- 0001: the macOS server asset is Developer ID-signed and notarized (cycle 1; it is why the LaunchAgent's LAN access survives updates)
