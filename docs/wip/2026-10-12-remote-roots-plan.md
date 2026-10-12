# Remote roots over ssh — plan

Ephemeral: deleted by /finish. Executes `docs/wip/2026-10-12-remote-roots-spec.md` (ADRs 0002, 0003, 0005, 0006).

Build-time facts that shape the slices:

- The remote-script tests need GNU coreutils ≥ 9.5 (`mv --update=none-fail`). GitHub's ubuntu-22.04 runner ships 8.32 and 24.04 ships 9.4, so the required `rust (ubuntu-22.04)` leg gets coreutils and findutils from the runner's preinstalled Homebrew. Locally they come from Homebrew's keg-only `gnubin`. The ssh shim puts a GNU bin directory first on `PATH`; a test that finds none fails loudly rather than skipping.
- The spec's open new-name edge is decided by its recommendation (standing go-ahead, 2026-10-12): a re-queued `unreachable` job whose source is definitely gone and whose stored output name exists on the host is booked `done`.

**1. One remote in-place job, added over HTTP.** Blocked by: nothing.
The server reads `CONVERTBAR_REMOTE_ROOTS` and `CONVERTBAR_STAGING_DIR` and refuses a malformed root. A client adds a file on a configured host with `remote` and starts the queue. ConvertBar refuses an unknown host, a path outside the roots and a symlinked path. It pulls the file into staging, encodes it, and pushes the smaller result under the marked temp name. It verifies size and SHA-256, takes over owner, group and mode, and renames over the original. It books `done` with the remote stat as the fingerprint, and staging ends empty. The low-disk gate looks at staging. Proven against the ssh shim running the real remote scripts.

— Checkpoint 1 —

**2. New names and every finalize refusal.** Blocked by: 1.
A new-name result is published beside the original, and the original is deleted, or kept under `keep`. An original that changed since the pull, a size or SHA mismatch, a failed chown and a name taken at publish each fail with a message naming what happened, and leave or remove files exactly as the spec says. An original that wins is booked `skipped` with nothing pushed. Switching to `keep` mid-encode discards an in-place result and deletes the row. A source the host cannot look at is an `environment` error, never "no longer exists". Every fingerprint is a remote stat.

**3. Skip-by-media after the pull, and the hook fields.** Blocked by: 1.
With the setting on, a staged copy that cannot improve is booked `skipped` with `skip_reason = already_at_target`, nothing encoded or pushed, and its notification says so. Jobs carry `remote_host` and `skip_reason` to the frontends, and the hook payloads and command environment carry both (null for local jobs).

**4. An unreachable host pauses the queue.** Blocked by: 1.
ssh exit 255 at the test, stat, pull, push or finalize books `error` with class `unreachable`, a message naming the host and the step, the hook fired, and staging emptied. The queue takes a persisted pause and remembers the job, without "Queue complete" or the drained hook. A banner reason is available by route and event. `/api/status` reads `stopped` or `idle`. The next start re-queues only that job, with its timestamps cleared, unless its host and path are active again. Clearing the queue forgets it. A re-queued new-name job whose source is gone and whose output exists is booked `done`.

**5. Restart, pause and cancel for remote jobs.** Blocked by: 1.
Recovery re-queues a remote row and deletes nothing locally, and startup empties staging. Pause freezes a running transfer and resumes it, and between transfers it arms pause-after-current. Cancel kills a transfer, deletes nothing locally, and leaves staging and the remote temp to the queue thread. Cancel during finalize is refused with "finishing on <host>".

— Checkpoint 2 —

**6. Browsing and folder adds.** Blocked by: 1.
`/api/info` lists the remote roots. The browse, folder-scan and add-folder routes take `remote`, read the host in one round trip each, and handle names containing newlines. An unreachable host answers 502 naming it, and an unknown host, an outside path or a symlink answers 400. A remote path never collides with an identical local path when deciding "already queued" or "name taken".

**7. The web UI and purge.** Blocked by: 4, 6.
The file browser offers the remote roots and queues from them. The encoding row shows "Pulling…", "Pushing…" or "Verifying…". The unreachable banner names the host and the step. A remote bad source shows its host, and purge refuses it before any local stat with "delete by hand on <host>".

**8. Documentation and the live run against Unraid.** Blocked by: 2, 3, 4, 5, 6.
The README environment table and the compose example document both variables, and the Unraid template does not. An ignored test, run by hand against `unraid` in a scratch directory under DATA, replaces an `.mp4` in place and converts a `.wmv` beside it with the original deleted. Remote `stat` shows owner, group and mode kept, ffprobe shows the tracks, and the hook payload is captured. This is the recorded artifact.

— Final review —

## Ledger
