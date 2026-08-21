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

**Encode priority** — Normal, Low, or Idle, lowering the HandBrakeCLI child
process's scheduling priority so it yields CPU to whatever else you're running.
It's a proportional share, not a cap: a lowered encode still uses every core
nothing else wants. Fresh desktop installs default to Low; existing installs
stay on Normal so nobody's encodes change speed after an update. On Linux this
setting largely has no effect — kernel autogrouping and cgroup CPU controllers
both confine a nice value to the process's own scheduling group — so use
`--cpu-shares` on a Docker container or `CPUWeight=` on a systemd unit instead.

## How a conversion works

1. ConvertBar runs HandBrakeCLI with your chosen preset. The default is
   *H.265 Apple VideoToolbox 1080p* on macOS, *H.265 NVENC 1080p* on Windows, and
   *H.265 MKV 1080p30* on Linux — all hardware-accelerated where available.
2. The output filename gets a suffix built from a template. The default,
   `.{resolution}-{codec}`, turns `vacation.mkv` into `vacation.1080p-h265.mp4`.
   Available variables: `{codec}`, `{resolution}`, `{quality}`, `{preset}`,
   `{device}` — all read from the preset's own metadata.
3. When the encode finishes, the two files are compared and the larger one is
   removed, according to your **After conversion** setting.

**After conversion** — what happens to the file that loses on size:

- **Move original to Trash** (desktop only) — recoverable from the OS Trash.
- **Delete original permanently** — the default on the server head; a headless
  deployment has no Trash, and the `trash` crate litters `.Trash-<uid>` folders on
  NAS mounts.
- **Keep both files** — nothing is deleted. This is an evaluation mode: run a batch,
  check the encodes are good on your hardware, delete the originals yourself, then
  switch to Delete. History still shows how much each encode saved, so you can judge
  the result before committing to it.

Four things to know about Keep:

- An empty output suffix re-encodes in place, so there is no second file to keep.
  Those files are skipped with a note until you set a suffix or leave Keep. Only Keep
  blocks them: Trash runs an in-place job too, and puts the original in the Trash
  rather than deleting it.
- While originals are kept, ConvertBar avoids re-converting them by remembering each
  file's size and modification time in History. Clearing History forgets that, and a
  watched folder will convert those files again into renumbered outputs
  (`movie (1).1080p-h265.mp4`).
- History's savings figure is labeled "Potential savings" rather than "Total saved"
  while Keep is active: it is still the same original-minus-encoded delta per file,
  but under Keep neither file has actually been removed, so nothing has been freed yet.
- **Before rolling back to an older ConvertBar version, switch back to Trash or
  Delete first.** A pre-Keep binary compares `cleanup_mode` against the literal
  string `"delete"`; `"keep"` fails that check and falls through to the trash branch
  instead, so a routine version rollback silently stops keeping anything on its next
  batch. On the desktop app that branch moves each original to the Trash, where it
  is recoverable; on the server head, which has no Trash, it deletes them for good.

Encoding is deliberately sequential: hardware encoders would just contend for the
same silicon if run in parallel, so two at once is usually slower overall.

## Post-convert hooks

ConvertBar can notify an external system after each conversion, and once when a queue run
finishes. Configure both under Settings → Hooks, in the desktop app or the web UI.

**Two trigger points:**

- **After each conversion** — fires once a file reaches a terminal state (`done`, `skipped`, or
  `error`), even on failure. A cancelled job fires nothing — it's a user action, not a conversion
  result.
- **When the queue finishes** — fires once per true drain (the queue genuinely ran out of work),
  not on every pause. Jobs are reported in batches of up to 100; a backlog larger than that sends
  several payloads in a row from one drain rather than one giant one. **The first drain after you
  configure the hook reports every job already in History**, since there is no prior watermark to
  start from — expect a burst the first time, or right after an upgrade. A failed hook does not
  advance past the jobs it tried to report, so **receivers must be idempotent**: the same batch is
  resent on the next drain rather than lost.

Each trigger point supports two independent mechanisms, which can be combined:

**Webhook** — URL, headers (one `Name: value` per line), and an optional body template. Leave the
body empty to send the full JSON payload with `Content-Type: application/json`. A non-empty body
is a template:

- `{{field}}` (e.g. `{{status}}`, `{{result_path}}`) substitutes a JSON-escaped scalar, safe to
  drop inside a string literal: `"path": "{{result_path}}"`.
- `{{field_json}}` (note the `_json` suffix — e.g. `{{output_dirs_json}}`, `{{payload_json}}`)
  substitutes raw, already-valid JSON. It belongs at a JSON *value* position, never inside a
  string literal — putting it inside quotes would splice unescaped quotes into that string and
  produce an invalid body. This is why the Stash example below passes paths as a GraphQL
  **variable** instead of interpolating them into the query text.

**Command** (desktop UI only, with a Browse button to pick the script — see below for the server
head) — a path to a script or executable, run with no shell. It receives the payload as
`CONVERTBAR_<FIELD>` environment variables (e.g. `CONVERTBAR_STATUS`, `CONVERTBAR_RESULT_PATH`)
plus `CONVERTBAR_PAYLOAD`, the whole JSON payload. Each individual environment value is capped at
96 KiB; a value over that fails with a message naming the variable rather than an opaque `E2BIG`
from the OS — use the webhook instead if a payload might get that large. Path mapping (below)
does **not** apply to the command hook: it gets raw paths, and a script can rewrite them itself.

**Path mapping** rewrites path fields in the webhook payload only, one `from => to` rule per
line. The longest matching prefix wins regardless of line order, and a trailing slash on either
side is tolerated (`/media/ => /data/` and `/media => /data` are equivalent).

### Example: make Stash rescan after a batch

Set the **queue-drained** webhook to:

| Field | Value |
|---|---|
| URL | `http://stash:9999/graphql` |
| Headers | `ApiKey: your-stash-api-key` |
| Body | `{"query":"mutation($input: ScanMetadataInput!) { metadataScan(input: $input) }","variables":{"input":{"paths":{{output_dirs_json}}}}}` |

If Stash mounts the same media at a different path, add a path-map rule — `/media => /data`.

The paths are passed as a GraphQL **variable**, not interpolated into the query string, precisely
because `{{output_dirs_json}}` inserts raw JSON: at the `variables` value position that's exactly
right, but splicing it into the quoted `query` string would inject unescaped `"` characters and
break the request. Keep it as a variable rather than "simplifying" it back into the query text.

**Command hooks on the server head** are configured only by environment variable —
`CONVERTBAR_POST_CONVERT_COMMAND` and `CONVERTBAR_QUEUE_DRAINED_COMMAND` — never through the web
UI or the HTTP API. See [Environment variables](#environment-variables) below.

Webhook headers are stored in plaintext in `convertbar.db` and are readable by any authenticated
web-UI user, and an authenticated user can point the webhook at any address the container can
reach. Both are the same trust class as the auth token itself.

## Server (Docker)

ConvertBar also ships as a headless server image with a browser UI, for running
on a NAS or home server instead of (or alongside) the desktop app.

```sh
docker pull ghcr.io/rhurling/convertbar:latest
```

See [`docker-compose.example.yml`](docker-compose.example.yml) for a ready-to-copy
compose file.

The web UI takes files through the picker, not by dragging them onto the page — a
browser tab receives no OS drag-drop event. Click **Add files or folders…** in the
intake panel on the Queue tab to browse. Inside the picker, every row has a checkbox
(folders included, added recursively), the header selects everything in the current
folder, shift-click selects a range, and the selection survives moving between
folders. Reordering the queue by dragging still works.

### Unraid

[`unraid-template.xml`](unraid-template.xml) is a container template for Unraid's
Docker tab (Add Container → Template → paste the raw URL). Two Unraid-specific
caveats it also documents inline:

- **Watched folders need a disk or cache path**, not a `/mnt/user` share. User
  shares are a FUSE overlay and don't deliver inotify events reliably, so files
  dropped there may not be noticed until the container restarts and rescans.
  Ad-hoc adds via the web file browser work on any path.
- **Reaching the UI by hostname requires `CONVERTBAR_ALLOWED_HOSTS`** (e.g.
  `tower.local`); IP addresses always work. Without it those requests are
  rejected with HTTP 421.

### Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `CONVERTBAR_AUTH_TOKEN` | *(none)* | Bearer/cookie token required on every `/api/*` request. Set this **or** `CONVERTBAR_NO_AUTH=1` — see [Auth](#auth) below. |
| `CONVERTBAR_NO_AUTH` | *(none)* | Set to `1` to disable auth entirely. Only do this behind a trusted network or a reverse proxy that already handles auth. |
| `CONVERTBAR_BIND` | `0.0.0.0` | Address to listen on. |
| `CONVERTBAR_PORT` | `8080` | Port to listen on. |
| `CONVERTBAR_ALLOWED_HOSTS` | *(none)* | Comma-separated extra `Host` header values to accept (anti DNS-rebinding). Localhost and IP literals are always allowed; needed if you browse by hostname instead of IP (e.g. `nas.local`) or use a reverse proxy. |
| `CONVERTBAR_BROWSE_ROOTS` | `/` | Colon-separated paths the web file browser may navigate. Restrict it to your media mount(s), e.g. `/media`. |
| `CONVERTBAR_TRUSTED_PROXIES` | *(none)* | Comma-separated IPs or CIDR ranges whose `X-Forwarded-For` header is believed, so login throttling counts real client addresses instead of the proxy's. **Set this as narrowly as possible** — see [Auth](#auth). |
| `CONVERTBAR_POST_CONVERT_COMMAND` | *(none)* | Script or executable to run after each conversion — see [Post-convert hooks](#post-convert-hooks). Not configurable through the web UI or the HTTP API; environment-variable-only by design. |
| `CONVERTBAR_QUEUE_DRAINED_COMMAND` | *(none)* | Script or executable to run when the queue finishes a true drain. Same restriction as above. |
| `PUID` | `0` | UID to drop to after start. `0` keeps the container's starting user (root, unless you pass `--user`). See [Volumes and permissions](#volumes-and-permissions). |
| `PGID` | `0` | GID to drop to after start. |

### Auth

The server refuses to start unless `CONVERTBAR_AUTH_TOKEN` or
`CONVERTBAR_NO_AUTH=1` is set — there is no unauthenticated-by-default mode.
`CONVERTBAR_NO_AUTH=1` is only for a trusted LAN or a deployment where a reverse
proxy already gates access.

**Token requirements.** `CONVERTBAR_AUTH_TOKEN` must be at least 16 characters
long and use at least 8 distinct characters; anything weaker is refused at
startup rather than warned about. Generate one with:

```sh
openssl rand -base64 24
```

**Failed-attempt throttling.** Each source — identified at `/api/login`, via an
`Authorization` header, or via the session cookie (used by `/api/events`,
which can't send headers) — gets 8 free attempts. After that it may be
evaluated only once per interval: 500 ms, then 1 s, 2 s, 4 s, and so on to a
30-second ceiling. Every attempt outside that interval is refused with 401
**without comparing the credential at all — even a correct token is refused
while a source is gated.** That refusal is the rate limit working as intended,
not a bug: if you're locked out, wait for the next interval, since retrying
faster does not help. A successful sign-in clears the source immediately, and
a source's history is forgotten 15 minutes after its first attempt. Nothing
sleeps — every response, allowed or refused, is immediate.

This bounds a single source; it does not stop an attacker spread across many
source addresses, since each gets its own free allowance. **A randomly
generated token is what actually protects the server** — the floor above
permits a memorable passphrase like `Sommer2026!Berlin`, which is not
comfortable at any guess rate.

Rotating the token means changing the variable and restarting the container.
Open browser tabs will be signed out and can usually log in again immediately —
but each tab retries its stale cookie against the same ~4-request fan-out, so
with 3 or more tabs open the retries alone (~12) can exceed the free allowance
(8) and gate the source; if that happens, the first login attempt with the
correct token is refused and needs a retry after a short wait. A script looping
on an outdated token will quickly ramp itself into the same gating any attacker
gets — refused outright, not merely slowed — which is working as intended and
indistinguishable from one.

**Behind a reverse proxy**, every request appears to come from the proxy, so all
clients share one throttling ramp. Set `CONVERTBAR_TRUSTED_PROXIES` to the
proxy's address to have `X-Forwarded-For` believed instead:

```
CONVERTBAR_TRUSTED_PROXIES=172.18.0.5
```

> Set it as narrowly as possible. Every address listed is trusted to assert who
> it is, so a range that contains *clients* rather than only the proxy lets each
> of them forge a fresh identity per request and skip throttling entirely —
> worse than leaving it unset. Do not use a whole Docker bridge network
> (`172.18.0.0/16`) or a LAN range; pin the proxy to a static address and list
> that. This cannot help behind plain NAT, where there is no forwarded header.
>
> Without it, everyone behind the proxy shares one bucket, so one attacker's
> guesses can gate — and, if sustained, keep gated — every legitimate user's
> login on that address, not just slow their failed attempts. It also means a
> legitimate user's successful request resets the *whole* shared bucket (a
> successful login clears its own ramp outright), handing the attacker a fresh
> 8-guess allowance each time someone else logs in — another reason to trust
> only the proxy's exact address.

### Reverse proxy / HTTPS

The server itself speaks plain HTTP only. Put a reverse proxy (Caddy, nginx,
Traefik, …) in front of it for TLS termination if you expose it beyond your LAN.

### Volumes and permissions

`/config` holds the SQLite database and probe cache — mount it to persist state
across container restarts (see `docker-compose.example.yml`).

The preferred way to run unprivileged is `PUID`/`PGID`: the container starts as
root, chowns `/config`, then drops to that uid:gid before starting the server —
so everything ConvertBar (and the `HandBrakeCLI` processes it forks) writes into
your media mounts lands with that ownership instead of `root:root`. Make sure
mounted media directories are writable by that uid/gid; the entrypoint
deliberately never chowns them. Starting as root is what keeps this compatible
with entrypoint-hook integrations that need it, such as Unraid's Tailscale
integration — which is exactly what breaks under `--user`.

`docker run --user <uid>:<gid>` still works: the entrypoint detects it isn't
root, ignores `PUID`/`PGID`, and execs the server directly. In that mode you
must make `/config` and the media mounts writable by that uid/gid yourself.

Either way, run with `--init` (or compose's `init: true`). If an entrypoint hook
backgrounds a helper daemon, the server ends up as PID 1 and won't reap
zombies; `tini` handles that.

### Watched folders on network shares

Watched folders rely on `inotify`, which doesn't fire for changes made over NFS
or SMB mounts. A watched directory on a network filesystem only picks up new
files when the container restarts — bind-mount local disks instead for live
intake.

## Development

```sh
npm install
npm run tauri dev     # run the app
npm test              # frontend tests (Vitest)
npm run build         # type-check + production frontend build
cd src-tauri && cargo test
```

`docs/RECOMMENDATIONS.md` is the live backlog, including larger unstarted ideas.
`docs/archive/` holds completed history — the original
design spec (`docs/archive/SPEC.md`, largely superseded), shipped feature
specs/plans, and two rounds of code review. `CLAUDE.md` and the implementation
are the source of truth over any of it.

Releases go through `scripts/release.sh`, which bumps every manifest, rebuilds,
opens a PR, and tags the merged commit to trigger the multi-platform CI build.

## License

MIT — see [LICENSE](LICENSE).

HandBrake itself is a separate GPL-2.0 project. ConvertBar invokes `HandBrakeCLI`
as an external program and does not link against or redistribute it.
