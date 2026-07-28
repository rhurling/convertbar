import { useCallback, useEffect, useRef, useState } from "react";
import { useSettings } from "../hooks/useSettings";
import { commands } from "../lib/tauri";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "../lib/events";
import type { AppSettings, PresetMetadata } from "../lib/tauri";

const DEFAULT_SUFFIX_TEMPLATE = ".{resolution}-{codec}";

const VARIABLES: { key: keyof PresetMetadata; label: string }[] = [
  { key: "codec", label: "{codec}" },
  { key: "resolution", label: "{resolution}" },
  { key: "quality", label: "{quality}" },
  { key: "preset", label: "{preset}" },
  { key: "device", label: "{device}" },
];

const SUFFIX_PREVIEW_DEBOUNCE_MS = 250;

interface SettingsPageProps {
  onHbPathChanged?: () => void;
}

export default function SettingsPage({ onHbPathChanged }: SettingsPageProps) {
  const {
    settings,
    presets,
    presetSuffix,
    presetMetadata,
    metadataLoading,
    presetsError,
    error,
    loading,
    updateSetting,
    updatePresetSuffix,
    detectHandbrake,
  } = useSettings();

  const inputRef = useRef<HTMLInputElement>(null);
  const [updateStatus, setUpdateStatus] = useState<string | null>(null);
  const [appVersion, setAppVersion] = useState<string>("");

  // Local drafts so text inputs echo keystrokes instantly and commit once (on blur/Enter),
  // instead of round-tripping an IPC write per character (which dropped/reordered characters).
  const [hbDraft, setHbDraft] = useState(settings?.handbrake_path ?? "");
  const [markerDraft, setMarkerDraft] = useState(settings?.watch_skip_marker ?? "");
  const [diskDraft, setDiskDraft] = useState(String(settings?.low_disk_min_gb ?? 0));
  const [suffixDraft, setSuffixDraft] = useState(presetSuffix);
  const [resolvedSuffix, setResolvedSuffix] = useState("");

  useEffect(() => { getVersion().then(setAppVersion); }, []);
  useEffect(() => {
    if (settings) setHbDraft(settings.handbrake_path);
  }, [settings?.handbrake_path]);
  useEffect(() => {
    if (settings) setMarkerDraft(settings.watch_skip_marker);
  }, [settings?.watch_skip_marker]);
  useEffect(() => {
    if (settings) setDiskDraft(String(settings.low_disk_min_gb));
  }, [settings?.low_disk_min_gb]);
  useEffect(() => {
    setSuffixDraft(presetSuffix);
  }, [presetSuffix]);

  // Preview resolves the *draft* (so it updates as you type) via the backend resolver —
  // never a JS reimplementation, which diverged from the real output-name algorithm.
  useEffect(() => {
    if (!presetMetadata) {
      setResolvedSuffix(suffixDraft);
      return;
    }
    // Generation guard: an already-fired resolve must not overwrite a newer draft's preview
    // (or setState after unmount) when it resolves late. The cleanup flips `active` off before
    // the next effect run, so a superseded invoke's continuation is dropped.
    let active = true;
    const timer = setTimeout(() => {
      commands
        .resolveSuffixTemplate(suffixDraft, presetMetadata)
        .then((resolved) => {
          if (active) setResolvedSuffix(resolved);
        })
        .catch(() => {
          if (active) setResolvedSuffix(suffixDraft);
        });
    }, SUFFIX_PREVIEW_DEBOUNCE_MS);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [suffixDraft, presetMetadata]);

  const commitHbPath = async () => {
    if (hbDraft === settings?.handbrake_path) return;
    // Await the write before validating, so the banner reflects the new path, not the old.
    await updateSetting("handbrake_path", hbDraft);
    onHbPathChanged?.();
  };

  const commitMarker = () => {
    if (markerDraft !== settings?.watch_skip_marker) {
      updateSetting("watch_skip_marker", markerDraft);
    }
  };

  const commitDisk = () => {
    if (diskDraft !== String(settings?.low_disk_min_gb)) {
      updateSetting("low_disk_min_gb", diskDraft);
    }
  };

  const commitSuffix = () => {
    if (suffixDraft !== presetSuffix) updatePresetSuffix(suffixDraft);
  };

  const handleChipClick = useCallback(
    (variable: string) => {
      const newSuffix = suffixDraft + variable;
      setSuffixDraft(newSuffix);
      updatePresetSuffix(newSuffix);
      inputRef.current?.focus();
    },
    [suffixDraft, updatePresetSuffix],
  );

  const handleReset = useCallback(() => {
    setSuffixDraft(DEFAULT_SUFFIX_TEMPLATE);
    updatePresetSuffix(DEFAULT_SUFFIX_TEMPLATE);
  }, [updatePresetSuffix]);

  if (loading || !settings) {
    return <div className="settings-page loading">Loading settings...</div>;
  }

  const previewFilename = `vacation${resolvedSuffix}.mp4`;

  const visibleVariables = VARIABLES.filter(
    ({ key }) => presetMetadata && presetMetadata[key],
  );

  return (
    <div className="settings-page">
      {error && <div className="setting-error">{error}</div>}
      <div className="setting-group">
        <label className="setting-label">Preset</label>
        {presetsError ? (
          <div className="setting-error">{presetsError}</div>
        ) : (
          <select
            className="setting-input"
            value={settings.preset}
            onChange={(e) => updateSetting("preset", e.target.value)}
          >
            {presets.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        )}
      </div>

      <div className="setting-group">
        <div className="suffix-header">
          <label className="setting-label">Output suffix template</label>
          <button
            className="btn btn-small"
            onClick={handleReset}
            title="Reset to default template"
          >
            Reset
          </button>
        </div>

        {metadataLoading ? (
          <div className="suffix-loading">Loading preset info...</div>
        ) : (
          <>
            <input
              ref={inputRef}
              className="setting-input"
              type="text"
              value={suffixDraft}
              onChange={(e) => setSuffixDraft(e.target.value)}
              onBlur={commitSuffix}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
              placeholder={DEFAULT_SUFFIX_TEMPLATE}
            />

            {visibleVariables.length > 0 && (
              <div className="variable-chips">
                {visibleVariables.map(({ key, label }) => (
                  <button
                    key={key}
                    className="variable-chip"
                    onClick={() => handleChipClick(label)}
                    title={`Click to append ${label}`}
                  >
                    <span className="variable-chip-name">{label}</span>
                    <span className="variable-chip-value">
                      {presetMetadata![key]}
                    </span>
                  </button>
                ))}
              </div>
            )}

            <div className="suffix-preview">
              Preview: <span>{previewFilename}</span>
            </div>

            {resolvedSuffix.trim() === "" && (
              <div className="suffix-inplace-note">
                Empty suffix: mp4 files are re-encoded in place, replacing the original. The fast
                &quot;already converted&quot; skip-by-suffix is also disabled.
              </div>
            )}
          </>
        )}
      </div>

      <div className="setting-group">
        <label className="setting-label">After conversion</label>
        <div className="setting-radios">
          <label className="radio-label">
            <input
              type="radio"
              name="cleanup"
              checked={settings.cleanup_mode === "trash"}
              onChange={() => updateSetting("cleanup_mode", "trash")}
            />
            Move original to Trash
          </label>
          <label className="radio-label">
            <input
              type="radio"
              name="cleanup"
              checked={settings.cleanup_mode === "delete"}
              onChange={() => updateSetting("cleanup_mode", "delete")}
            />
            Delete permanently
          </label>
        </div>
      </div>

      <div className="setting-group">
        <label className="setting-label">Bad source files</label>
        <p className="setting-hint">
          Files ConvertBar could not read, or that turned out to be incomplete
          downloads, are listed in History. Nothing is removed until you choose to.
        </p>
        <div className="setting-radios">
          <label className="radio-label">
            <input
              type="radio"
              name="badSource"
              checked={settings.bad_source_action === "trash"}
              onChange={() => updateSetting("bad_source_action", "trash")}
            />
            Move bad source files to Trash
          </label>
          <label className="radio-label">
            <input
              type="radio"
              name="badSource"
              checked={settings.bad_source_action === "delete"}
              onChange={() => updateSetting("bad_source_action", "delete")}
            />
            Delete bad source files permanently
          </label>
        </div>
      </div>

      <div className="setting-group">
        <label className="setting-label">
          <input
            type="checkbox"
            checked={settings.skip_already_converted}
            onChange={(e) =>
              updateSetting("skip_already_converted", String(e.target.checked))
            }
          />
          Skip already-converted files
        </label>
        <p className="setting-hint">
          When adding files, skip any that were previously converted successfully
        </p>
      </div>

      <div className="setting-group">
        <label className="setting-label">
          <input
            type="checkbox"
            checked={settings.skip_by_source_media}
            onChange={(e) =>
              updateSetting("skip_by_source_media", String(e.target.checked))
            }
          />
          Skip files already at or below the target
        </label>
        <p className="setting-hint">
          When adding files, skip any whose codec and resolution already meet the target
          preset, so they aren&apos;t needlessly re-encoded. Turn this off to force a
          conversion (e.g. for device compatibility).
        </p>
      </div>

      <div className="setting-group">
        <label className="setting-label">Pause when destination free space is low</label>
        <div className="setting-row">
          <input
            className="setting-input"
            type="number"
            min="0"
            step="0.5"
            value={diskDraft}
            onChange={(e) => setDiskDraft(e.target.value)}
            onBlur={commitDisk}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
            }}
          />
          <span>GB</span>
        </div>
        <p className="setting-hint">
          Before starting each file, if the destination disk has less free space than this (plus
          room for the encode), the queue pauses instead of converting the next file. Resume it
          from the Queue tab once you&apos;ve freed space. Set to 0 to never pause.
        </p>
      </div>

      <div className="setting-group">
        <label className="setting-label">Watched-folder skip marker</label>
        <input
          className="setting-input"
          type="text"
          value={markerDraft}
          onChange={(e) => setMarkerDraft(e.target.value)}
          onBlur={commitMarker}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
          placeholder=".downloading"
        />
        <p className="setting-hint">
          In watched folders, ignore any file whose folder (or a parent folder) contains a
          file with this name, and convert them once it&apos;s removed. Point it at the marker
          your downloader creates while downloading. Leave empty to disable.
        </p>
      </div>

      <div className="setting-group">
        <label className="setting-label">Menu bar display</label>
        <p className="setting-hint">Choose what to show next to the icon during encoding</p>
        <div className="setting-toggles">
          {[
            { key: "menubar_show_percent", label: "Percentage" },
            { key: "menubar_show_eta", label: "ETA" },
            { key: "menubar_show_queue", label: "Queue count" },
            { key: "menubar_show_filename", label: "File name" },
            { key: "menubar_show_fps", label: "Encoding speed" },
          ].map(({ key, label }) => (
            <label key={key} className="toggle-label">
              <input
                type="checkbox"
                checked={settings[key as keyof AppSettings] === true}
                onChange={(e) => updateSetting(key, String(e.target.checked))}
              />
              {label}
            </label>
          ))}
        </div>
      </div>

      <div className="setting-group">
        <label className="setting-label">Notifications</label>
        <div className="setting-toggles">
          <label className="toggle-label">
            <input type="checkbox"
              checked={settings.notifications_per_file}
              onChange={(e) => updateSetting("notifications_per_file", String(e.target.checked))} />
            Notify per file
          </label>
          {settings.notifications_per_file && (
            <label className="toggle-label toggle-sub">
              <input type="checkbox"
                checked={settings.notifications_errors_only}
                onChange={(e) => updateSetting("notifications_errors_only", String(e.target.checked))} />
              Errors only
            </label>
          )}
          <label className="toggle-label">
            <input type="checkbox"
              checked={settings.notifications_queue_done}
              onChange={(e) => updateSetting("notifications_queue_done", String(e.target.checked))} />
            Notify when queue finishes
          </label>
        </div>
      </div>

      <div className="setting-group">
        <label className="setting-label">
          <input
            type="checkbox"
            checked={settings.launch_at_login}
            onChange={(e) =>
              updateSetting("launch_at_login", String(e.target.checked))
            }
          />
          Launch at login
        </label>
      </div>

      <div className="setting-group">
        <label className="setting-label">HandBrakeCLI path</label>
        <div className="setting-row">
          <input
            className="setting-input flex-1"
            type="text"
            value={hbDraft}
            onChange={(e) => setHbDraft(e.target.value)}
            onBlur={commitHbPath}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
            }}
            placeholder="/usr/local/bin/HandBrakeCLI"
          />
          <button
            className="btn btn-small"
            onClick={async () => {
              const path = await detectHandbrake();
              onHbPathChanged?.();
              if (!path) {
                alert("HandBrakeCLI not found on this system.");
              }
            }}
          >
            Detect
          </button>
        </div>
      </div>

      <div className="setting-group">
        <label className="setting-label">Updates {appVersion && <span className="version-label">v{appVersion}</span>}</label>
        <div className="setting-row">
          <button
            className="btn btn-small"
            onClick={async () => {
              setUpdateStatus("Checking...");
              try {
                const update = await check();
                if (update) {
                  setUpdateStatus(`Downloading v${update.version}...`);
                  await update.downloadAndInstall();

                  const queue = await commands.getQueue();
                  const isEncoding = queue.some(j => j.status === "encoding" || j.status === "paused");

                  if (!isEncoding) {
                    await relaunch();
                  } else {
                    await commands.pauseAfterCurrent();
                    setUpdateStatus("Update ready, restarting after current job...");
                    const unlisten = await listen<{ status: string }>("menu-bar-update", async (event) => {
                      if (event.payload.status === "idle" || event.payload.status === "error") {
                        unlisten();
                        await relaunch();
                      }
                    });
                  }
                } else {
                  setUpdateStatus("You're up to date");
                  setTimeout(() => setUpdateStatus(null), 3000);
                }
              } catch (e) {
                setUpdateStatus(`Error: ${e}`);
                setTimeout(() => setUpdateStatus(null), 5000);
              }
            }}
            disabled={updateStatus === "Checking..." || updateStatus?.startsWith("Downloading") || updateStatus?.startsWith("Update ready")}
          >
            Check for updates
          </button>
          {updateStatus && <span className="update-status">{updateStatus}</span>}
        </div>
      </div>

      <div className="setting-group setting-group-quit">
        <button
          className="btn btn-quit"
          onClick={() => commands.quitApp()}
        >
          Quit ConvertBar
        </button>
      </div>
    </div>
  );
}
