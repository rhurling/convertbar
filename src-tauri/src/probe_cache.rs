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
