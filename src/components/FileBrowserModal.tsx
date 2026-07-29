import { useCallback, useEffect, useState } from "react";
import { httpCommands } from "../lib/transport/http";
import type { FsEntry } from "../lib/transport/types";
import { errorText } from "../lib/errors";

const FALLBACK_ROOT = "/";

interface FileBrowserModalProps {
  /** "files": multi-select files to add to the queue. "directory": pick one directory to watch. */
  mode: "files" | "directory";
  onSelect: (paths: string[]) => void;
  onClose: () => void;
}

/** The configured root that contains `path`, so breadcrumb up-navigation never offers an
 * ancestor above it (the server 403s anything outside `browse_roots` — see `routes::fs`). */
function containingRoot(path: string, roots: string[]): string {
  return roots.find((root) => path === root || path.startsWith(root.endsWith("/") ? root : `${root}/`)) ?? roots[0];
}

/** Joins a root with the relative segments under it, without producing a doubled slash when
 * root is "/" itself. */
function joinUnderRoot(root: string, segments: string[]): string {
  if (segments.length === 0) return root;
  const base = root === "/" ? "" : root;
  return `${base}/${segments.join("/")}`;
}

/**
 * Server-head-only file/folder browser, backed by the http transport's `fsList` (a server-only
 * extra outside `Transport` — imported directly from `transport/http`, same seam LoginScreen
 * uses for `login`). Replaces Tauri's native dialog, which has no equivalent in a browser tab.
 *
 * Starts at the first configured `browse_roots` entry (fetched via `getAppInfo()` on mount)
 * rather than always guessing "/" — a deployment that restricts `CONVERTBAR_BROWSE_ROOTS` (e.g.
 * to `/media`) would otherwise 403 on the very first listing with no way to navigate anywhere.
 */
export default function FileBrowserModal({ mode, onSelect, onClose }: FileBrowserModalProps) {
  const [roots, setRoots] = useState<string[]>([FALLBACK_ROOT]);
  const [path, setPath] = useState<string | null>(null);
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
      setError(errorText(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // Resolve the configured root(s) before the first listing, so the modal starts inside
  // whatever the deployment actually allows browsing rather than an unconditional "/".
  useEffect(() => {
    let active = true;
    httpCommands
      .getAppInfo()
      .then((info) => {
        if (!active) return;
        const resolvedRoots = info.browse_roots.length > 0 ? info.browse_roots : [FALLBACK_ROOT];
        setRoots(resolvedRoots);
        return load(resolvedRoots[0]);
      })
      .catch(() => {
        // getAppInfo itself failing is unexpected (it's the same endpoint that gated this
        // component's very presence) — fall back to "/" rather than leaving the modal stuck.
        if (active) load(FALLBACK_ROOT);
      });
    return () => {
      active = false;
    };
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
    onSelect(mode === "directory" ? [path!] : [...selected]);
  };

  // Breadcrumb: a crumb for the containing configured root (its own label, not always "/"),
  // then one crumb per path segment beneath it — never a crumb above the root itself.
  const root = path === null ? roots[0] : containingRoot(path, roots);
  const relativeSegments =
    path === null || path === root ? [] : path.slice(root.length).split("/").filter(Boolean);

  const confirmDisabled = path === null || (mode === "files" && selected.size === 0);
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
          <button type="button" onClick={() => load(root)}>
            {root}
          </button>
          {relativeSegments.map((seg, i) => (
            <span key={i}>
              <span className="breadcrumb-sep"> / </span>
              <button type="button" onClick={() => load(joinUnderRoot(root, relativeSegments.slice(0, i + 1)))}>
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
