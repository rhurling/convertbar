//! Persistent memoization of `probe_source` results, keyed by file identity
//! `(path, size, mtime)`. A cached `SourceMedia` is reused only when the file still has the
//! same size AND mtime, so any content change — including our own in-place re-encode —
//! forces an honest re-probe. The pure skip policy (`media_skip`) still decides each scan.

use crate::media_skip::SourceMedia;
use rusqlite::{params, Connection};

/// A file's content identity: its byte size and last-modified time (epoch millis). A cached
/// probe is reusable only when BOTH match the file's current stat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub size: i64,
    pub mtime: i64,
}

/// Split `candidates` into cache hits and misses. A hit is `(path, media)` whose stored row
/// matches BOTH the supplied size and mtime; everything else (no row, or a stale row) is a
/// miss carrying the identity to re-probe and re-store.
pub fn lookup_batch(
    conn: &Connection,
    candidates: &[(String, FileIdentity)],
) -> (Vec<(String, SourceMedia)>, Vec<(String, FileIdentity)>) {
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    for (path, id) in candidates {
        match cached_media(conn, path, *id) {
            Some(media) => hits.push((path.clone(), media)),
            None => misses.push((path.clone(), *id)),
        }
    }
    (hits, misses)
}

/// Read the cached `SourceMedia` for `path`, but only if the stored size AND mtime equal
/// `id`. Returns `None` on no row, an identity mismatch, or any query error.
fn cached_media(conn: &Connection, path: &str, id: FileIdentity) -> Option<SourceMedia> {
    conn.query_row(
        "SELECT size, mtime, codec, height FROM probe_cache WHERE path = ?1",
        params![path],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        },
    )
    .ok()
    .filter(|(size, mtime, _, _)| *size == id.size && *mtime == id.mtime)
    .map(|(_, _, codec, height)| SourceMedia { codec, height })
}

/// Upsert each freshly probed `(path, identity, media)`, replacing any stale row for that
/// path. Cache writes are best-effort: a failure must never break adding files, so errors
/// are swallowed.
pub fn store_batch(conn: &Connection, probed: &[(String, FileIdentity, SourceMedia)]) {
    let now = chrono::Utc::now().to_rfc3339();
    for (path, id, media) in probed {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO probe_cache (path, size, mtime, codec, height, probed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![path, id.size, id.mtime, media.codec, media.height, now],
        );
    }
}

/// Memoize probes for `candidates`: reuse cached media for unchanged files, probe only the
/// rest, persist the fresh probes, and return the `(path, Option<SourceMedia>)` list that
/// `media_skip::select_media_skips` consumes. Generic over the three side effects so the
/// memoization behavior is unit-testable without a database or HandBrake.
///
/// Each candidate carries its `FileIdentity`, or `None` when its size/mtime couldn't be
/// read — those are forced misses: probed every time, never cached (no stable key).
///
/// Call order matters: `lookup` runs first (one batch), then `probe` per miss, then `store`
/// once. The wiring relies on this so the expensive probe never holds the DB lock.
pub fn resolve_media<L, P, S>(
    candidates: &[(String, Option<FileIdentity>)],
    lookup: L,
    probe: P,
    store: S,
) -> Vec<(String, Option<SourceMedia>)>
where
    L: Fn(&[(String, FileIdentity)]) -> (Vec<(String, SourceMedia)>, Vec<(String, FileIdentity)>),
    P: Fn(&str) -> Option<SourceMedia>,
    S: Fn(&[(String, FileIdentity, SourceMedia)]),
{
    // Files with no readable identity can't be cached — set them aside to always probe.
    let mut identified = Vec::new();
    let mut forced = Vec::new();
    for (path, id) in candidates {
        match id {
            Some(id) => identified.push((path.clone(), *id)),
            None => forced.push(path.clone()),
        }
    }

    let (hits, misses) = lookup(&identified);

    // Probe every cache miss and every identity-less file. Cache only the successes.
    let mut to_store = Vec::new();
    let mut probed: Vec<(String, Option<SourceMedia>)> = Vec::new();
    for (path, id) in &misses {
        let media = probe(path);
        if let Some(m) = &media {
            to_store.push((path.clone(), *id, m.clone()));
        }
        probed.push((path.clone(), media));
    }
    for path in &forced {
        probed.push((path.clone(), probe(path)));
    }
    // Acquire the write lock only when there's something to persist — a steady-state
    // re-scan of already-cached files (all hits) stores nothing.
    if !to_store.is_empty() {
        store(&to_store);
    }

    // Hits + freshly probed. Order is irrelevant — select_media_skips collects into a set.
    let mut out: Vec<(String, Option<SourceMedia>)> =
        hits.into_iter().map(|(p, m)| (p, Some(m))).collect();
    out.extend(probed);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(codec: &str, height: i64) -> SourceMedia {
        SourceMedia {
            codec: codec.into(),
            height,
        }
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn store_then_lookup_returns_a_hit_on_matching_identity() {
        let conn = test_conn();
        let id = FileIdentity {
            size: 100,
            mtime: 5000,
        };
        store_batch(&conn, &[("/m/a.mp4".to_string(), id, media("h265", 1080))]);

        let (hits, misses) = lookup_batch(&conn, &[("/m/a.mp4".to_string(), id)]);
        assert_eq!(hits, vec![("/m/a.mp4".to_string(), media("h265", 1080))]);
        assert!(
            misses.is_empty(),
            "a matching identity is a hit, not a miss"
        );
    }

    #[test]
    fn lookup_misses_when_size_or_mtime_differs() {
        let conn = test_conn();
        let id = FileIdentity {
            size: 100,
            mtime: 5000,
        };
        store_batch(&conn, &[("/m/a.mp4".to_string(), id, media("h265", 1080))]);

        // Changed size — e.g. a different file was dropped at this path.
        let (h1, m1) = lookup_batch(
            &conn,
            &[(
                "/m/a.mp4".to_string(),
                FileIdentity {
                    size: 101,
                    mtime: 5000,
                },
            )],
        );
        assert!(h1.is_empty());
        assert_eq!(m1.len(), 1, "a size change must re-probe");

        // Changed mtime — e.g. our in-place re-encode rewrote the file.
        let (h2, m2) = lookup_batch(
            &conn,
            &[(
                "/m/a.mp4".to_string(),
                FileIdentity {
                    size: 100,
                    mtime: 6000,
                },
            )],
        );
        assert!(h2.is_empty());
        assert_eq!(m2.len(), 1, "an mtime change must re-probe");
    }

    #[test]
    fn lookup_misses_when_path_absent() {
        let conn = test_conn();
        let (hits, misses) = lookup_batch(
            &conn,
            &[(
                "/m/never.mp4".to_string(),
                FileIdentity { size: 1, mtime: 1 },
            )],
        );
        assert!(hits.is_empty());
        assert_eq!(misses.len(), 1, "an unseen path is always a miss");
    }

    #[test]
    fn resolve_media_probes_only_misses_and_never_reprobes_a_hit() {
        use std::cell::RefCell;
        // The whole point: a cached hit costs ZERO probes; a miss costs exactly one.
        let probe_calls = RefCell::new(Vec::<String>::new());
        let stored = RefCell::new(Vec::<(String, FileIdentity, SourceMedia)>::new());

        let candidates = vec![
            (
                "/m/hit.mp4".to_string(),
                Some(FileIdentity {
                    size: 10,
                    mtime: 100,
                }),
            ),
            (
                "/m/miss.mp4".to_string(),
                Some(FileIdentity {
                    size: 20,
                    mtime: 200,
                }),
            ),
        ];

        let lookup = |ids: &[(String, FileIdentity)]| {
            let mut hits = Vec::new();
            let mut misses = Vec::new();
            for (p, id) in ids {
                if p == "/m/hit.mp4" {
                    hits.push((p.clone(), media("h265", 1080)));
                } else {
                    misses.push((p.clone(), *id));
                }
            }
            (hits, misses)
        };
        let probe = |p: &str| {
            probe_calls.borrow_mut().push(p.to_string());
            Some(media("h264", 1080))
        };
        let store = |items: &[(String, FileIdentity, SourceMedia)]| {
            stored.borrow_mut().extend_from_slice(items);
        };

        let out = resolve_media(&candidates, lookup, probe, store);

        assert_eq!(
            probe_calls.borrow().as_slice(),
            ["/m/miss.mp4"],
            "only the miss is probed; the hit is never re-probed"
        );
        assert_eq!(stored.borrow().len(), 1, "the fresh probe is cached");
        assert_eq!(stored.borrow()[0].0, "/m/miss.mp4");

        // Both files come back carrying media for the skip policy.
        let hit = out.iter().find(|(p, _)| p == "/m/hit.mp4").unwrap();
        assert_eq!(hit.1, Some(media("h265", 1080)));
        let miss = out.iter().find(|(p, _)| p == "/m/miss.mp4").unwrap();
        assert_eq!(miss.1, Some(media("h264", 1080)));
    }

    #[test]
    fn resolve_media_probes_identityless_files_but_never_caches_them() {
        use std::cell::RefCell;
        // No readable size/mtime -> no stable key: probe it, but never store it.
        let stored = RefCell::new(Vec::<(String, FileIdentity, SourceMedia)>::new());
        let candidates = vec![("/m/no-id.mp4".to_string(), None)];
        let lookup = |_: &[(String, FileIdentity)]| (Vec::new(), Vec::new());
        let probe = |_: &str| Some(media("h265", 1080));
        let store = |items: &[(String, FileIdentity, SourceMedia)]| {
            stored.borrow_mut().extend_from_slice(items)
        };

        let out = resolve_media(&candidates, lookup, probe, store);

        assert_eq!(
            out,
            vec![("/m/no-id.mp4".to_string(), Some(media("h265", 1080)))]
        );
        assert!(
            stored.borrow().is_empty(),
            "an identity-less file is never cached"
        );
    }

    #[test]
    fn resolve_media_does_not_cache_a_failed_probe() {
        use std::cell::RefCell;
        // Uncertainty (None) is never stored, so it is re-evaluated next scan.
        let stored = RefCell::new(Vec::<(String, FileIdentity, SourceMedia)>::new());
        let candidates = vec![(
            "/m/bad.mp4".to_string(),
            Some(FileIdentity { size: 1, mtime: 1 }),
        )];
        let lookup = |ids: &[(String, FileIdentity)]| (Vec::new(), ids.to_vec());
        let probe = |_: &str| None;
        let store = |items: &[(String, FileIdentity, SourceMedia)]| {
            stored.borrow_mut().extend_from_slice(items)
        };

        let out = resolve_media(&candidates, lookup, probe, store);

        assert_eq!(out, vec![("/m/bad.mp4".to_string(), None)]);
        assert!(
            stored.borrow().is_empty(),
            "a failed probe must not be cached"
        );
    }

    #[test]
    fn resolve_media_skips_store_when_there_is_nothing_to_cache() {
        use std::cell::Cell;
        // Steady state: every candidate is a cache hit, so nothing is probed or stored — the
        // store side effect (which would lock the DB) must not be invoked at all.
        let store_called = Cell::new(false);
        let candidates = vec![(
            "/m/hit.mp4".to_string(),
            Some(FileIdentity { size: 1, mtime: 1 }),
        )];
        let lookup = |ids: &[(String, FileIdentity)]| {
            (
                ids.iter()
                    .map(|(p, _)| (p.clone(), media("h265", 1080)))
                    .collect(),
                Vec::new(),
            )
        };
        let probe =
            |_: &str| -> Option<SourceMedia> { panic!("a pure cache hit must never probe") };
        let store = |_: &[(String, FileIdentity, SourceMedia)]| store_called.set(true);

        let out = resolve_media(&candidates, lookup, probe, store);

        assert!(
            !store_called.get(),
            "no store (no DB write lock) when nothing needs caching"
        );
        assert_eq!(
            out,
            vec![("/m/hit.mp4".to_string(), Some(media("h265", 1080)))]
        );
    }

    #[test]
    fn store_upserts_a_stale_row() {
        let conn = test_conn();
        let path = "/m/a.mp4".to_string();
        store_batch(
            &conn,
            &[(
                path.clone(),
                FileIdentity {
                    size: 100,
                    mtime: 5000,
                },
                media("h264", 1080),
            )],
        );
        // Re-probe after the file changed: new identity + new media replace the row.
        store_batch(
            &conn,
            &[(
                path.clone(),
                FileIdentity {
                    size: 200,
                    mtime: 9000,
                },
                media("h265", 720),
            )],
        );

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM probe_cache WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "upsert keeps exactly one row per path");

        let (hits, _) = lookup_batch(
            &conn,
            &[(
                path.clone(),
                FileIdentity {
                    size: 200,
                    mtime: 9000,
                },
            )],
        );
        assert_eq!(
            hits,
            vec![(path, media("h265", 720))],
            "the row reflects the latest probe"
        );
    }
}
