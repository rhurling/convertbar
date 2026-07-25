import { useState, useEffect, useRef } from "react";
import { useHistory } from "../hooks/useHistory";
import { useBadSources } from "../hooks/useBadSources";
import { useSettings } from "../hooks/useSettings";
import { formatBytes } from "../lib/format";
import { commands, type JobInfo, type PathsExist } from "../lib/tauri";
import { resolveTargetPath } from "../lib/historyTarget";
import HistoryItem from "../components/HistoryItem";
import ContextMenu from "../components/ContextMenu";

interface MenuState {
  job: JobInfo;
  x: number;
  y: number;
  exists: PathsExist | null; // null = existence check in flight
}

export default function HistoryPage() {
  const { history, summary, hasMore, loading, loadMore, refresh, setSearchDebounced, sortBy, setSortBy } = useHistory();
  const { badSources, purge } = useBadSources();
  const { settings } = useSettings();
  const [showClearMenu, setShowClearMenu] = useState(false);
  const [searchInput, setSearchInput] = useState("");
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [confirmingPurge, setConfirmingPurge] = useState(false);
  const [purgeOutcomeNote, setPurgeOutcomeNote] = useState<string | null>(null);

  // useSettings() returns `AppSettings | null` while loading, so the optional chain is
  // required — and a null settings object must read as the non-destructive default (Trash
  // wording), never as delete.
  const destructive = settings?.bad_source_action === "delete";
  const purgeActionLabel = destructive
    ? `Delete ${badSources.length} permanently`
    : `Move ${badSources.length} to Trash`;

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

  const runPurge = async () => {
    setPurgeOutcomeNote(null);
    try {
      // Safety-critical: pass the review list's own ids, never history's. The backend also
      // rejects ids that don't belong to a live bad-source row, but that is a backstop, not a
      // substitute for wiring the right collection here.
      const results = await purge(badSources.map((j) => j.id));
      setConfirmingPurge(false);
      const skipped = results.filter((r) => r.outcome !== "purged");
      setPurgeOutcomeNote(
        skipped.length === 0
          ? null
          : `${skipped.length} file(s) were left alone: ${skipped
              .map((r) => r.outcome.replace(/_/g, " "))
              .join(", ")}`,
      );
    } catch (e) {
      console.error("Failed to purge bad sources:", e);
      setConfirmingPurge(false);
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
                    <span className="bad-sources-name">
                      {job.source_path.split(/[/\\]/).pop()}
                    </span>
                    <span className="bad-sources-reason">
                      {(job.error_message ?? "").split("\n")[0]}
                    </span>
                  </li>
                ))}
              </ul>
              {!confirmingPurge ? (
                <button className="btn btn-small" onClick={() => setConfirmingPurge(true)}>
                  {purgeActionLabel}
                </button>
              ) : (
                <div className="bad-sources-confirm">
                  <span>
                    {destructive
                      ? "This cannot be undone."
                      : "Files move to your Trash."}
                  </span>
                  <button className="btn btn-small btn-danger" onClick={runPurge}>
                    Confirm
                  </button>
                  <button className="btn btn-small" onClick={() => setConfirmingPurge(false)}>
                    Cancel
                  </button>
                </div>
              )}
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
