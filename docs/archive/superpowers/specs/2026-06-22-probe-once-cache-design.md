# Probe once per file — persist source-media probe results — Design

**Date:** 2026-06-22
**Status:** Approved design, ready for implementation plan
**Branch:** `feature/probe-once-cache` (worktree `.worktrees/probe-once-cache`)
**Extends:** `2026-06-21-skip-by-source-media-design.md`. That feature added
`probe_source()` and the pure skip policy; this one stops re-running the probe on files
whose content hasn't changed.

## Problem

`skip_by_source_media` shells out `HandBrakeCLI --scan --json` (`probe_source`,
`src-tauri/src/probe.rs`) to read a source's codec + height before queueing it. The
startup-hang fix (PR #47) narrowed probing to the cheap-skip survivors
(`probe_candidates`, `src-tauri/src/commands/queue.rs`), so a re-scan no longer probes
files that are already-queued, in history, suffix-matched, or whose separate output
already exists.

One case still re-probes on **every** scan/launch: the **in-place mp4 default**. With an
empty output suffix, `output_path == source_path`, so `cheap_skip_reason`
(`queue.rs`) never fires `OutputExists` (that branch is `!in_place`-gated), and unless the
separate `skip_already_converted` toggle is on, history doesn't catch it either. An
already-at-target mp4 therefore survives all cheap checks and pays a fresh 0–30s probe
every scan — for an answer that cannot change until the file's content changes. A watched
folder full of already-converted mp4s re-probes its whole contents on each launch
(background CPU, not a hang, after the startup-scan fix).

## Approach: memoize the probe, not the verdict

The skip *decision* (`should_skip_by_media`, `src-tauri/src/media_skip.rs`) is already
pure and is recomputed each scan against the **current** preset. The only expensive,
repeated step is the probe that produces `SourceMedia`. So we **memoize `probe_source`
keyed by file identity** and keep deciding skip/queue fresh each scan off the cached
media. Changing the preset re-evaluates correctly with no re-probe.

We memoize **all** successful probes (both at-target/skip and would-help/queue outcomes),
not just skips — one uniform path, no second decision to keep in sync.

## Identity model: `(path, size, mtime)`

A probe result is reused only when the file at `path` still has the same `size` **and**
`mtime` it had when probed. This single rule resolves every correctness trap the
maintainer raised, because **any** identity change forces an honest re-probe — we never
have to infer *why* a file changed.

| Scenario | Cache key behaviour | Outcome |
|---|---|---|
| Re-scan, file untouched | `(path, size, mtime)` all match | **hit → no probe** (the win) |
| Same path, **different file** dropped later | size and/or mtime differ | miss → re-probe → handled on its merits (no false skip) |
| In-place re-encode rewrote the file | our write changed size + mtime | miss → **one** re-probe → re-cache as at-target → skip thereafter |
| Convert in-place, original trashed, **new file moved onto that path** | new file's size/mtime ≠ cached | miss → re-probe → handled on its merits |
| Probe failed / unknown / not a video | result **not cached** (see Uncertainty) | re-probed next scan; never skipped |

A wrong reuse requires a different file to match **both** size and mtime exactly —
effectively impossible for distinct content, and its only cost is "a file that could have
been shrunk isn't," never data loss.

### Why `converter.rs` needs no changes (the trap-3 resolution)

The brief proposed recording the output's post-conversion identity at convert time, to
"tell our writes apart from the user's." Under this memoization framing that is
unnecessary. After an in-place encode the file's size + mtime change → next scan misses →
**one** honest re-probe reports the new (at-target) media → it caches and skips
thereafter. We never distinguish our writes from the user's because *any* change
re-probes truthfully. The cost is exactly **one re-probe per file after each in-place
conversion** (versus every scan today).

Proactively writing the cache at convert finish to eliminate that one probe was rejected:
the converter would have to *approximate* the output height (the preset cap, since it
doesn't know the real source height), and that approximation goes subtly wrong if the
user later switches to a smaller-resolution preset. **Decision: lazy re-probe; leave
`converter.rs` untouched.** Simpler and strictly correct.

## Data model

New table, created in `init_db` (`src-tauri/src/db.rs`). Rows are written **only** when
`skip_by_source_media` is on, so the feature is inert when the toggle is off.

```sql
CREATE TABLE IF NOT EXISTS probe_cache (
    path      TEXT PRIMARY KEY,
    size      INTEGER NOT NULL,
    mtime     INTEGER NOT NULL,   -- epoch millis from metadata.modified(), read identically each scan
    codec     TEXT NOT NULL,      -- normalized slug, same vocabulary as SourceMedia.codec
    height    INTEGER NOT NULL,
    probed_at TEXT NOT NULL       -- rfc3339; hygiene / debugging only
);
```

`path` is the primary key, so a re-probe **upserts** (one row per distinct path ever
seen — bounded). The row stores raw `SourceMedia`; the pure policy interprets it each
scan.

**mtime representation.** `std::fs::metadata(path).modified()` → `SystemTime` →
`duration_since(UNIX_EPOCH)` → `i64` millis. If `modified()` is unavailable or errors
(rare, platform/filesystem-dependent), we cannot form an identity → treat it as a forced
miss: probe, and do **not** cache. Both write (at probe time) and read (at lookup time)
use the same conversion so values compare exactly.

## Architecture & data flow

Two layers, each independently testable — mirroring the codebase's "pure core + thin I/O
shell" pattern (`decide_cleanup`, `should_skip_by_media`).

**DB layer — `src-tauri/src/probe_cache.rs` (new):**
- `lookup_batch(conn, &[(path, size, mtime)]) -> (hits: Vec<(String, SourceMedia)>, misses: Vec<(path, size, mtime)>)`
  — a hit requires the stored `size` **and** `mtime` to equal the supplied ones.
- `store_batch(conn, &[(path, size, mtime, SourceMedia)])` — upsert by `path`.

**Pure orchestration — `resolve_media(candidates_with_identity, lookup_fn, probe_fn, store_fn) -> Vec<(String, Option<SourceMedia>)>`:**
generic over closures, no DB or filesystem types. It splits hits from misses via
`lookup_fn`, calls `probe_fn` on **misses only**, sends successful probes to `store_fn`,
and returns the `(path, Option<SourceMedia>)` list `select_media_skips` already consumes.
Candidates whose identity couldn't be read (stat failure) are passed through as forced
misses that are probed but never stored.

**Wiring in `add_files_inner` (`queue.rs`)** — preserves "the expensive probe runs
outside the DB lock":
1. *Outside lock:* `stat` each probe-candidate → `(path, size, mtime)` (or `None` on stat
   failure).
2. *Under lock (brief):* `probe_cache::lookup_batch` → hits vs misses.
3. *Outside lock:* `probe_source` the misses only.
4. *Under lock (brief):* `probe_cache::store_batch` the successful probes; assemble the
   `(path, media)` list and hand it to `select_media_skips` as today.

The replaced code is the current eager probe loop in `add_files_inner` (the
`candidates_to_probe.iter().map(|p| (p, probe_source(hb, p)))` block). `probe_source`,
`media_skip.rs`, and the cheap-skip path are untouched.

## Uncertainty: never cache a non-answer

A probe that returns `None` (launch failure, timeout, unknown codec, not a video) is
**not** stored. The file is queued (never skipped — the core invariant from the
skip-by-source-media spec) and re-evaluated on the next scan. Caching `None` would only
suppress re-probes of pathological files without changing that they are queued anyway, so
we keep "uncertainty = re-evaluate."

## Cache hygiene

Path-keyed upsert bounds the table at one row per distinct path ever probed. Rows for
files later deleted/renamed linger as harmless dead weight (a deleted file is never a scan
candidate, so its row is never read). **No active pruning in v1** (YAGNI); a sweep of rows
whose `path` no longer exists is a possible later maintenance step, noted under
Follow-ups.

## Migration

Additive `CREATE TABLE IF NOT EXISTS probe_cache` in `init_db`. Existing databases gain an
empty table on next launch; no data migration, no rewrite of `jobs`. **No new setting** —
the cache is internal plumbing gated by the existing `skip_by_source_media` key, so the
seeded-settings count test (`init_db_seeds_defaults`, asserts 14) is untouched. To verify
with the `sqlite-migration-reviewer` agent during implementation.

## Cross-platform

`SystemTime` + `metadata.modified()` and the SQLite table work identically on
macOS/Windows/Linux — no `cfg` gating. No frontend Tauri API call is added, so **no new
ACL permission** (`capabilities/default.json` unchanged); confirm with the `acl-auditor`
agent. No UI changes.

## Testing

- **`probe_cache` DB layer** (in-memory `Connection`): `store_batch` then `lookup_batch`
  returns a hit; a hit requires matching `size` **and** `mtime` (miss when either
  differs); a `path` with no row is a miss; upsert overwrites a stale row.
- **`resolve_media` memoization invariant** (in-memory fakes for lookup/store + a
  call-counting `probe_fn`): probe is called exactly once per miss and **zero** times for
  a hit; a `None` probe is not stored; a stat-failure identity is probed but not cached.
  These encode the *why* (Rule 9): "we never re-probe a file whose content is unchanged."
- **Identity scenarios** as table cases over `resolve_media` / `lookup_batch`: untouched
  re-scan (hit), same-path different file (miss), in-place rewrite changes size+mtime
  (miss → re-probe), convert-then-replace-at-path (miss).
- The existing ignored end-to-end test
  (`add_files_inner_skips_at_target_source_end_to_end`, `queue.rs`) still passes; a second
  ignored pass over the same input should now record **zero** probes on the repeat.
- `cargo test --lib` green; `rustfmt` clean (note: `queue.rs` is heavily touched and the
  format hook reformats whole files — make surgical edits per project convention).

## Out of scope / follow-ups

- **Proactive cache write at convert finish** — eliminate the one post-conversion
  re-probe. Rejected for v1 (approximate output height; see trap-3 resolution). Revisit
  only if that single re-probe is ever shown to matter.
- **Stale-row pruning** — delete `probe_cache` rows whose `path` no longer exists.
  Harmless to omit; bounded growth.
- **Parallel probing** — still sequential (inherited from the skip-by-source-media spec).
  Memoization reduces the *number* of probes; it does not parallelize the remaining
  misses.
</content>
</invoke>
