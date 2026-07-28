import { useCallback, useEffect, useState } from "react";
import { httpCommands } from "../lib/transport/http";
import type { FsEntry } from "../lib/transport/types";

const ROOT = "/";

interface FileBrowserModalProps {
  /** "files": multi-select files to add to the queue. "directory": pick one directory to watch. */
  mode: "files" | "directory";
  onSelect: (paths: string[]) => void;
  onClose: () => void;
}

/**
 * Server-head-only file/folder browser, backed by the http transport's `fsList` (a server-only
 * extra outside `Transport` — imported directly from `transport/http`, same seam LoginScreen
 * uses for `login`). Replaces Tauri's native dialog, which has no equivalent in a browser tab.
 */
export default function FileBrowserModal({ mode, onSelect, onClose }: FileBrowserModalProps) {
  const [path, setPath] = useState(ROOT);
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (target: string) => {
    setLoading(true);
    setError(null);
    try {
      const result = await httpCommands.fsList(target);
      setEntries(result.entries);
      setPath(target);
      setSelected(new Set());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load(ROOT);
  }, [load]);

  const toggleSelect = (entryPath: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(entryPath)) next.delete(entryPath);
      else next.add(entryPath);
      return next;
    });
  };

  const handleEntryClick = (entry: FsEntry) => {
    if (entry.is_dir) {
      load(entry.path);
    } else if (mode === "files") {
      toggleSelect(entry.path);
    }
  };

  const handleConfirm = () => {
    onSelect(mode === "directory" ? [path] : [...selected]);
  };

  // Breadcrumb: "/" plus one crumb per path segment, each jumping straight to that ancestor.
  const segments = path === ROOT ? [] : path.split("/").filter(Boolean);

  const confirmDisabled = mode === "files" && selected.size === 0;
  const confirmLabel =
    mode === "directory"
      ? "Choose this folder"
      : `Add ${selected.size} file${selected.size === 1 ? "" : "s"}`;

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="file-browser-modal" onClick={(e) => e.stopPropagation()}>
        <div className="file-browser-header">
          <span>{mode === "directory" ? "Choose a folder" : "Add files"}</span>
          <button type="button" className="btn btn-small" onClick={onClose} title="Close">
            &times;
          </button>
        </div>

        <div className="file-browser-breadcrumb">
          <button type="button" onClick={() => load(ROOT)}>
            /
          </button>
          {segments.map((seg, i) => (
            <span key={i}>
              <span className="breadcrumb-sep"> / </span>
              <button type="button" onClick={() => load("/" + segments.slice(0, i + 1).join("/"))}>
                {seg}
              </button>
            </span>
          ))}
        </div>

        {error && <div className="setting-error">{error}</div>}

        <div className="file-browser-list">
          {loading ? (
            <div className="empty-state">Loading…</div>
          ) : entries.length === 0 ? (
            <div className="empty-state">Empty folder</div>
          ) : (
            entries.map((entry) => {
              const isSelected = selected.has(entry.path);
              const selectable = mode === "files" && !entry.is_dir;
              return (
                <button
                  key={entry.path}
                  type="button"
                  className={`file-browser-entry${isSelected ? " file-browser-entry-selected" : ""}`}
                  onClick={() => handleEntryClick(entry)}
                  disabled={!entry.is_dir && mode === "directory"}
                >
                  <span className="file-browser-entry-icon">
                    {entry.is_dir ? "📁" : selectable ? (isSelected ? "☑" : "☐") : "📄"}
                  </span>
                  <span className="file-browser-entry-name">{entry.name}</span>
                </button>
              );
            })
          )}
        </div>

        <div className="file-browser-footer">
          <button type="button" className="btn btn-small" onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="btn btn-small" disabled={confirmDisabled} onClick={handleConfirm}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
