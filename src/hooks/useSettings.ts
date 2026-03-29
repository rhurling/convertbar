import { useState, useEffect, useCallback } from "react";
import { commands, type AppSettings, type PresetMetadata } from "../lib/tauri";

const DEFAULT_SUFFIX_TEMPLATE = ".{resolution}-{codec}";

export function useSettings() {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [presets, setPresets] = useState<string[]>([]);
  const [presetSuffix, setPresetSuffix] = useState<string>("");
  const [presetMetadata, setPresetMetadata] = useState<PresetMetadata | null>(null);
  const [metadataLoading, setMetadataLoading] = useState(false);
  const [presetsError, setPresetsError] = useState<string | null>(null);
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

      try {
        const suffix = await commands.getPresetSuffix(s.preset);
        if (suffix) {
          setPresetSuffix(suffix);
        } else {
          await commands.setPresetSuffix(s.preset, DEFAULT_SUFFIX_TEMPLATE);
          setPresetSuffix(DEFAULT_SUFFIX_TEMPLATE);
        }
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
      await commands.updateSetting(key, value);
      const s = await commands.getSettings();
      setSettings(s);

      if (key === "preset") {
        try {
          const suffix = await commands.getPresetSuffix(value);
          if (suffix) {
            setPresetSuffix(suffix);
          } else {
            await commands.setPresetSuffix(value, DEFAULT_SUFFIX_TEMPLATE);
            setPresetSuffix(DEFAULT_SUFFIX_TEMPLATE);
          }
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
    presetMetadata,
    metadataLoading,
    presetsError,
    loading,
    updateSetting,
    updatePresetSuffix,
    detectHandbrake,
    refresh,
  };
}
