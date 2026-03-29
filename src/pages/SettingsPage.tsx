import { useSettings } from "../hooks/useSettings";

export default function SettingsPage() {
  const {
    settings,
    presets,
    presetSuffix,
    presetsError,
    loading,
    updateSetting,
    updatePresetSuffix,
    detectHandbrake,
  } = useSettings();

  if (loading || !settings) {
    return <div className="settings-page loading">Loading settings...</div>;
  }

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
        <label className="setting-label">Output suffix</label>
        <input
          className="setting-input"
          type="text"
          value={presetSuffix}
          onChange={(e) => updatePresetSuffix(e.target.value)}
          placeholder="e.g. _converted"
        />
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
            onChange={(e) =>
              updateSetting("handbrake_path", e.target.value)
            }
            placeholder="/usr/local/bin/HandBrakeCLI"
          />
          <button
            className="btn btn-small"
            onClick={async () => {
              const path = await detectHandbrake();
              if (!path) {
                alert("HandBrakeCLI not found on this system.");
              }
            }}
          >
            Detect
          </button>
        </div>
      </div>
    </div>
  );
}
