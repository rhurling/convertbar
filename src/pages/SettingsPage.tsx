import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useSettings } from "../hooks/useSettings";
import { commands } from "../lib/tauri";
import UpdatePanel from "../components/UpdatePanel";
import { isServerHead } from "../lib/head";
import { RELEASES_URL } from "../lib/releases";
import type { AppSettings, PresetMetadata } from "../lib/tauri";

// get_command_hooks / set_command_hook are desktop-only #[tauri::command]s that bypass the
// `commands` transport abstraction on purpose (CLAUDE.md, "Permissions (ACL)" background in
// task 7): they are absent from ALLOWED_KEYS so the server head's HTTP API can't reach them, so
// there is no httpCommands twin to unify them behind. Called only when !isServerHead.
interface CommandHooksResponse {
  postConvert: string;
  queueDrained: string;
}

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
  // Desktop's version display now lives inside UpdatePanel (via useUpdate/getUpdateState); this
  // is only rendered on the server head, which has no updater UI of its own.
  const [appVersion, setAppVersion] = useState<string>("");
  const [groupScoped, setGroupScoped] = useState(false);

  // Local drafts so text inputs echo keystrokes instantly and commit once (on blur/Enter),
  // instead of round-tripping an IPC write per character (which dropped/reordered characters).
  const [hbDraft, setHbDraft] = useState(settings?.handbrake_path ?? "");
  const [markerDraft, setMarkerDraft] = useState(settings?.watch_skip_marker ?? "");
  const [diskDraft, setDiskDraft] = useState(String(settings?.low_disk_min_gb ?? 0));
  const [suffixDraft, setSuffixDraft] = useState(presetSuffix);
  const [resolvedSuffix, setResolvedSuffix] = useState("");

  // Same commit-on-blur draft pattern as above, for the eight hook settings (task 3) plus the
  // desktop-only command hook (task 7, fetched separately below).
  const [pcUrlDraft, setPcUrlDraft] = useState(settings?.post_convert_webhook_url ?? "");
  const [pcHeadersDraft, setPcHeadersDraft] = useState(settings?.post_convert_webhook_headers ?? "");
  const [pcBodyDraft, setPcBodyDraft] = useState(settings?.post_convert_webhook_body ?? "");
  const [qdUrlDraft, setQdUrlDraft] = useState(settings?.queue_drained_webhook_url ?? "");
  const [qdHeadersDraft, setQdHeadersDraft] = useState(settings?.queue_drained_webhook_headers ?? "");
  const [qdBodyDraft, setQdBodyDraft] = useState(settings?.queue_drained_webhook_body ?? "");
  const [pathMapDraft, setPathMapDraft] = useState(settings?.hook_path_map ?? "");
  const [timeoutDraft, setTimeoutDraft] = useState(settings?.hook_timeout_seconds ?? "30");

  // Command hooks: fetched via get_command_hooks (not part of `settings`), so `null` here means
  // "not yet loaded" — distinct from "" (loaded, empty) — the same guard shape `commitDrafts`
  // uses for `settings` itself, so an unmount before the fetch lands writes nothing. Two
  // independent pairs, one per trigger — queue_drained is the primary use case (a library
  // rescan once per batch), so it gets full parity with post_convert, not a lesser field.
  const [pcCommandHook, setPcCommandHook] = useState<string | null>(null);
  const [pcCommandDraft, setPcCommandDraft] = useState("");
  const [qdCommandHook, setQdCommandHook] = useState<string | null>(null);
  const [qdCommandDraft, setQdCommandDraft] = useState("");

  // getAppInfo() works on both heads (desktop composes it from getVersion() internally, server
  // hits /api/info). The version is only rendered on the server head, but the priority caveat is
  // per-OS and so is needed on both.
  useEffect(() => {
    commands
      .getAppInfo()
      .then((info) => {
        setAppVersion(info.version);
        setGroupScoped(info.priority_is_group_scoped);
      })
      .catch(() => {});
  }, []);
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
  useEffect(() => {
    if (settings) setPcUrlDraft(settings.post_convert_webhook_url);
  }, [settings?.post_convert_webhook_url]);
  useEffect(() => {
    if (settings) setPcHeadersDraft(settings.post_convert_webhook_headers);
  }, [settings?.post_convert_webhook_headers]);
  useEffect(() => {
    if (settings) setPcBodyDraft(settings.post_convert_webhook_body);
  }, [settings?.post_convert_webhook_body]);
  useEffect(() => {
    if (settings) setQdUrlDraft(settings.queue_drained_webhook_url);
  }, [settings?.queue_drained_webhook_url]);
  useEffect(() => {
    if (settings) setQdHeadersDraft(settings.queue_drained_webhook_headers);
  }, [settings?.queue_drained_webhook_headers]);
  useEffect(() => {
    if (settings) setQdBodyDraft(settings.queue_drained_webhook_body);
  }, [settings?.queue_drained_webhook_body]);
  useEffect(() => {
    if (settings) setPathMapDraft(settings.hook_path_map);
  }, [settings?.hook_path_map]);
  useEffect(() => {
    if (settings) setTimeoutDraft(settings.hook_timeout_seconds);
  }, [settings?.hook_timeout_seconds]);

  // The server head can't serve this value (get_command_hooks is desktop-only), so it must never
  // be called there — not even to read. Fires once, when `settings` first finishes loading
  // (mirrors the rest of the page's gate on rendering at all), not on every settings change.
  useEffect(() => {
    if (isServerHead || !settings) return;
    invoke<CommandHooksResponse>("get_command_hooks")
      .then((hooks) => {
        setPcCommandHook(hooks.postConvert);
        setQdCommandHook(hooks.queueDrained);
      })
      .catch((e) => {
        // Swallowing this used to disable the fields permanently and invisibly: both hooks stay
        // `null`, which renders as "" (indistinguishable from "no hook configured") AND blocks
        // every write through the `!== null` guard in the commits below, with nothing to retry
        // it. Log it (the page's convention for a bespoke invoke that has no error banner), and
        // fall back to "" so the guard opens — a user who then edits the field writes a real
        // value rather than being silently ignored forever.
        console.error("Couldn't read the command hooks:", e);
        setPcCommandHook("");
        setQdCommandHook("");
      });
  }, [!!settings]);
  useEffect(() => {
    if (pcCommandHook !== null) setPcCommandDraft(pcCommandHook);
  }, [pcCommandHook]);
  useEffect(() => {
    if (qdCommandHook !== null) setQdCommandDraft(qdCommandHook);
  }, [qdCommandHook]);

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

  const commitPcUrl = () => {
    if (pcUrlDraft !== settings?.post_convert_webhook_url)
      updateSetting("post_convert_webhook_url", pcUrlDraft);
  };
  const commitPcHeaders = () => {
    if (pcHeadersDraft !== settings?.post_convert_webhook_headers)
      updateSetting("post_convert_webhook_headers", pcHeadersDraft);
  };
  const commitPcBody = () => {
    if (pcBodyDraft !== settings?.post_convert_webhook_body)
      updateSetting("post_convert_webhook_body", pcBodyDraft);
  };
  const commitQdUrl = () => {
    if (qdUrlDraft !== settings?.queue_drained_webhook_url)
      updateSetting("queue_drained_webhook_url", qdUrlDraft);
  };
  const commitQdHeaders = () => {
    if (qdHeadersDraft !== settings?.queue_drained_webhook_headers)
      updateSetting("queue_drained_webhook_headers", qdHeadersDraft);
  };
  const commitQdBody = () => {
    if (qdBodyDraft !== settings?.queue_drained_webhook_body)
      updateSetting("queue_drained_webhook_body", qdBodyDraft);
  };
  const commitPathMap = () => {
    if (pathMapDraft !== settings?.hook_path_map)
      updateSetting("hook_path_map", pathMapDraft);
  };
  const commitTimeout = () => {
    if (timeoutDraft !== settings?.hook_timeout_seconds)
      updateSetting("hook_timeout_seconds", timeoutDraft);
  };

  // Not routed through useSettings/updateSetting: set_command_hook is the bespoke desktop-only
  // command (see the CommandHooksResponse comment above), so this manages its own "last known
  // committed value" instead of relying on the settings object's optimistic-merge machinery.
  // The page has no general mechanism to surface a failed bespoke `invoke()` the way
  // `useSettings.updateSetting` surfaces failed setting writes via its `error` state (that
  // banner is wired to useSettings's own writes only) — console.error at minimum keeps a
  // rejected write from vanishing silently.
  // Takes the value explicitly rather than reading the draft, so `pick*Command` can persist the
  // path it just received: `setPcCommandDraft` does not update the state this closure captured.
  const savePcCommand = (command: string) => {
    if (pcCommandHook !== null && command !== pcCommandHook) {
      invoke("set_command_hook", { trigger: "post_convert", command })
        // Only advance the committed value on success — a rejected write must leave the old
        // "last known committed" value in place, so the guard above still sees a diff and the
        // next blur retries instead of silently believing the write already landed.
        .then(() => setPcCommandHook(command))
        .catch((e) => console.error("Couldn't save post-convert command hook:", e));
    }
  };
  const saveQdCommand = (command: string) => {
    if (qdCommandHook !== null && command !== qdCommandHook) {
      invoke("set_command_hook", { trigger: "queue_drained", command })
        .then(() => setQdCommandHook(command))
        .catch((e) => console.error("Couldn't save queue-drained command hook:", e));
    }
  };

  const commitPcCommand = () => savePcCommand(pcCommandDraft);
  const commitQdCommand = () => saveQdCommand(qdCommandDraft);

  // Commits on pick, NOT draft-only. Persistence cannot ride on the input's `onBlur` here:
  // clicking Browse focuses the button (and on macOS WKWebView may take no focus at all), so
  // the input never blurs — the user saw the path appear, quit from the Settings tab, and the
  // hook was never saved. Worse, when the input *was* focused, the blur committed the
  // PRE-pick draft and then stranded the picked value. Cancel stays a no-op.
  const pickPcCommand = async () => {
    const path = await invoke<string | null>("pick_file").catch((e) => {
      console.error("Couldn't open the file picker:", e);
      return null;
    });
    if (path !== null) {
      setPcCommandDraft(path);
      savePcCommand(path);
    }
  };
  const pickQdCommand = async () => {
    const path = await invoke<string | null>("pick_file").catch((e) => {
      console.error("Couldn't open the file picker:", e);
      return null;
    });
    if (path !== null) {
      setQdCommandDraft(path);
      saveQdCommand(path);
    }
  };

  // Commit-on-blur alone loses data: crossing the 1300px layout breakpoint swaps App's two JSX
  // trees and remounts this page (browser zoom crosses it without touching the window), and an
  // unmount fires no `blur` — so a typed-but-unblurred draft was silently discarded. Flushing the
  // same commits the blur handlers run keeps the edit. Held in a ref because the unmount effect
  // must run *only* on unmount, and a bare `[]` effect would capture the first render's drafts.
  const commitDrafts = () => {
    // Before get_settings lands, every draft still holds a placeholder ("" / "0"). Committing
    // those would persist them over the user's stored values, so a pre-load unmount writes
    // nothing; each commit's own equality guard covers the loaded case. This is also what keeps
    // StrictMode's dev-only mount/unmount/mount cycle from writing anything.
    // Both independently guarded by their own `*CommandHook !== null`, not by `settings`.
    commitPcCommand();
    commitQdCommand();
    if (!settings) return;
    commitHbPath();
    commitMarker();
    commitDisk();
    commitSuffix();
    commitPcUrl();
    commitPcHeaders();
    commitPcBody();
    commitQdUrl();
    commitQdHeaders();
    commitQdBody();
    commitPathMap();
    commitTimeout();
  };
  const commitDraftsRef = useRef(commitDrafts);
  useEffect(() => {
    commitDraftsRef.current = commitDrafts;
  });
  useEffect(() => () => commitDraftsRef.current(), []);

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

            {/* Exactly-empty, not `.trim()`: the engine calls a job in-place only when the
                output path equals the source, and a whitespace suffix still yields a distinct
                `vacation .mp4`. Trimming here warned about skips that never happen. */}
            {resolvedSuffix === "" && (
              <div className="suffix-inplace-note">
                Empty suffix: mp4 files are re-encoded in place, replacing the original. The fast
                &quot;already converted&quot; skip-by-suffix is also disabled.
                {settings.cleanup_mode === "keep" && (
                  <>
                    {" "}
                    <strong>
                      An in-place re-encode cannot keep the original — there is only one file. These
                      files will be skipped until you set a suffix or switch to{" "}
                      {isServerHead ? "Delete" : "Trash or Delete"}.
                    </strong>
                  </>
                )}
              </div>
            )}
          </>
        )}
      </div>

      <div className="setting-group">
        <label className="setting-label">After conversion</label>
        <div className="setting-radios">
          {/* No Trash on the server head: the `trash` crate litters .Trash-<uid>
              directories on the NAS mounts a headless deployment runs against. */}
          {!isServerHead && (
            <label className="radio-label">
              <input
                type="radio"
                name="cleanup"
                checked={settings.cleanup_mode === "trash"}
                onChange={() => updateSetting("cleanup_mode", "trash")}
              />
              Move original to Trash
            </label>
          )}
          <label className="radio-label">
            <input
              type="radio"
              name="cleanup"
              checked={settings.cleanup_mode === "delete"}
              onChange={() => updateSetting("cleanup_mode", "delete")}
            />
            Delete original permanently
          </label>
          <label className="radio-label">
            <input
              type="radio"
              name="cleanup"
              checked={settings.cleanup_mode === "keep"}
              onChange={() => updateSetting("cleanup_mode", "keep")}
            />
            Keep both files
          </label>
        </div>
        <p className="setting-hint">
          Keep both files deletes nothing. Use it to check the encodes are good on this
          machine, remove the originals yourself, then switch to Delete once you trust the
          results.
        </p>
      </div>

      <div className="setting-group">
        <label className="setting-label">Encode priority</label>
        <div className="setting-radios">
          <label className="radio-label">
            <input
              type="radio"
              name="encode-priority"
              checked={settings.encode_priority === "normal"}
              onChange={() => updateSetting("encode_priority", "normal")}
            />
            Normal — compete equally with other apps
          </label>
          <label className="radio-label">
            <input
              type="radio"
              name="encode-priority"
              checked={settings.encode_priority === "low"}
              onChange={() => updateSetting("encode_priority", "low")}
            />
            Low — yield to other apps when the CPU is busy
          </label>
          <label className="radio-label">
            <input
              type="radio"
              name="encode-priority"
              checked={settings.encode_priority === "idle"}
              onChange={() => updateSetting("encode_priority", "idle")}
            />
            Idle — run only with CPU nothing else wants
          </label>
        </div>
        <p className="setting-hint">
          This is not a CPU limit: encodes still use every core nothing else wants. It applies
          to the next encode, not one already running.
        </p>
        {groupScoped && (
          <p className="setting-hint">
            On Linux this often has little effect — the kernel confines priority to a process
            group, so the encode yields to ConvertBar itself rather than to the rest of the
            machine. To free CPU for other work, use <code>--cpu-shares</code> on the Docker
            container or <code>CPUWeight=</code> on the systemd unit.
          </p>
        )}
      </div>

      <div className="setting-group">
        <label className="setting-label">Bad source files</label>
        <p className="setting-hint">
          Files ConvertBar could not read, or that turned out to be incomplete
          downloads, are listed in History. Nothing is removed until you choose to.
        </p>
        {isServerHead ? (
          <p className="setting-hint">
            Bad source files are deleted permanently — the server has no Trash to move them to.
          </p>
        ) : (
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
        )}
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
        <label className="setting-label" htmlFor="low-disk-min-gb">
          Pause when destination free space is low
        </label>
        <div className="setting-row">
          <input
            id="low-disk-min-gb"
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

      <h2 className="setting-section-heading">Hooks</h2>

      <div className="setting-group">
        <label className="setting-label" htmlFor="post-convert-url">
          After each conversion — URL
        </label>
        <input
          id="post-convert-url"
          className="setting-input"
          type="text"
          value={pcUrlDraft}
          onChange={(e) => setPcUrlDraft(e.target.value)}
          onBlur={commitPcUrl}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
          placeholder="https://example.com/webhook"
        />
        <label className="setting-label" htmlFor="post-convert-headers">
          After each conversion — Headers
        </label>
        <textarea
          id="post-convert-headers"
          className="setting-input"
          rows={2}
          value={pcHeadersDraft}
          onChange={(e) => setPcHeadersDraft(e.target.value)}
          onBlur={commitPcHeaders}
          placeholder="ApiKey: your-key"
        />
        <label className="setting-label" htmlFor="post-convert-body">
          After each conversion — Body
        </label>
        <textarea
          id="post-convert-body"
          className="setting-input"
          rows={3}
          value={pcBodyDraft}
          onChange={(e) => setPcBodyDraft(e.target.value)}
          onBlur={commitPcBody}
          placeholder="Leave empty to send the raw JSON payload"
        />
        {!isServerHead && (
          <>
            <label className="setting-label" htmlFor="post-convert-command">
              Command to run after each conversion
            </label>
            <div className="setting-row">
              <input
                id="post-convert-command"
                className="setting-input flex-1"
                type="text"
                value={pcCommandDraft}
                onChange={(e) => setPcCommandDraft(e.target.value)}
                onBlur={commitPcCommand}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                }}
                placeholder="/path/to/script.sh"
              />
              <button
                className="btn btn-small"
                onClick={pickPcCommand}
                aria-label="Choose a script to run after each conversion"
              >
                Browse
              </button>
            </div>
          </>
        )}
        <p className="setting-hint">
          Fires after every file finishes converting, whether it succeeded or failed. Leave the
          URL and command empty to disable.
        </p>
      </div>

      <div className="setting-group">
        <label className="setting-label" htmlFor="queue-drained-url">
          When the queue finishes — URL
        </label>
        <input
          id="queue-drained-url"
          className="setting-input"
          type="text"
          value={qdUrlDraft}
          onChange={(e) => setQdUrlDraft(e.target.value)}
          onBlur={commitQdUrl}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
          placeholder="https://example.com/webhook"
        />
        <label className="setting-label" htmlFor="queue-drained-headers">
          When the queue finishes — Headers
        </label>
        <textarea
          id="queue-drained-headers"
          className="setting-input"
          rows={2}
          value={qdHeadersDraft}
          onChange={(e) => setQdHeadersDraft(e.target.value)}
          onBlur={commitQdHeaders}
          placeholder="ApiKey: your-key"
        />
        <label className="setting-label" htmlFor="queue-drained-body">
          When the queue finishes — Body
        </label>
        <textarea
          id="queue-drained-body"
          className="setting-input"
          rows={3}
          value={qdBodyDraft}
          onChange={(e) => setQdBodyDraft(e.target.value)}
          onBlur={commitQdBody}
          placeholder="Leave empty to send the raw JSON payload"
        />
        {!isServerHead && (
          <>
            <label className="setting-label" htmlFor="queue-drained-command">
              Command to run when the queue finishes
            </label>
            <div className="setting-row">
              <input
                id="queue-drained-command"
                className="setting-input flex-1"
                type="text"
                value={qdCommandDraft}
                onChange={(e) => setQdCommandDraft(e.target.value)}
                onBlur={commitQdCommand}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                }}
                placeholder="/path/to/script.sh"
              />
              <button
                className="btn btn-small"
                onClick={pickQdCommand}
                aria-label="Choose a script to run when the queue finishes"
              >
                Browse
              </button>
            </div>
          </>
        )}
        <p className="setting-hint">
          Fires once the queue empties — not on every pause, only a true drain. Leave the URL
          and command empty to disable.
        </p>
      </div>

      <div className="setting-group">
        <label className="setting-label" htmlFor="hook-path-map">
          Path mapping
        </label>
        <textarea
          id="hook-path-map"
          className="setting-input"
          rows={3}
          value={pathMapDraft}
          onChange={(e) => setPathMapDraft(e.target.value)}
          onBlur={commitPathMap}
          placeholder="/media => /data"
        />
        <p className="setting-hint">
          One rule per line. Applies to webhooks only — a command hook receives raw paths.
        </p>
      </div>

      <div className="setting-group">
        <label className="setting-label" htmlFor="hook-timeout">
          Timeout (seconds)
        </label>
        <input
          id="hook-timeout"
          className="setting-input"
          type="number"
          min="1"
          step="1"
          value={timeoutDraft}
          onChange={(e) => setTimeoutDraft(e.target.value)}
          onBlur={commitTimeout}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
        />
        <p className="setting-hint">
          Per hook. With both a webhook and a command configured, a dead receiver costs twice
          this per job.
        </p>
      </div>

      {isServerHead && (
        <div className="setting-group">
          <span className="setting-label">Command hooks</span>
          <p className="setting-hint">
            Set by environment variable on the server head (
            <code>CONVERTBAR_POST_CONVERT_COMMAND</code> and{" "}
            <code>CONVERTBAR_QUEUE_DRAINED_COMMAND</code>).
          </p>
        </div>
      )}

      <div className="setting-group">
        <label className="setting-label">History</label>
        <div className="setting-toggles">
          <label className="toggle-label">
            <input
              type="checkbox"
              checked={settings.history_show_duration}
              onChange={(e) =>
                updateSetting("history_show_duration", String(e.target.checked))
              }
            />
            Show processing time
          </label>
        </div>
      </div>

      {/* Menu bar display, notifications, and launch-at-login are all macOS menu-bar-app
          concepts with no equivalent on a headless server. */}
      {!isServerHead && (
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
      )}

      {!isServerHead && (
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
      )}

      {!isServerHead && (
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
      )}

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

      {/* The auto-updater (UpdatePanel) is a desktop-only concept — it downloads and relaunches
          the app bundle. The server head has no equivalent; it just shows its running version,
          updated by redeploying. */}
      {isServerHead ? (
        <div className="setting-group">
          <label className="setting-label">
            Version {appVersion && <span className="version-label">v{appVersion}</span>}
          </label>
          {/* Unlike the desktop head, nothing here ever checks whether a newer release exists, so
              this link can't be gated on one being available — it is the only path from a server
              deployment to "is there something newer than what I redeployed?". */}
          <a className="update-release-link" href={RELEASES_URL} target="_blank" rel="noreferrer">
            Release notes ↗
          </a>
        </div>
      ) : (
        <UpdatePanel />
      )}

      {/* quitApp() has no server equivalent (there's no local app process for the user to quit). */}
      {!isServerHead && (
        <div className="setting-group setting-group-quit">
          <button
            className="btn btn-quit"
            onClick={() => commands.quitApp()}
          >
            Quit ConvertBar
          </button>
        </div>
      )}
    </div>
  );
}
