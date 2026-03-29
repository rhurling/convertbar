import { useState, useEffect, useCallback } from "react";
import { commands, type AppSettings } from "../lib/tauri";

export function useSettings() {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [presets, setPresets] = useState<string[]>([]);
  const [presetSuffix, setPresetSuffix] = useState<string>("");
  const [presetsError, setPresetsError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

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

      try {
        const suffix = await commands.getPresetSuffix(s.preset);
        setPresetSuffix(suffix || "");
      } catch {
        setPresetSuffix("");
      }
    } catch (e) {
      console.error("Failed to load settings:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const updateSetting = useCallback(
    async (key: string, value: string) => {
      await commands.updateSetting(key, value);
      const s = await commands.getSettings();
      setSettings(s);

      if (key === "preset") {
        try {
          const suffix = await commands.getPresetSuffix(value);
          setPresetSuffix(suffix || "");
        } catch {
          setPresetSuffix("");
        }
      }
    },
    [],
  );

  const updatePresetSuffix = useCallback(
    async (suffix: string) => {
      if (!settings) return;
      await commands.setPresetSuffix(settings.preset, suffix);
      setPresetSuffix(suffix);
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
    presetsError,
    loading,
    updateSetting,
    updatePresetSuffix,
    detectHandbrake,
    refresh,
  };
}
