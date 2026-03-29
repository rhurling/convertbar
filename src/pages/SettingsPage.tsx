import { useCallback, useRef, useState } from "react";
import { useSettings } from "../hooks/useSettings";
import { commands } from "../lib/tauri";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import type { AppSettings, PresetMetadata } from "../lib/tauri";

const DEFAULT_SUFFIX_TEMPLATE = ".{resolution}-{codec}";

const VARIABLES: { key: keyof PresetMetadata; label: string }[] = [
  { key: "codec", label: "{codec}" },
  { key: "resolution", label: "{resolution}" },
  { key: "quality", label: "{quality}" },
  { key: "preset", label: "{preset}" },
  { key: "device", label: "{device}" },
];

function resolveTemplate(
  template: string,
  metadata: PresetMetadata | null,
): string {
  if (!metadata) return template;

  let result = template;
  for (const { key } of VARIABLES) {
    result = result.replace(
      new RegExp(`\\{${key}\\}`, "g"),
      metadata[key] || "",
    );
  }

  // Clean up separators adjacent to empty values
  result = result.replace(/[-_]{2,}/g, (m) => m[0]);
  result = result.replace(/[-_]\./, ".");
  result = result.replace(/\.[-_]/, ".");
  result = result.replace(/[-_]$/, "");

  return result;
}

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
    loading,
    updateSetting,
    updatePresetSuffix,
    detectHandbrake,
  } = useSettings();

  const inputRef = useRef<HTMLInputElement>(null);
  const [updateStatus, setUpdateStatus] = useState<string | null>(null);

  const handleChipClick = useCallback(
    (variable: string) => {
      const newSuffix = presetSuffix + variable;
      updatePresetSuffix(newSuffix);
      inputRef.current?.focus();
    },
    [presetSuffix, updatePresetSuffix],
  );

  const handleReset = useCallback(() => {
    updatePresetSuffix(DEFAULT_SUFFIX_TEMPLATE);
  }, [updatePresetSuffix]);

  if (loading || !settings) {
    return <div className="settings-page loading">Loading settings...</div>;
  }

  const resolvedSuffix = resolveTemplate(presetSuffix, presetMetadata);
  const previewFilename = `vacation${resolvedSuffix}.mp4`;

  const visibleVariables = VARIABLES.filter(
    ({ key }) => presetMetadata && presetMetadata[key],
  );

  return (
    <div className="settings-page">
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
              value={presetSuffix}
              onChange={(e) => updatePresetSuffix(e.target.value)}
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
            value={settings.handbrake_path}
            onChange={(e) => {
              updateSetting("handbrake_path", e.target.value);
              onHbPathChanged?.();
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
        <label className="setting-label">Updates</label>
        <div className="setting-row">
          <button
            className="btn btn-small"
            onClick={async () => {
              setUpdateStatus("Checking...");
              try {
                const update = await check();
                if (update) {
                  setUpdateStatus(`Updating to v${update.version}...`);
                  await update.downloadAndInstall();
                  await relaunch();
                } else {
                  setUpdateStatus("You're up to date");
                  setTimeout(() => setUpdateStatus(null), 3000);
                }
              } catch (e) {
                setUpdateStatus(`Error: ${e}`);
                setTimeout(() => setUpdateStatus(null), 5000);
              }
            }}
            disabled={updateStatus === "Checking..." || updateStatus?.startsWith("Updating")}
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
