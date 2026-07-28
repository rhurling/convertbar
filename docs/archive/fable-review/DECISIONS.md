# Open Decisions — Fable Review

Judgment calls that are product/process choices, not engineering fixes.
Referenced from [TRIAGE.md](TRIAGE.md) batches. Record the decision inline;
batches blocked on a decision stay parked until it's filled in.

## D1 — CSP policy (blocks B6)
`tauri.conf.json` ships `"csp": null` — no webview hardening at all. Any
script injection gets unrestricted `invoke()` access, including
`update_setting("handbrake_path", ...)` which points at a binary the app
executes.
- **Options:** (a) strict CSP `default-src 'self'; style-src 'self'
  'unsafe-inline'` + Tauri nonce injection (recommended; Vite self-hosted
  assets work, needs a manual smoke test of all pages); (b) keep null and
  accept the risk (defensible for a local-only menubar app, but weakest link
  compounds with any future ACL drift).
- **Recommendation:** (a).
- **Decision (2026-07-07):** (a) strict CSP + smoke test all pages.

## D2 — Windows PR CI (blocks B10 item 1)
Rust tests run on Windows only on pushes to main; Windows-only failures land
post-merge (has happened twice). Windows minutes bill 2×, suite is small,
rust-cache keeps it cheap.
- **Options:** (a) advisory windows-latest job on every PR (non-required —
  can't block merges, just visible); (b) same but gated on
  `paths: [src-tauri/**]` so frontend-only PRs skip it; (c) status quo.
- **Recommendation:** (b).
- **Decision (2026-07-07):** (b) advisory windows-latest PR job gated on paths: [src-tauri/**].

## D3 — Zero-byte output semantics (blocks B3 item 2)
A conversion whose process exits 0 but produced a 0-byte file is recorded
as done with a "saved 0B" notification.
- **Options:** (a) treat as failure (status=failed, partial file removed,
  error surfaced); (b) keep done but warn in notification/history.
- **Recommendation:** (a) — a 0-byte video is never a success.
- **Decision (2026-07-07):** (a) treat zero-byte output as failure.

## D4 — build.yml workflow_dispatch (blocks B9 item 3)
Manual dispatch from main is half-broken: `publish-release` runs
`gh release edit main`, guaranteeing a red run + stray unpublished draft.
- **Options:** (a) remove `workflow_dispatch` entirely — releases only via
  tags from release.sh (simplest, matches actual practice); (b) fix it to
  derive the tag from tauri.conf.json and skip publish on non-tag refs
  (keeps a manual "test the build" escape hatch).
- **Recommendation:** (a) unless you actually use manual dispatch to
  smoke-test builds.
- **Decision (2026-07-07):** (a) remove workflow_dispatch.

## D5 — Updater UX (blocks B4 item 5)
The updater silently downloads AND installs on every startup — no consent,
no notification, no relaunch prompt; errors swallowed; `unwrap()` can panic.
The error handling gets fixed regardless; the UX question:
- **Options:** (a) keep silent auto-install but notify "update installed,
  restart to apply" (plugin-notification already registered); (b) notify
  before installing and let the user defer; (c) keep fully silent.
- **Recommendation:** (a) — lowest friction, no more invisible updates.
- **Decision (2026-07-07):** (a) auto-install + "restart to apply" notification.

## D6 — Real-encoder e2e tests in CI (blocks B10 item 3)
Two valuable ffmpeg+HandBrakeCLI e2e tests are `#[ignore]`d; CI never runs
them, nothing enforces they still pass.
- **Options:** (a) scheduled weekly + on-main job installing
  HandBrakeCLI+ffmpeg (brew/apt) and running `cargo test -- --ignored`;
  (b) run them only in the release workflow as a pre-release gate;
  (c) leave manual.
- **Recommendation:** (a) — failures surface near the change, not at release.
- **Decision (2026-07-07):** (a) weekly scheduled + on-main ignored-tests job.

## D7 — Stop-hook version-sync warning (blocks B12 item 2)
`check-version-sync.sh` writes its warning to exit-0 stdout, which Stop
hooks only show in transcript mode — effectively invisible.
- **Options:** (a) exit 2 + stderr so Claude sees it and acts (may
  occasionally block a Stop on a false positive mid-release);
  (b) JSON `{"decision":"block","reason":...}` — same effect, structured;
  (c) keep as-is and document it's transcript-only.
- **Recommendation:** (a) — the hook exists to catch drift; invisible ≠ hook.
- **Decision (2026-07-07):** (a) exit 2 + stderr.

## D8 — rustfmt PostToolUse hook strategy (blocks B12 item 3)
The hook reformats entire edited files; the tree is not fmt-clean
(`cargo fmt --check` fails today), so edits to unclean files produce noisy
out-of-scope diffs (documented in project memory).
- **Options:** (a) run `cargo fmt` once in a standalone chore PR to make the
  tree clean, keep the hook as-is (one noisy PR, then the problem is gone
  forever); (b) gate the hook to skip files that aren't already fmt-clean
  (no noisy PR, but unclean files stay unclean and edits there go
  unformatted); (c) remove the hook.
- **Recommendation:** (a).
- **Decision (2026-07-07):** (a) one-time cargo fmt chore PR, keep hook.

## D9 — progress-event throttle (R6 "consider")
`process_queue`'s progress thread (converter.rs:586-635) emits a
`conversion-progress` + `menu-bar-update` pair for every parseable HandBrake
progress line, unthrottled.
- **Options:** (a) throttle emits (min interval or percent-delta gate);
  (b) drop it — leave as-is.
- **Recommendation:** (b) for R6. HandBrake's progress cadence is periodic
  (roughly per-second), not per-frame, so the "flood" is modest; a correct
  throttle touches the hot loop and needs an injectable clock to unit-test
  cleanly (it uses wall-clock `Instant`), which is out of scope for a mechanical
  Low batch. Revisit only if profiling shows the IPC rate actually matters.
- **Decision (2026-07-08):** (b) dropped from R6; not a correctness issue.

## D10 — probe_cache eviction (R6 "consider")
`probe_cache` (probe_cache.rs) has no eviction; rows accumulate over time.
- **Options:** (a) add eviction (age-based prune or row-count LRU cap);
  (b) drop it — leave as-is.
- **Recommendation:** (b). It is a persistent SQLite table `INSERT OR REPLACE`d
  per path (one row per unique file, not per probe), each row a handful of
  bytes; growth is bounded by the user's unique-file count and stale rows for
  deleted files are harmless. Eviction is a retention *policy* (with its own
  migration + tuning), not a mechanical fix, and there is no unbounded-memory or
  correctness problem to solve. Not this batch.
- **Decision (2026-07-08):** (b) dropped from R6; no leak, policy feature.

## D11 — canonicalization backfill for pre-fix watch rows (R7.1)
`canonical_watch_path` runs only on new inserts; watched_directories rows
written before the B5 fix keep verbatim paths, so re-adding the same folder
post-update can create a duplicate watcher.
- **Options:** (a) one-time init-time backfill rewriting each row to its
  canonical path, dropping a row that collides with an existing canonical one;
  (b) leave as-is (Low realism — pickers usually emit resolved paths).
- **Recommendation:** (a).
- **Decision (2026-07-08):** (a) one-time backfill migration in init-time Rust,
  UNIQUE-collision-safe.

## D12 — nested-watch purge overreach (R7.2)
Removing an enclosing root purges pending entries under a still-active nested
watch, silently dropping mid-stabilization files until a rescan.
- **Options:** (a) purge by desired-config coverage — retain any pending entry
  still matched by a desired config (`delay_for_path(&desired, path)`), instead
  of dropping everything under a removed root; (b) leave as-is (nested watches
  unusual).
- **Recommendation:** (a) — arguably simpler and fixes the overreach exactly.
- **Decision (2026-07-08):** (a) purge by "no desired config still covers it".

## D13 — uncancellable in-flight background scans (R7.3)
`scan_existing_background` enqueues via `enqueue_and_start`, bypassing `pending`,
so a watch removed mid-scan still gets its files enqueued.
- **Options:** (a) re-check current-config membership at the `enqueue_and_start`
  chokepoint (~5 lines; probe work still runs but nothing from a removed watch
  is enqueued; doubles as defense-in-depth); (b) generation counter that aborts
  the scan thread early (saves the probing too, more plumbing); (c) leave as-is.
- **Recommendation:** (a).
- **Decision (2026-07-08):** (a) config re-check at the enqueue chokepoint.

## D14 — by-name preset repair breadth (R7.4)
The init repair `UPDATE settings ... WHERE value='H.265 MKV 1080p'` also rewrites
a user's custom preset that happens to carry the old name.
- **Options:** (a) leave as-is; (b) gate/limit the repair (migration flag, or
  skip on Linux).
- **Recommendation:** (a) — a same-named custom preset can't be reliably told
  from the seeded bad default at init (before HandBrake's list is queryable);
  self-limiting (built-in exists, conversions work).
- **Decision (2026-07-08):** (a) leave as-is; act only on a real support report.

## D15 — updater install-failure notification (R8)
The startup auto-updater notifies on a successful install (D5) but a *failed*
`download_and_install` is fully silent — a persistently failing install leaves
the user invisibly stuck on an old version. (An offline *check* staying quiet is
correct and separate.)
- **Options:** (a) notify on install failure too (consistent with D5's "no
  invisible updates"), plus log; (b) log only; (c) keep silent.
- **Recommendation:** (a) — the D5 precedent is that update state shouldn't be
  invisible; a stuck-on-old-version failure is exactly what the user needs told.
- **Decision (2026-07-08):** (a) notify on install failure + `eprintln` log.

## D16 — ci-release #5 trap-based manifest restore (R8)
`bump_manifests` dirties the tree; only `build_app` restores it on failure. A
failure in `commit_release` (e.g. a signing error) after a successful build
leaves the bumped manifests dirty.
- **Options:** (a) add an EXIT-trap restore covering the whole bump→commit
  window (also de-duplicates build_app's inline restore); (b) keep as-is.
- **Recommendation:** (b) for this batch. The marginal gain over the existing
  build-failure restore is only the unlikely post-build commit/signing failure,
  and the dirty-tree outcome is recoverable (the next run's preflight flags a
  non-clean tree with a clear message). Critically, `release.sh` can't be driven
  past `preflight` (must be on main, clean, in sync with origin) in the CI test
  harness — which runs on a PR/detached checkout — so a trap addition can't meet
  this batch's failing-test-first bar without a test-only bypass hook in the
  production script. Not worth that trade for an optional, unlikely-failure edge.
- **Decision (2026-07-08):** (b) keep as-is; revisit only with a testability
  refactor (extract the bump→commit pipeline so cleanup is unit-testable).
