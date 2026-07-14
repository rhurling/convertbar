import { useState } from "react";
import { useHistory } from "../hooks/useHistory";
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
  const [showClearMenu, setShowClearMenu] = useState(false);
  const [searchInput, setSearchInput] = useState("");
  const [menu, setMenu] = useState<MenuState | null>(null);

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
