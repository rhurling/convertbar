import { useCallback, useEffect, useId, useRef, useState } from "react";
import { httpCommands } from "../lib/transport/http";
import type { FsEntry } from "../lib/transport/types";
import { errorText } from "../lib/errors";
import { rangeBetween } from "../lib/pathSelection";

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
  const [anchor, setAnchor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [gotoDraft, setGotoDraft] = useState("");

  // Every navigation stays live while a listing is in flight (the breadcrumb and the
  // jump-to-path form are never disabled), and `fsList` has no abort, so responses can arrive
  // out of order — a cold-NAS folder takes seconds while the root the user retreated to
  // answers instantly. Only the newest request may touch state: without this, arrival order
  // wins over user intent, the abandoned folder's listing snaps back under the cursor, and a
  // late failure paints an error banner over a listing that loaded fine (nothing clears it —
  // `setError(null)` only runs when a load starts).
  const latestRequest = useRef(0);

  const dialogRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // Both pickers (Queue's and Watch's) are mounted at once in the wider layouts, so the
  // heading's id cannot be a fixed string.
  const titleId = useId();

  const load = useCallback(async (target: string) => {
    const request = ++latestRequest.current;
    const isCurrent = () => request === latestRequest.current;
    setLoading(true);
    setError(null);
    try {
      const result = await httpCommands.fsList(target);
      if (!isCurrent()) return;
      setEntries(result.entries);
      setPath(target);
      // The selection deliberately survives navigation — gathering from several folders
      // in one pass is the point. The shift anchor does NOT: a range only means
      // something within one listing.
      setAnchor(null);
    } catch (e) {
      if (!isCurrent()) return;
      setError(errorText(e));
    } finally {
      // Guarded too: clearing `loading` from a superseded request drops the spinner and
      // exposes the previous listing as though the pending navigation had finished.
      if (isCurrent()) setLoading(false);
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

  // Focus starts in the dialog rather than wherever the opening click left it, so Escape and
  // Tab-cycling both have somewhere to begin and a screen reader announces the dialog by name.
  useEffect(() => {
    dialogRef.current?.focus();
  }, []);

  // Navigating unmounts the row that had focus, which drops focus to <body> — from there a
  // keyboard user Tabs the whole document again after every folder they enter. Put it on the
  // new listing instead, so Tab continues into the rows. Only when focus actually escaped:
  // a jump-to-path submit leaves it on the still-mounted input, which must not be stolen.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog || dialog.contains(document.activeElement)) return;
    listRef.current?.focus();
  }, [entries]);

  // Tab-cycling and Escape, on the document rather than the dialog: focus can sit on <body>
  // (a click on the inert backdrop, or a row unmounted before the effect above runs), and a
  // handler on the dialog would never see the keydown to bring it back.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
        return;
      }
      if (e.key !== "Tab") return;
      const dialog = dialogRef.current;
      if (!dialog) return;
      // tabIndex >= 0 excludes both the dialog itself and each row's nested checkbox, which
      // is deliberately not its own tab stop; `disabled` excludes the confirm button while
      // nothing is selected.
      const stops = [...dialog.querySelectorAll<HTMLElement>("button, input, [tabindex]")].filter(
        (el) => el.tabIndex >= 0 && !el.matches(":disabled"),
      );
      if (stops.length === 0) return;
      const first = stops[0];
      const last = stops[stops.length - 1];
      const active = document.activeElement;
      if (!dialog.contains(active)) {
        e.preventDefault();
        (e.shiftKey ? last : first).focus();
      } else if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  const toggleSelect = (entryPath: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(entryPath)) next.delete(entryPath);
      else next.add(entryPath);
      return next;
    });
  };

  const selectable = mode === "files";

  const handleRowClick = (entry: FsEntry, shiftKey: boolean) => {
    // Directory mode keeps its original behavior: a folder click navigates, a file is inert.
    if (!selectable) {
      if (entry.is_dir) load(entry.path);
      return;
    }
    if (shiftKey && anchor) {
      const range = rangeBetween(entries, anchor, entry.path);
      if (range.length > 0) {
        // Additive by design: a range never deselects, so a mis-aimed shift-click
        // cannot silently drop work the user did earlier.
        setSelected((prev) => {
          const next = new Set(prev);
          for (const p of range) next.add(p);
          return next;
        });
        return;
      }
    }
    setAnchor(entry.path);
    toggleSelect(entry.path);
  };

  const allSelected = entries.length > 0 && entries.every((e) => selected.has(e.path));
  const someSelected = entries.some((e) => selected.has(e.path));

  const toggleSelectAll = () => {
    // A bulk toggle is not a positional pick, so it leaves no range origin behind — same
    // reset Clear does. Otherwise select-all-off shows an empty selection while a row
    // picked before it still anchors a range, and the next shift-click resurrects it.
    setAnchor(null);
    setSelected((prev) => {
      const next = new Set(prev);
      for (const e of entries) {
        if (allSelected) next.delete(e.path);
        else next.add(e.path);
      }
      return next;
    });
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
      : `Add ${selected.size} item${selected.size === 1 ? "" : "s"}`;

  return (
    <div className="modal-overlay">
      <div
        className="file-browser-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        // Not a tab stop — just a landing target, so opening the picker can put focus inside it.
        tabIndex={-1}
        ref={dialogRef}
      >
        <div className="file-browser-header">
          <span id={titleId}>{mode === "directory" ? "Choose a folder" : "Add files"}</span>
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

        <form
          className="file-browser-goto"
          onSubmit={(e) => {
            e.preventDefault();
            const target = gotoDraft.trim();
            if (target) load(target);
          }}
        >
          <input
            className="setting-input"
            type="text"
            aria-label="Go to path"
            placeholder="/media/movies"
            value={gotoDraft}
            onChange={(e) => setGotoDraft(e.target.value)}
          />
          <button type="submit" className="btn btn-small">
            Go
          </button>
        </form>

        {error && <div className="setting-error">{error}</div>}

        {selectable && !loading && entries.length > 0 && (
          <label className="file-browser-select-all">
            <input
              type="checkbox"
              aria-label="Select all"
              checked={allSelected}
              ref={(el) => {
                if (el) el.indeterminate = !allSelected && someSelected;
              }}
              onChange={toggleSelectAll}
            />
            <span>Select all ({entries.length})</span>
          </label>
        )}

        <div className="file-browser-list" tabIndex={-1} ref={listRef}>
          {loading ? (
            <div className="empty-state">Loading…</div>
          ) : /* `path` is only set by a listing that landed, so a null one means we have
                 nothing to describe — "Empty folder" under the error banner of a folder that
                 was never listed is a claim we cannot make. */
          path === null ? null : entries.length === 0 ? (
            <div className="empty-state">Empty folder</div>
          ) : (
            entries.map((entry) => {
              const isSelected = selected.has(entry.path);
              // Directory-mode file rows stay inert (not selectable, no navigate target), same
              // as their old disabled-button state; every other row is keyboard-operable.
              const rowInteractive = selectable || entry.is_dir;
              return (
                <div
                  key={entry.path}
                  className={`file-browser-entry${isSelected ? " file-browser-entry-selected" : ""}${
                    !selectable && !entry.is_dir ? " file-browser-entry-disabled" : ""
                  }`}
                  role={selectable ? "checkbox" : entry.is_dir ? "button" : undefined}
                  aria-checked={selectable ? isSelected : undefined}
                  tabIndex={rowInteractive ? 0 : undefined}
                  onClick={(e) => handleRowClick(entry, e.shiftKey)}
                  onKeyDown={
                    rowInteractive
                      ? (e) => {
                          // Ignore keydowns bubbled up from the nested checkbox/Open button —
                          // otherwise Enter on the Open button both toggles selection (this
                          // handler) and navigates (the button's own native activation), and our
                          // preventDefault on Space for the row breaks the button's own Space
                          // activation.
                          if (e.target !== e.currentTarget) return;
                          // Both Enter and Space auto-repeat while held, and this handler acts
                          // on every event — a native checkbox toggles once per press. Without
                          // the guard, holding a beat too long toggles twice and leaves the row
                          // unselected while the user believes they selected it.
                          if (e.repeat) return;
                          // Route Enter/Space to the same handler the click uses — one state
                          // transition, two entry points. preventDefault on Space so the modal
                          // doesn't scroll.
                          if (e.key !== "Enter" && e.key !== " ") return;
                          if (e.key === " ") e.preventDefault();
                          handleRowClick(entry, e.shiftKey);
                        }
                      : undefined
                  }
                >
                  {selectable && (
                    /* readOnly, not onChange: the row's click handler owns the state transition
                       (it needs the shiftKey modifier, which a change event does not carry). Without
                       readOnly React warns on every row about a checked input with no onChange. */
                    <input
                      type="checkbox"
                      className="file-browser-entry-check"
                      checked={isSelected}
                      readOnly
                      aria-label={entry.name}
                      // The row is the selection control now — this box isn't a separate tab
                      // stop (three tab stops for one logical control was the wrong shape).
                      tabIndex={-1}
                      onClick={(e) => {
                        e.stopPropagation();
                        handleRowClick(entry, e.shiftKey);
                      }}
                    />
                  )}
                  <span className="file-browser-entry-icon">{entry.is_dir ? "📁" : "📄"}</span>
                  <span className="file-browser-entry-name">{entry.name}</span>
                  {selectable && entry.is_dir && (
                    <button
                      type="button"
                      className="file-browser-entry-open"
                      aria-label={`Open ${entry.name}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        load(entry.path);
                      }}
                    >
                      →
                    </button>
                  )}
                </div>
              );
            })
          )}
        </div>

        <div className="file-browser-footer">
          {mode === "files" && selected.size > 0 && (
            <span className="file-browser-count">
              {selected.size} selected
              <button
                type="button"
                className="btn btn-small btn-dim"
                onClick={() => {
                  setSelected(new Set());
                  setAnchor(null);
                }}
              >
                Clear
              </button>
            </span>
          )}
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
