import { useEffect, useState } from "react";
import { useWatchedDirectories } from "../hooks/useWatchedDirectories";
import { fileName } from "../lib/format";
import type { WatchedDirectory } from "../lib/tauri";
import { isServerHead } from "../lib/head";
import FileBrowserModal from "../components/FileBrowserModal";

interface WatchRowProps {
  dir: WatchedDirectory;
  onToggle: (enabled: boolean) => void;
  onRecursive: (recursive: boolean) => void;
  onDelay: (secs: number) => void;
  onRemove: () => void;
}

function WatchRow({
  dir,
  onToggle,
  onRecursive,
  onDelay,
  onRemove,
}: WatchRowProps) {
  // Local draft so typing in the number field doesn't fire a backend write per keystroke;
  // the value is committed on blur / Enter.
  const [delay, setDelay] = useState(String(dir.stability_delay_secs));

  useEffect(() => {
    setDelay(String(dir.stability_delay_secs));
  }, [dir.stability_delay_secs]);

  const commitDelay = () => {
    const secs = Math.max(1, parseInt(delay, 10) || dir.stability_delay_secs);
    setDelay(String(secs));
    if (secs !== dir.stability_delay_secs) onDelay(secs);
  };

  return (
    <div className={`watch-item ${dir.enabled ? "" : "watch-item-disabled"}`}>
      <input
        type="checkbox"
        className="watch-enable"
        checked={dir.enabled}
        onChange={(e) => onToggle(e.target.checked)}
        title={dir.enabled ? "Watching — click to pause" : "Paused — click to watch"}
      />
      <div className="watch-info">
        <div className="watch-name">{fileName(dir.path)}</div>
        <div className="watch-path" title={dir.path}>
          {dir.path}
        </div>
      </div>
      <label className="watch-field" title="Wait this long after a file stops changing before converting it">
        delay
        <input
          type="number"
          min={1}
          value={delay}
          onChange={(e) => setDelay(e.target.value)}
          onBlur={commitDelay}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
        />
        s
      </label>
      <label className="watch-field" title="Also watch nested subfolders">
        <input
          type="checkbox"
          checked={dir.recursive}
          onChange={(e) => onRecursive(e.target.checked)}
        />
        subfolders
      </label>
      <button
        className="btn btn-small btn-dim"
        onClick={onRemove}
        title="Stop watching this folder"
      >
        &times;
      </button>
    </div>
  );
}

export default function WatchedFoldersPage() {
  const {
    directories,
    loading,
    error,
    addDirectory,
    addDirectoryAtPath,
    updateDirectory,
    setEnabled,
    removeDirectory,
  } = useWatchedDirectories();
  const [showBrowser, setShowBrowser] = useState(false);

  return (
    <div className="section">
      <div className="section-header">
        <span>Watched Folders ({directories.length})</span>
        {/* Desktop uses the native folder picker; the server head has no pickFolder equivalent
            and opens the file-browser modal in directory mode instead. */}
        <button
          className="btn btn-small"
          onClick={() => (isServerHead ? setShowBrowser(true) : addDirectory())}
        >
          + Add folder
        </button>
      </div>

      {showBrowser && (
        <FileBrowserModal
          mode="directory"
          onSelect={(paths) => {
            setShowBrowser(false);
            if (paths[0]) addDirectoryAtPath(paths[0]);
          }}
          onClose={() => setShowBrowser(false)}
        />
      )}

      {error && <div className="watch-error">{error}</div>}

      {loading ? (
        <div className="empty-state">Loading…</div>
      ) : directories.length === 0 ? (
        <div className="empty-state">
          <div className="empty-state-icon">👁</div>
          <div>No watched folders yet.</div>
          <div>
            Add a folder and finished downloads dropped into it convert
            automatically.
          </div>
        </div>
      ) : (
        <div className="item-list">
          {directories.map((dir) => (
            <WatchRow
              key={dir.id}
              dir={dir}
              onToggle={(enabled) => setEnabled(dir.id, enabled)}
              onRecursive={(recursive) =>
                updateDirectory(dir.id, recursive, dir.stability_delay_secs)
              }
              onDelay={(secs) => updateDirectory(dir.id, dir.recursive, secs)}
              onRemove={() => removeDirectory(dir.id)}
            />
          ))}
        </div>
      )}
    </div>
  );
}
