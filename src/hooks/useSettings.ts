import { useState, useEffect, useCallback } from "react";
import { commands, type AppSettings, type PresetMetadata } from "../lib/tauri";

// `update_setting` is stringly-typed on the wire ("true"/"false" for booleans); the
// optimistic merge must land booleans as real booleans so `checked={value === true}` works.
function coerceSettingValue(current: unknown, value: string): string | boolean {
  return typeof current === "boolean" ? value === "true" : value;
}

export function useSettings() {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [presets, setPresets] = useState<string[]>([]);
  const [presetSuffix, setPresetSuffix] = useState<string>("");
  const [presetMetadata, setPresetMetadata] = useState<PresetMetadata | null>(null);
  const [metadataLoading, setMetadataLoading] = useState(false);
  const [presetsError, setPresetsError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const loadMetadata = useCallback(async (preset: string) => {
    setMetadataLoading(true);
    try {
      const metadata = await commands.generatePresetSuffix(preset);
      setPresetMetadata(metadata);
    } catch {
      setPresetMetadata(null);
    } finally {
      setMetadataLoading(false);
    }
  }, []);

  const refresh = useCallback(async () => {
    try {
      setLoading(true);
      const s = await commands.getSettings();
      setSettings(s);

      try {
        const p = await commands.listHandbrakePresets();
        setPresets(p);
        setPresetsError(null);
      } catch {
        setPresetsError("Could not load presets. Is HandBrakeCLI installed?");
        setPresets([]);
      }

      // The backend returns the default template when a preset has no stored suffix,
      // so a read never has to write one back (that write side-effect belonged in Rust).
      try {
        setPresetSuffix(await commands.getPresetSuffix(s.preset));
      } catch {
        setPresetSuffix("");
      }

      await loadMetadata(s.preset);
    } catch (e) {
      console.error("Failed to load settings:", e);
    } finally {
      setLoading(false);
    }
  }, [loadMetadata]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const updateSetting = useCallback(
    async (key: string, value: string) => {
      setError(null);
      // Optimistic: reflect the change immediately so controlled inputs and toggles don't
      // lag an IPC round-trip (and can't be reverted by an out-of-order get_settings).
      setSettings((prev) =>
        prev ? { ...prev, [key]: coerceSettingValue(prev[key as keyof AppSettings], value) } : prev,
      );

      try {
        await commands.updateSetting(key, value);
      } catch (e) {
        setError(`Couldn't save ${key}: ${e}`);
        // The optimistic value wasn't persisted; restore the stored truth.
        try {
          setSettings(await commands.getSettings());
        } catch {
          /* error already surfaced */
        }
        return;
      }

      if (key === "preset") {
        try {
          setPresetSuffix(await commands.getPresetSuffix(value));
        } catch {
          setPresetSuffix("");
        }
        await loadMetadata(value);
      }
    },
    [loadMetadata],
  );

  const updatePresetSuffix = useCallback(
    async (suffix: string) => {
      if (!settings) return;
      setError(null);
      setPresetSuffix(suffix); // optimistic; commit-on-blur means this is a discrete edit
      try {
        await commands.setPresetSuffix(settings.preset, suffix);
      } catch (e) {
        setError(`Couldn't save suffix: ${e}`);
      }
    },
    [settings],
  );

  const detectHandbrake = useCallback(async () => {
    const path = await commands.detectHandbrake();
    if (path) {
      await commands.updateSetting("handbrake_path", path);
      const s = await commands.getSettings();
      setSettings(s);
    }
    return path;
  }, []);

  return {
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
    refresh,
  };
}
