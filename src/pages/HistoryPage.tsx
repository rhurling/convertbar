import { useState, useEffect, useRef } from "react";
import { useHistory } from "../hooks/useHistory";
import { useBadSources } from "../hooks/useBadSources";
import { useSettings } from "../hooks/useSettings";
import { formatBytes } from "../lib/format";
import {
  commands,
  type JobInfo,
  type PathsExist,
  type PurgeOutcome,
  type PurgeResult,
} from "../lib/tauri";
import { resolveTargetPath } from "../lib/historyTarget";
import HistoryItem from "../components/HistoryItem";
import ContextMenu from "../components/ContextMenu";

interface MenuState {
  job: JobInfo;
  x: number;
  y: number;
  exists: PathsExist | null; // null = existence check in flight
}

// Plain-English wording for the six ways a file can be left alone, mirroring
// src-tauri/src/types.rs PurgeOutcome. An outcome the frontend doesn't recognize (a future
// backend variant) still needs a safe fallback rather than rendering "undefined" or crashing.
const OUTCOME_LABELS: Partial<Record<PurgeOutcome, string>> = {
  in_use: "still queued or being converted",
  already_gone: "already deleted",
  changed: "changed on disk since it was flagged",
  recovered: "turned out to be readable after all",
  unverifiable: "couldn't be re-checked (drive unavailable?)",
  failed: "could not be removed",
};

// Aggregates by outcome instead of dumping one entry per file — a raw enum list like
// "in use, in use, changed" is meaningless to a non-technical user and doesn't say how many
// of each, or which files. Always states the purged count too (even zero), so "nothing was
// removed" is stated as plainly as "everything was removed".
function buildOutcomeNote(results: PurgeResult[]): string | null {
  const purgedCount = results.filter((r) => r.outcome === "purged").length;
  const skipped = results.filter((r) => r.outcome !== "purged");
  if (skipped.length === 0) return null;

  const counts = new Map<string, number>();
  for (const r of skipped) {
    counts.set(r.outcome, (counts.get(r.outcome) ?? 0) + 1);
  }
  const parts = [...counts.entries()].map(
    ([outcome, count]) => `${count} ${OUTCOME_LABELS[outcome as PurgeOutcome] ?? "left alone"}`,
  );

  return `${purgedCount} file(s) removed. ${parts.join(", ")}.`;
}

// Plain-English reason per reviewed row, keyed by the failure_class the backend already
// distinguishes. The stored error_message can't serve here: its headline is HandBrake's own
// promoted diagnostic, so two corrupt MP4s read as
// "[mov,mp4,m4a,3gp,3g2,mj2 @ 0x8e0a44000] moov atom not found" — differing only by a heap
// pointer, with the informative words pushed past the panel's right edge. An unlabelled
// failure_class falls back to the stored message rather than rendering blank.
const BAD_SOURCE_REASONS: Record<string, string> = {
  bad_source: "Unreadable — HandBrake found no video",
  bad_source_truncated: "Incomplete — file stops partway through",
};

export default function HistoryPage() {
  const { history, summary, hasMore, loading, loadMore, refresh, setSearchDebounced, sortBy, setSortBy } = useHistory();
  const { badSources, purge } = useBadSources();
  const { settings } = useSettings();
  const [showClearMenu, setShowClearMenu] = useState(false);
  const [searchInput, setSearchInput] = useState("");
  const [menu, setMenu] = useState<MenuState | null>(null);
  // Snapshot of the ids the user actually reviewed when they armed the confirm step — Confirm
  // must only ever act on this snapshot, never on the live badSources list. null = not armed.
  const [armedIds, setArmedIds] = useState<string[] | null>(null);
  const [purging, setPurging] = useState(false);
  const [purgeOutcomeNote, setPurgeOutcomeNote] = useState<string | null>(null);

  // useSettings() returns `AppSettings | null` while loading. The purge button itself is
  // gated on settings below (C4) — a null settings object never gets a chance to front a
  // permanent delete under Trash wording — so this only affects wording once settings exist.
  const destructive = settings?.bad_source_action === "delete";
  const purgeActionLabel = destructive
    ? `Delete ${badSources.length} permanently`
    : `Move ${badSources.length} to Trash`;
  const confirmLabel = destructive
    ? `Confirm — delete ${armedIds?.length ?? 0} permanently`
    : `Confirm — move ${armedIds?.length ?? 0} to Trash`;

  // HistoryPage unmounts on tab switch. A purge started here can still be in flight when that
  // happens (each re-scan can take ~30s), and its result must not update state on a component
  // that's gone. This only prevents a dead/no-op update — it does not preserve the pending or
  // outcome state across the tab switch, so a result landing after the user has switched away
  // is still silently dropped rather than shown when they come back (see C6).
  const mountedRef = useRef(true);
  useEffect(() => {
    // React 19's StrictMode dev double-invoke mounts, cleans up, and remounts every effect on
    // the same component instance — without this line, the cleanup from that first synthetic
    // pass leaves mountedRef latched false for the rest of the session, so runPurge's
    // `if (!mountedRef.current) return;` guard (below) skips ALL post-await state updates,
    // permanently.
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // A purge can only ever REMOVE rows that were already in the list when it started (purged
  // or already-gone rows drop out; every other outcome leaves a row in place) — it can never
  // add one. So if the list ever contains an id that wasn't present the last time we saw it,
  // that arrival is unrelated to any purge in flight (e.g. a new job just failed) and any
  // outcome note on screen no longer describes what's currently shown.
  const seenIdsRef = useRef<Set<string>>(new Set());
  useEffect(() => {
    const isNewArrival = badSources.some((j) => !seenIdsRef.current.has(j.id));
    if (isNewArrival) {
      setPurgeOutcomeNote(null);
    }
    seenIdsRef.current = new Set(badSources.map((j) => j.id));
  }, [badSources]);

  // The armed confirm targets a snapshot taken at arm time. Escape hides the popover without
  // unmounting it, so an armed confirm can sit for hours while the queue keeps running — if
  // the reviewed set changes AT ALL before Confirm is clicked, disarm rather than silently
  // widening (or narrowing) what Confirm would destroy. This is the simpler and safer of the
  // two options the review called out: the user has to look at the new list and re-arm, so
  // the count they confirm is always the count they actually saw (C1). Skipped while a purge
  // is already running — those ids were committed at click time, and disarming mid-flight
  // would re-enable the arm button on top of an in-flight purge (C2).
  useEffect(() => {
    if (!armedIds || purging) return;
    const currentIds = new Set(badSources.map((j) => j.id));
    const unchanged =
      currentIds.size === armedIds.length && armedIds.every((id) => currentIds.has(id));
    if (!unchanged) setArmedIds(null);
  }, [badSources, armedIds, purging]);

  const runPurge = async () => {
    if (!armedIds || purging) return; // re-entry guard: a second click mid-flight is a no-op
    // Intersect with the current list as a last-instant backstop — the disarm effect above
    // should already make this a no-op, but Confirm must never send an id the list no longer
    // contains.
    const idsToPurge = armedIds.filter((id) => badSources.some((j) => j.id === id));
    setPurging(true);
    setPurgeOutcomeNote(null);
    try {
      // Safety-critical: pass the review list's own ids, never history's. The backend also
      // rejects ids that don't belong to a live bad-source row, but that is a backstop, not a
      // substitute for wiring the right collection here.
      const results = await purge(idsToPurge);
      if (!mountedRef.current) return;
      setArmedIds(null);
      setPurging(false);
      setPurgeOutcomeNote(buildOutcomeNote(results));
    } catch (e) {
      console.error("Failed to purge bad sources:", e);
      if (!mountedRef.current) return;
      setArmedIds(null);
      setPurging(false);
      setPurgeOutcomeNote("Failed to process bad sources. Please try again.");
    }
  };

  const handleItemContextMenu = (e: React.MouseEvent, job: JobInfo) => {
    setMenu({ job, x: e.clientX, y: e.clientY, exists: null });
    commands.checkPathsExist(job.source_path, job.output_path).then(
      (exists) =>
        // Guard against a slow stat landing after the menu moved to another row.
        setMenu((m) => (m && m.job.id === job.id ? { ...m, exists } : m)),
      () => {},
    );
  };

  const target = menu?.exists ? resolveTargetPath(menu.job, menu.exists) : null;
  const checkPending = menu !== null && menu.exists === null;

  return (
    <div className="history-page">
      {history.length > 0 && (
        <div className="history-summary">
          {summary.total_files > 0 && (
            <div className="summary-left">
              <span className="summary-saved">
                Total saved: {formatBytes(summary.total_saved_bytes)}
              </span>
              <span className="summary-files">
                {summary.total_files} file{summary.total_files !== 1 ? "s" : ""}
              </span>
            </div>
          )}
          <div className="clear-dropdown">
            <button className="btn btn-small" onClick={() => setShowClearMenu(!showClearMenu)}>
              Clear &#9662;
            </button>
            {showClearMenu && (
              <div className="clear-dropdown-menu">
                <button onClick={async () => { await commands.clearCompleted("all"); refresh(); setShowClearMenu(false); }}>
                  Clear All
                </button>
                <button onClick={async () => { await commands.clearCompleted("errors"); refresh(); setShowClearMenu(false); }}>
                  Clear Errors Only
                </button>
              </div>
            )}
          </div>
        </div>
      )}

      {(badSources.length > 0 || purgeOutcomeNote) && (
        <div className="bad-sources-panel">
          {badSources.length > 0 && (
            <>
              <span className="bad-sources-title">
                Bad sources ({badSources.length})
              </span>
              <ul className="bad-sources-list">
                {badSources.map((job) => (
                  <li key={job.id}>
                    <span className="bad-sources-name" title={job.source_path}>
                      {job.source_path.split(/[/\\]/).pop()}
                    </span>
                    <span className="bad-sources-reason">
                      {BAD_SOURCE_REASONS[job.failure_class ?? ""] ??
                        (job.error_message ?? "").split("\n")[0]}
                    </span>
                  </li>
                ))}
              </ul>
              {/* Gated on settings resolving (C4): while settings is still loading we don't
                  yet know whether bad_source_action is trash or delete, and the backend reads
                  the real setting regardless of what the button says — rendering Trash
                  wording that could actually execute as a permanent delete is worse than a
                  brief absence of the button. */}
              {settings &&
                (!armedIds ? (
                  <button
                    className="btn btn-small"
                    onClick={() => setArmedIds(badSources.map((j) => j.id))}
                  >
                    {purgeActionLabel}
                  </button>
                ) : (
                  <div className="bad-sources-confirm">
                    <span>
                      {purging
                        ? `Removing ${armedIds.length} file${armedIds.length !== 1 ? "s" : ""}…`
                        : destructive
                          ? "This cannot be undone."
                          : "Files move to your Trash."}
                    </span>
                    <button
                      className="btn btn-small btn-danger"
                      onClick={runPurge}
                      disabled={purging}
                    >
                      {confirmLabel}
                    </button>
                    <button
                      className="btn btn-small"
                      onClick={() => setArmedIds(null)}
                      disabled={purging}
                    >
                      Cancel
                    </button>
                  </div>
                ))}
            </>
          )}
          {/* Rendered even when the list above just emptied out (e.g. every row was
              already_gone and dropped out on refresh) — otherwise the panel vanishes
              with the outcome unreported, silently implying everything was destroyed. */}
          {purgeOutcomeNote && <p className="setting-hint">{purgeOutcomeNote}</p>}
        </div>
      )}

      <div className="history-controls">
        <input
          className="setting-input search-input"
          type="text"
          placeholder="Search files..."
          value={searchInput}
          onChange={(e) => { setSearchInput(e.target.value); setSearchDebounced(e.target.value); }}
        />
        <div className="sort-buttons">
          {[
            { key: "completed_at", label: "Date" },
            { key: "space_saved", label: "Saved" },
            { key: "original_size", label: "Size" },
            { key: "source_path", label: "Name" },
          ].map(({ key, label }) => (
            <button
              key={key}
              className={`btn btn-small ${sortBy === key ? "btn-active" : "btn-dim"}`}
              onClick={() => setSortBy(key)}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      <div className="item-list">
        {history.map((job) => (
          <HistoryItem key={job.id} job={job} onContextMenu={handleItemContextMenu} />
        ))}
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          hint={!checkPending && !target ? "File missing" : null}
          items={[
            {
              label: "Open",
              disabled: checkPending || !target,
              onClick: () => {
                if (target) commands.openPath(target).catch(() => {});
              },
            },
            {
              label: "Open containing folder",
              disabled: checkPending || !target,
              onClick: () => {
                if (target) commands.revealInDir(target).catch(() => {});
              },
            },
            {
              label: "Remove from history",
              danger: true,
              separatorBefore: true,
              onClick: async () => {
                await commands.removeHistoryEntry(menu.job.id);
                refresh();
              },
            },
          ]}
        />
      )}

      {hasMore && (
        <button className="btn btn-block" onClick={loadMore} disabled={loading}>
          {loading ? "Loading..." : "Load more"}
        </button>
      )}

      {history.length === 0 && !loading && (
        <div className="empty-state">
          <span className="empty-state-icon">&#128203;</span>
          <span>Completed conversions will appear here</span>
        </div>
      )}
    </div>
  );
}
