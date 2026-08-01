import { useState, useEffect, useCallback, useRef } from "react";
import { commands, type AppSettings, type PresetMetadata } from "../lib/tauri";
import { errorText, isPanic } from "../lib/errors";

// `update_setting` is stringly-typed on the wire ("true"/"false" for booleans); the
// optimistic merge must land booleans as real booleans so `checked={value === true}` works.
function coerceSettingValue(current: unknown, value: string): string | boolean | number {
  if (typeof current === "boolean") return value === "true";
  if (typeof current === "number") return Number(value);
  return value;
}

// `useSettings` fetches once on mount and the backend emits no settings-changed event, so two
// mounted instances (e.g. History and Settings, both mounted at once in the three-col layout)
// would otherwise drift apart forever once one of them writes. A module-level registry lets
// every mounted instance converge on a successful write without a backend event or schema
// change: the writing instance already knows the new value, so it shares it directly instead of
// making every other instance pay for its own get_settings round-trip.
type SettingsWriteListener = (key: string, value: string) => void;
const settingsWriteListeners = new Set<SettingsWriteListener>();

function broadcastSettingWrite(key: string, value: string) {
  for (const listener of settingsWriteListeners) listener(key, value);
}

/**
 * `withPresets: false` skips the preset pipeline — the preset list plus the selected preset's
 * suffix and metadata — for consumers that only read settings *values*. Every instance that asks
 * for it pays for `list_handbrake_presets`, which shells out to HandBrakeCLI on every call (the
 * server route has no cache, unlike `generate_preset_suffix`), so with History and Settings both
 * mounted in three-col, opening the app spawned two concurrent CLI processes for one list.
 * Under `withPresets: false` the preset pipeline never runs on load, so
 * `presets`/`presetSuffix`/`presetMetadata` must not be read.
 */
export function useSettings({ withPresets = true }: { withPresets?: boolean } = {}) {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [presets, setPresets] = useState<string[]>([]);
  const [presetSuffix, setPresetSuffix] = useState<string>("");
  const [presetMetadata, setPresetMetadata] = useState<PresetMetadata | null>(null);
  const [metadataLoading, setMetadataLoading] = useState(false);
  const [presetsError, setPresetsError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // Suffix + metadata both key off the selected preset; rapid preset switches can resolve
  // out of order, so stamp each load and apply only the latest (the useQueue monotonic guard).
  // The backend returns the default template when a preset has no stored suffix, so a read
  // never has to write one back (that write side-effect belonged in Rust).
  const latestPresetLoad = useRef(0);
  const loadPresetData = useCallback(async (preset: string) => {
    const requestId = ++latestPresetLoad.current;
    setMetadataLoading(true);
    const [suffix, metadata] = await Promise.allSettled([
      commands.getPresetSuffix(preset),
      commands.generatePresetSuffix(preset),
    ]);
    if (requestId !== latestPresetLoad.current) return; // superseded by a newer preset
    setPresetSuffix(suffix.status === "fulfilled" ? suffix.value : "");
    setPresetMetadata(metadata.status === "fulfilled" ? metadata.value : null);
    setMetadataLoading(false);
  }, []);

  const refresh = useCallback(async () => {
    try {
      setLoading(true);
      const s = await commands.getSettings();
      setSettings(s);

      if (withPresets) {
        try {
          const p = await commands.listHandbrakePresets();
          setPresets(p);
          setPresetsError(null);
        } catch (e) {
          // A panic here is a bug in ConvertBar, not a missing binary. Blaming the install would
          // send the user after the wrong thing — which is precisely the confusion the
          // discriminator exists to end, and this site swallowed the error entirely, so the fix
          // reached it only once the swallow was opened up.
          setPresetsError(
            isPanic(e) ? errorText(e) : "Could not load presets. Is HandBrakeCLI installed?",
          );
          setPresets([]);
        }

        await loadPresetData(s.preset);
      }
    } catch (e) {
      console.error("Failed to load settings:", e);
    } finally {
      setLoading(false);
    }
  }, [loadPresetData, withPresets]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Converge with every other mounted instance: apply a value another instance just wrote to
  // the backend, the same coercion the optimistic update below applies to our own writes.
  useEffect(() => {
    const listener: SettingsWriteListener = (key, value) => {
      setSettings((prev) =>
        prev ? { ...prev, [key]: coerceSettingValue(prev[key as keyof AppSettings], value) } : prev,
      );
    };
    settingsWriteListeners.add(listener);
    return () => {
      settingsWriteListeners.delete(listener);
    };
  }, []);

  const updateSetting = useCallback(
    async (key: string, value: string) => {
      setError(null);
      // Optimistic: reflect the change immediately so controlled inputs and toggles don't
      // lag an IPC round-trip (and can't be reverted by an out-of-order get_settings).
      // Capture the pre-edit value now (from the render closure) — capturing it inside the
      // updater is lazy and may not have run by the time the catch needs it.
      const restoreValue = settings?.[key as keyof AppSettings];
      setSettings((prev) =>
        prev ? { ...prev, [key]: coerceSettingValue(prev[key as keyof AppSettings], value) } : prev,
      );

      try {
        await commands.updateSetting(key, value);
      } catch (e) {
        setError(`Couldn't save ${key}: ${errorText(e)}`);
        // Restore only the failed key. A whole-object get_settings refetch can resolve out
        // of order and clobber a concurrent optimistic edit to a *different* key.
        setSettings((prev) =>
          prev && restoreValue !== undefined ? { ...prev, [key]: restoreValue } : prev,
        );
        return;
      }

      // Share the confirmed value with every other mounted useSettings instance instead of
      // letting each one pay for its own get_settings round-trip.
      broadcastSettingWrite(key, value);

      if (key === "preset") {
        await loadPresetData(value);
      }
    },
    [loadPresetData, settings],
  );

  const updatePresetSuffix = useCallback(
    async (suffix: string) => {
      if (!settings) return;
      setError(null);
      const previous = presetSuffix; // pre-edit value, captured before the optimistic overwrite
      setPresetSuffix(suffix); // optimistic; commit-on-blur means this is a discrete edit
      try {
        await commands.setPresetSuffix(settings.preset, suffix);
      } catch (e) {
        setError(`Couldn't save suffix: ${errorText(e)}`);
        // Roll back like updateSetting does, so SettingsPage's `suffixDraft !== presetSuffix`
        // commit guard still sees a diff and a re-blur retries instead of no-op'ing.
        setPresetSuffix(previous);
      }
    },
    [settings, presetSuffix],
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
