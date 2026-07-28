# ConvertBar

A menu bar app for batch video conversion with [HandBrakeCLI](https://handbrake.fr/).
Drop files in, watch the progress next to the clock, reclaim the disk space.

ConvertBar lives entirely in the menu bar (or system tray) — no dock icon, no
main window. It queues conversions one at a time, keeps whichever file ends up
smaller, and records what it saved.

Built with [Tauri 2](https://tauri.app/), React, and Rust. macOS, Windows, and Linux.

## Requirements

**HandBrakeCLI must be installed and on your `PATH`.** ConvertBar drives it; it
does not bundle or install it.

| Platform | Install |
|---|---|
| macOS | `brew install handbrake` |
| Windows | `winget install HandBrake.HandBrake.CLI` |
| Linux | `apt install handbrake-cli` (or your distro's equivalent) |

If ConvertBar can't find it, the Queue tab shows a warning banner, and you can
set an explicit path in Settings.

## Install

Grab the latest build from the [Releases page](https://github.com/rhurling/convertbar/releases/latest):

- **macOS** — `ConvertBar_x.y.z_aarch64.dmg` (Apple Silicon) or `_x64.dmg` (Intel)
- **Windows** — `ConvertBar_x.y.z_x64-setup.exe` or `_x64_en-US.msi`
- **Linux** — `.AppImage`, `.deb`, or `.rpm`

### First launch

The binaries are **not code-signed**, so both desktop platforms will warn you the
first time. This is expected — ConvertBar is a free hobby project and the
certificates cost several hundred euros a year.

**macOS** — Gatekeeper refuses a plain double-click. Right-click ConvertBar.app
→ **Open** → **Open** in the dialog. You only do this once. If macOS insists the
app is "damaged", clear the quarantine flag:

```
xattr -dr com.apple.quarantine /Applications/ConvertBar.app
```

**Windows** — SmartScreen shows a blue banner. Click **More info** → **Run anyway**.

Once installed, ConvertBar updates itself: it checks for new releases and can
install them from Settings → Updates. Updates are cryptographically signed, so
you only need the manual step for the very first install.

## What it does

**Queue** — Drag video files or whole folders onto the drop zone. Folders are
scanned recursively. Reorder with the drag handles, remove items before they run,
pause or cancel the active job. On macOS, pause is a real process freeze
(`SIGSTOP`/`SIGCONT`), so encoding resumes exactly where it stopped instead of
starting the file over. Other platforms pause at the queue level, between files.

**Menu bar progress** — Percentage, ETA, fps, filename, and queue count, each one
toggleable, so you can keep the readout as terse or as verbose as you like.

**History** — Every finished conversion with original → converted size, the
percentage saved, and which file was kept. A running total sits at the top.
ConvertBar keeps whichever file is smaller, so an encode that came out *larger*
keeps the original and is flagged accordingly.

**Watched folders** — Point ConvertBar at a directory and it converts new videos
as they land, which makes it a reasonable back end for a download client. A
configurable marker file lets you exclude specific folders from the sweep.

**Bad source detection** — Files that are unreadable or that turn out to be
incomplete downloads are set aside in History rather than being retried forever.
Nothing is removed until you explicitly purge them.

**Skip rules** — Files already carrying the output suffix, files whose output
already exists, and (optionally) files whose codec and resolution already meet the
target are skipped instead of being pointlessly re-encoded.

**Low disk guard** — Set a floor in GB and the queue pauses rather than filling
the destination volume.

## How a conversion works

1. ConvertBar runs HandBrakeCLI with your chosen preset. The default is
   *H.265 Apple VideoToolbox 1080p* on macOS, *H.265 NVENC 1080p* on Windows, and
   *H.265 MKV 1080p30* on Linux — all hardware-accelerated where available.
2. The output filename gets a suffix built from a template. The default,
   `.{resolution}-{codec}`, turns `vacation.mkv` into `vacation.1080p-h265.mp4`.
   Available variables: `{codec}`, `{resolution}`, `{quality}`, `{preset}`,
   `{device}` — all read from the preset's own metadata.
3. When the encode finishes, the two files are compared and the larger one is
   removed — moved to the Trash by default, or deleted permanently if you prefer.

Encoding is deliberately sequential: hardware encoders would just contend for the
same silicon if run in parallel, so two at once is usually slower overall.

## Development

```sh
npm install
npm run tauri dev     # run the app
npm test              # frontend tests (Vitest)
npm run build         # type-check + production frontend build
cd src-tauri && cargo test
```

`docs/RECOMMENDATIONS.md` is the live backlog and `docs/OPEN_ISSUES.md` holds
larger unstarted ideas. `docs/archive/` holds completed history — the original
design spec (`docs/archive/SPEC.md`, largely superseded), shipped feature
specs/plans, and two rounds of code review. `CLAUDE.md` and the implementation
are the source of truth over any of it.

Releases go through `scripts/release.sh`, which bumps every manifest, rebuilds,
opens a PR, and tags the merged commit to trigger the multi-platform CI build.

## License

MIT — see [LICENSE](LICENSE).

HandBrake itself is a separate GPL-2.0 project. ConvertBar invokes `HandBrakeCLI`
as an external program and does not link against or redistribute it.
