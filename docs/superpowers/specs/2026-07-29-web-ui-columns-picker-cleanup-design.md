# Web UI Columns, File-Picker Overhaul, and a Third Cleanup Mode — Design

## Problem

The web UI shipped in 2.0.0 (PR #130) is the desktop menu-bar UI rendered in a
browser tab. That was the right call for a first release — one React app, one
transport seam — but it inherits three assumptions that only hold for a 400×500
popover pinned to a menu bar:

1. **One panel at a time.** The tab bar is the only navigation, so a 27" monitor
   shows the same single 400px column a menu-bar popover does.
2. **Files arrive by dragging them onto the window.** There is no OS drag-drop
   event in a browser tab, so the drop zone is decoration; the real intake is a
   secondary "Add files…" button below it.
3. **The originals go to the Trash.** A headless server has no Trash, so the
   server head force-rewrites `cleanup_mode` to `delete` at every boot — which
   silently converts "I picked the safe option" into permanent deletion.

The file browser built to replace the native dialog is also minimal by design:
files only (folders navigate, never select), one directory's worth per
confirm, no way to reach a known path except by clicking down to it, and an
overlay that discards the whole selection on a stray backdrop click.

## Goal

Make the Docker web UI good on its own terms — a real multi-column layout on
large displays, a file picker that can express "these three folders and that
file", and a cleanup policy that lets the user keep their source files. Desktop
behavior changes in exactly one way: it gains the same new cleanup option.

## Decisions (settled with the user)

| Topic | Decision |
|---|---|
| Wide layout | Three breakpoints: tabs (<900px), Queue pinned + tabbed second column (≥900px), Queue \| History \| Watch+Settings (≥1300px) |
| Layout gating | Width-driven, not head-gated. The desktop window is fixed 400×500 and non-resizable, so it always resolves to `tabs` |
| "Keep original" scope | Both heads. Core learns `cleanup_mode = "keep"`; desktop offers Trash/Delete/Keep, server offers Delete/Keep |
| Keep + larger output | Nothing is ever removed, either direction. `decide_cleanup` still records `"skipped"` |
| What Keep is for | An evaluation mode — verify the encodes, delete originals by hand, then switch to Trash/Delete. `space_saved` keeps its normal value: it is the optimization delta, not a claim about freed bytes |
| Keep + in-place (empty suffix) | Prevented at add time and at setting-change time, so the job never exists; a non-destructive converter arm covers the race |
| Unknown `cleanup_mode` value | Falls back to `"trash"` — preserves today's behavior and mirrors `normalize_bad_source_action` |
| Whole-directory selection | One model: a checkbox per row (files and folders) plus a header "Select all" over the current listing |
| Selection persistence | Persists across navigation, with a visible count and Clear |
| Backdrop click | Inert. Only ×, Cancel close the modal. No confirm dialog, and no new Esc handler |
| Drag-and-drop removal | Intake only. The drop surface becomes a click-to-pick target on the server head; queue-item drag-reordering is untouched |

## Rejected alternatives

- **All four panels in a 2×2 grid, no tabs.** Settings is by far the longest
  panel and would dominate a cell at every width. The chosen layout keeps
  Settings paired with Watch in one column, where their combined height is
  comparable to Queue and History.
- **A separate `delete_original` boolean for the server.** Two settings
  governing one behavior, with a cross-product of states that `converter.rs`
  would have to reconcile. `cleanup_mode` is already the single knob; it grows
  a third value.
- **Keep protects only the source, and a losing output is deleted.** Tidier on
  disk, but it makes "keep" perform a permanent delete the user never chose.
- **"Keep" replaces in place anyway when the suffix is empty.** Jobs always
  complete, but a setting named "keep the original" would overwrite the
  original — the worst possible surprise on a destructive path.
- **Zeroing `space_saved` under keep.** Rejected by the user, correctly: the
  field is the optimization delta, not a bytes-freed claim, and it is the
  number Keep exists to let you evaluate before deleting anything.
- **Refusing an in-place job inside `process_queue` with an error row.** The
  first draft of this spec; it creates an unbounded watched-folder loop. See
  Part 3 for the full trace — it is the single biggest change the adversarial
  review forced.
- **Dropping selected paths that live under other selected paths.** Also a
  first-draft idea, and also removed: the backend already dedupes, and doing
  it in the picker silently loses an explicitly ticked file when the user
  skips its parent folder's confirm prompt.
- **Always confirm on backdrop click** (the literal original request). Making
  the backdrop inert removes the accident class outright, with no dialog to
  build and no nag when nothing is selected.

---

## Part 1 — Multi-column layout

### Current behavior

`App.tsx:65-70` mounts exactly one page, selected by `activeTab`. `TabBar`
(`TabBar.tsx:13-18`) renders all four tabs plus the adding-spinner, the
update badge, and (desktop only) the close button. `.app` is
`height: 100vh; width: 100vw` with `body { overflow: hidden }`; `.page` is the
single scroll container (`App.css:38-45, 90-94`).

### Design

A pure-CSS solution is not available: which panels *exist* has to change, and
CSS cannot mount an unmounted component. So layout resolution is a hook.

**`src/hooks/useLayoutMode.ts`** — wraps `window.matchMedia`, subscribes to
`change`, and returns:

| Mode | Trigger | Pinned columns | Tab bar renders |
|---|---|---|---|
| `tabs` | `<900px` | — | Queue, History, Watch, Settings |
| `two-col` | `≥900px` | Queue | History, Watch, Settings |
| `three-col` | `≥1300px` | Queue, History, Watch+Settings | no tab buttons |

`matchMedia` is absent in some jsdom configurations; the hook falls back to
`tabs` when it is undefined rather than throwing.

**`App.tsx`** renders the pinned columns unconditionally and the tabbed column
from `activeTab`, in a `.app-columns` flex/grid container. Two derived rules:

- **Active-tab fallback.** When the current `activeTab` becomes pinned (Queue
  at ≥900px, History at ≥1300px), it falls back to the first tab still in the
  bar — History in `two-col`. **Derived from the mode, not stored**, so any
  attempt to select a pinned tab resolves to a visible one instead of blanking
  the tabbed column, and shrinking the window back restores a sensible tab
  without a second effect. In `three-col` nothing is tabbed, so `activeTab` is
  simply unused.

  This subsumes the drop-to-Queue case: `useFileIntake({ onDrop: () => setActiveTab("queue") })`
  needs no guard, because the derived fallback already handles a pinned target.
  It could not fire anyway — `onDrop` comes from the desktop drag listener
  (`useFileIntake.ts:131-139`), and desktop is always `tabs`.

**`TabBar`** takes the tab subset as a prop. It stays mounted in every mode:
it carries `data-tauri-drag-region`, the adding-spinner, and the desktop close
button, none of which have another home. In `three-col` it renders no tab
buttons — just the spacer and the status affordances.

**Panel headers.** Each pinned *panel* gets a header with its name — so the
third `three-col` column shows two of them, "Watch" above "Settings" — and a
four-panel view is self-describing.

The update badge stays on the Settings **tab button only**, and does not follow
Settings into a pinned header. It cannot fire there: `useUpdate` returns early
on the server head (`useUpdate.ts:40-44`), leaving `state` null forever, and the
server head is the only one that ever reaches a multi-column mode. A badge in a
pinned header would be code for an impossible head × width combination.

**CSS.** `.app-columns` is the flex row; each column is a `flex: 1` scroll
container (`overflow-y: auto`), replacing `.page` as the scroll owner in the
multi-column modes. `.page` is retained unchanged for `tabs`.

### Behavior delta

- Desktop: none. `tauri.conf.json:16-20` fixes the window at 400×500 with
  `resizable: false`, so the hook always resolves to `tabs`.
- Web ≥900px: Queue is always visible; the tab bar loses the Queue button.
- Web ≥1300px: no tab bar buttons at all; all four panels live.
- **Mount cost:** `three-col` mounts all four pages simultaneously, so every
  page hook fires its initial fetch on load. It is not four requests:
  `HistoryPage` also calls `useSettings` (`HistoryPage.tsx:72`), so two
  `useSettings` instances mount, each issuing `getSettings`,
  `listHandbrakePresets`, `getPresetSuffix` and `generatePresetSuffix` — and
  the last two shell out to HandBrakeCLI on the server. Two duplicate
  HandBrake spawns per page load is ugly but harmless, and the two instances
  drive no conflicting UI (the only setting `HistoryPage` reads is
  `bad_source_action`, which the server fixes at boot). Accepted; lifting
  `useSettings` to `App` and passing it down is listed as a follow-up.
- No SSE cost: `lib/events.ts` holds one module-level `EventSource`, and all
  six hooks are fetch-plus-listen with per-instance cleanup — none assumes it
  is the only listener.

### Testing

- `useLayoutMode` returns `tabs` / `two-col` / `three-col` for representative
  widths, updates on a `matchMedia` `change` event, and falls back to `tabs`
  when `matchMedia` is undefined.
- `App` in `two-col` renders Queue and exactly one tabbed panel; the tab bar
  has no Queue button.
- `App` in `three-col` renders all four panels and no tab buttons.
- Active-tab fallback: with `activeTab === "queue"`, entering `two-col` renders
  History (not a blank column) in the tabbed slot.
- Each pinned panel renders its own header; the `three-col` third column
  renders both "Watch" and "Settings" headers.

---

## Part 2 — File-picker overhaul

Scope is `FileBrowserModal.tsx`, `mode: "files"` only. `mode: "directory"`
(the watched-folders picker) keeps its current single-directory behavior. The
component is server-head-only — it imports `httpCommands` directly — so none of
this reaches the desktop native dialog.

### Current behavior

- `handleEntryClick` (`FileBrowserModal.tsx:92-98`): a directory navigates, a
  file toggles. Directories are never selectable.
- `load()` calls `setSelected(new Set())` (`:53`), so navigating anywhere
  discards the selection.
- The only way to reach a path is to click down to it from a configured root.
- `.modal-overlay` has `onClick={onClose}` (`:117`), so a backdrop click
  discards everything with no confirmation.

### Design

**Row model.** Every row carries a checkbox and toggles selection when clicked
— files *and* folders. Folder rows gain a trailing `→` navigate button that
stops propagation, with an `aria-label` of `Open <name>` and a full-height hit
area.

This is a deliberate change to a habit: clicking a folder no longer enters it.
It is what makes a shift-range across mixed file/folder rows uniform, which is
a stated requirement — a model where the name navigates and only the checkbox
selects would force precise checkbox clicks for every range. The breadcrumb
already covers upward navigation.

**Select all.** A header row above the listing with a tri-state checkbox
(none / some / all selected in the *current* listing) and a count. This is
what "select the whole current directory" resolves to: ticking every entry,
each selected folder then expanding recursively through the existing scan
pipeline.

**Jump to path.** A text input under the breadcrumb; Enter or a "Go" button
calls the existing `load()`. No new route and no server change —
`GET /api/fs/list` canonicalizes and 403s anything outside `browse_roots`
(`fs.rs:102`), and 404s a nonexistent path. Both render in the existing error
slot. A path that is a *file* rather than a directory returns a 500 from
`read_dir`; the server's error string is shown as-is rather than special-cased.

**Persistent selection.** `load()` no longer clears `selected`. The footer
shows `N selected` with a Clear button, and the confirm label keeps its
existing count. The trade-off — a selection made three folders ago is
off-screen — is mitigated by the always-visible count.

**Shift-range.** A plain toggle sets the anchor. `shift`+click selects every
row between the anchor and the clicked row in current listing order,
inclusive, across mixed types. It is **additive**: it never deselects, so a
mis-aimed shift-click cannot silently drop earlier work. The anchor resets on
navigation — ranges are per-listing.

**No confirm-time dedup.** An earlier draft dropped any selected path living
under another selected path, to stop a folder-plus-a-file-inside-it from
queueing twice. That is both unnecessary and harmful:

- Unnecessary, because the backend already dedupes. `useFileIntake` enqueues
  `classified.files` *before* `classified.folders` (`useFileIntake.ts:107-117`)
  through a serialized pipeline, and `add_files_to_db` skips any source already
  in `queued_paths` (`queue_ops.rs:932`, `fetch_skip_sets` at `:691-741`). The
  explicit file lands first; the folder scan then skips it.
- Harmful, because a folder over `AUTO_ADD_MAX` files goes to the confirm
  queue, and **Skip** (`useFileIntake.ts:157-161`) discards it — taking the
  individually-ticked file with it, silently, if dedup had already removed it
  from the direct-file batch.

So the selection is passed through as-is. `dropDescendants` is not built.

**Inert backdrop.** `.modal-overlay` loses its `onClick`; the inner
`stopPropagation` on `.file-browser-modal` becomes unnecessary and is removed
with it. × and Cancel remain. No Esc handler is added: `App.tsx:41-52` binds
Esc to `hideWindow()` on desktop only, and adding a browser Esc-closes-modal
binding would re-introduce the accidental-close class this change removes.

**New module — `src/lib/pathSelection.ts`.** One pure function, outside the
component so it is testable without rendering:

- `rangeBetween(entries, anchorPath, targetPath): string[]` — the inclusive
  slice of `entries` between two paths, in listing order; empty when either is
  absent. Order-agnostic (anchor may follow target) and separator-agnostic, so
  it holds if a server head ever runs on Windows.

### Behavior delta

- Selecting a folder is new; those paths flow into `classifyPaths`, which
  already returns `folders` and routes them through the recursive scan with
  the >5-file confirm prompt (`useFileIntake.ts:110-117`). No backend work.
- A backdrop click no longer closes the modal.
- Clicking a folder row selects instead of navigating.

### Testing

`pathSelection` unit tests first (pure, exhaustive), then component tests:

- `rangeBetween`: forward range, reverse range (anchor after target), single
  row, anchor absent from the listing.
- A folder and a file inside it both reach `onSelect` — pinning that the
  component does not dedupe, and that the "Skip the folder, lose the file"
  hole stays closed.
- Select-all ticks every row and reflects tri-state after one is unticked.
- Shift-click selects a mixed file/folder range and does not deselect anything
  already selected outside it.
- Selection survives navigating into a subfolder and back; the footer count
  reflects the total across directories.
- Jump-to-path loads a valid path; a 403 renders the server's error and leaves
  the current listing intact.
- A click on `.modal-overlay` does **not** call `onClose`.
- `mode: "directory"` is unchanged: no checkboxes, no select-all, folders still
  navigate on click, "Choose this folder" still returns the current path.

---

## Part 3 — `cleanup_mode = "keep"`

The only core change, and the only one on a destructive path.

### Current behavior

`cleanup_mode` is `trash | delete`. After every conversion `decide_cleanup`
(`converter.rs:283-303`) picks a winner by size and the loser is **always**
removed (`converter.rs:1185-1203`): `delete` unlinks it, anything else routes
through `ctx.disposer` (OS Trash on desktop, `DeleteDisposer` on the server).
There is no way to keep both files.

On the server, `startup.rs:15`'s `FORCED_DELETE_KEYS` rewrites any row still at
`'trash'` to `'delete'` at every boot, because the `trash` crate litters
`.Trash-<uid>` directories on NAS mounts.

### Design

**Normalizer.** `normalize_cleanup_mode(&str) -> &'static str` in
`settings_ops.rs`, mirroring `normalize_bad_source_action` (`:62-68`): exact
`"keep"` and `"delete"` pass through, everything else resolves to `"trash"`.
`converter.rs`'s `get_cleanup_mode` reads through it, so no call site does a
bare string compare against an unvalidated column.

The fallback is `trash`, not `keep`, for two reasons: it preserves today's
behavior for every existing row byte-for-byte, and it keeps the two normalizers
symmetric. The cost is named explicitly: on the server `trash` resolves to
`DeleteDisposer`, so a corrupted row there still permanently deletes — exactly
as it does today. (A `keep` fallback would be strictly non-destructive but
would let a corrupt row silently change conversion outcomes.)

**Distinct-file path** (`converter.rs:1185-1203`). A third arm: `"keep"`
removes nothing, in either `KeptFile` direction. `decide_cleanup` stays pure
and untouched, so status is still `done`/`skipped` on the same size rule.

**`space_saved` keeps its normal value under keep.** An earlier draft zeroed it
on the grounds that nothing was freed. That misreads the field: it records how
much the encode *optimized*, whether or not the original has been removed yet.
`get_history_summary` (`queue_ops.rs:1388-1397`), the "Saved" badge, and the
completion notification (`converter.rs:1297-1301`) all continue to work
unchanged — which also means zero new code on this path.

This follows from what Keep is **for**. It is an evaluation mode, not a steady
state: run a batch, verify the encodes are actually good on this hardware,
delete the originals by hand, then switch to Trash or Delete for normal
operation. Under that lifecycle the delta is exactly the number the user is
evaluating — "was this worth it?" — and zeroing it would blank the only
column that answers the question. The Settings copy says so, so Keep does not
read as a permanent "never clean up" setting.

**In-place under keep is prevented, never refused.** The obvious design — let
the job run and error out in `process_queue` — is wrong, and the reason is
worth recording because it is not obvious.

A per-job `error` row is invisible to both re-ingestion guards.
`fetch_skip_sets` (`queue_ops.rs:691-741`) suppresses only `queued`/`encoding`/
`paused` rows and `done`/`skipped` fingerprints; `filter_known_bad_sources`
(`watcher.rs:391-397`) filters only `bad_source[_truncated]`. So a watched
folder of N files, under keep with an empty-suffix preset, would re-queue and
re-refuse all N on every `scan_all_enabled` — a new error row and a "failed"
notification each, forever, with `enqueue_and_start` (`watcher.rs:470-473`)
clearing the user's queue pause each time. That is precisely the failure
`watcher.rs:382-390` documents as the reason `filter_known_bad_sources` exists.
Unlike a transient Environment fault, keep + empty suffix is a *stable
configuration* the user can sit in indefinitely.

So the impossible job is never created. Two enforcement points, both at the
moment the user acts, both reportable:

1. **At add time.** `add_files_to_db` (`queue_ops.rs:925-940`) already computes
   the output path and carries skip counters (`n_not_video`, `n_queued`,
   `n_converted`, `n_output_exists`). It gains one more: a source whose output
   path equals itself is not queued while `cleanup_mode == "keep"`, and is
   counted as `n_inplace_keep_blocked`. No row is created, so no watcher loop
   is possible — a rescan re-skips it as cheaply as any other skip. `AddResult`
   and `summarizeAdds` (`src/lib/addSummary.ts`) surface it in the intake
   summary, at exactly the moment the user can fix it.
2. **At setting-change time.** Writing `cleanup_mode = "keep"` drops any
   already-`queued` in-place job in the same `update_setting` post-write hook
   that `watch_skip_marker` uses (`settings_ops.rs:161-181`), then emits
   `queue-updated` so the Queue panel refreshes. **The db guard must be dropped
   before that emit** — `update_setting` already scopes it for this exact
   reason, and violating it is the shipped-twice deadlock CLAUDE.md documents.

**The converter keeps one non-destructive backstop.** A job can flip to
`encoding` in the window between the setting write and the dequeue. For that
race, `in_place_action` (`:116-127`) gains a real arm — `"keep" => RemoveTemp`
— not the `debug_assert!` I first specified. `debug_assert!` compiles out in
release, and the `else` branch it would have guarded is `TrashSourceThenRename`,
which on the server routes through `DeleteDisposer` and permanently removes the
user's source. One line buys a release-mode guarantee that the worst case is a
wasted encode rather than a destroyed original.

No new event, no new Ctx state, no new command, no new banner — and no error
rows for the watcher to trip over.

**Server boot.** `FORCED_DELETE_KEYS` needs no code change — it only rewrites
rows equal to `'trash'`, so `'keep'` survives. That gets a regression test,
because it is precisely the kind of invariant that breaks silently.

**`bad_source_action` is unchanged.** Nothing is removed there until the user
presses purge, so "no delete" is already its default; the server keeps forcing
`delete` for the action it takes when purge *is* pressed.

**Settings UI** (`SettingsPage.tsx:231-260`). The "After conversion" group
becomes three radios on desktop (Trash / Delete / Keep) and two on the server
(Delete / Keep), replacing the server's current static "originals are deleted
permanently" hint.

Keep's label carries its lifecycle, so it does not read as "never clean up":

> **Keep both files** — nothing is deleted. Use this to check the encodes are
> good on this machine, remove the originals yourself, then switch to
> Delete once you trust the results.

The existing empty-suffix note (`:221-226`) already renders
when the active preset's resolved suffix is blank; it extends with the
keep-specific warning when `cleanup_mode === "keep"`, so the impossible
combination is visible at the moment it is configured rather than at the moment
a job fails.

### Accepted consequence: kept sources rely on history fingerprints

Under `trash`/`delete` the source is gone after a successful conversion, so
re-ingestion is structurally impossible. Under `keep` the source survives in
the watched folder, and the *only* thing preventing a re-convert is its
`(size, mtime)` fingerprint recorded in the `done`/`skipped` row
(`queue_ops.rs:709-741`, `cheap_skip_reason` at `:627-634`). That is a real
behavioral shift, and it has two edges we accept rather than fix:

- **Clearing history re-converts kept sources.** `clear_history`
  (`queue_ops.rs:1181-1187`) deletes the rows that carry the fingerprints; the
  next scan re-adds the still-present source, and `choose_output_path`
  (`:655-682`) finds the old output name taken and renumbers —
  `movie (1).1080p-h265.mp4`. Clearing history is the user saying "forget
  this", so re-converting is arguably correct; the disk cost is not obvious,
  so it goes in the README.
- **A NULL fingerprint falls back to the legacy bucket.** If `file_identity`
  fails at add time (`queue_ops.rs:942`), the completed row is honored only
  when `in_place || skip_already_converted` (`:729-739`). With the default
  `skip_already_converted = false`, a kept source in that state is re-added and
  re-converted on every scan, renumbering each time. The precondition is rare
  (an unreadable mtime), the consequence is disk-filling, and the fix belongs
  to the fingerprint layer rather than to this change. Documented, not built.

The mainline case — fingerprinted row, source still present — does **not** loop,
and gets a test that says so.

### Behavior delta

- A new terminal state exists: both files on disk, job status `done` or
  `skipped`, `space_saved` as usual, nothing disposed.
- In-place sources are no longer queued while keep is active, and switching to
  keep drops already-queued in-place jobs.
- Existing `trash` and `delete` rows behave identically to today.
- The desktop UI gains one radio; the server UI gains a real choice where it
  previously had a sentence.

### Testing

Rust, `convertbar-core`:

- `normalize_cleanup_mode` table test: `keep`, `delete`, `trash`, `""`,
  `"KEEP"`, `"nonsense"`.
- Keep, converted smaller: both files still exist after the job, the
  `RecordingDisposer` recorded nothing, status `done`, `space_saved` is the
  positive delta — identical to what `delete` records for the same sizes.
- Keep, converted larger: both files still exist, status `skipped`,
  `space_saved` is the negative delta. Together these pin that keep changed
  *only* the disposal, and that `decide_cleanup` stayed pure.
- Add time: an in-place source under keep produces no job row and increments
  the new skip counter; the same source under `delete` still queues.
- Setting change: with an in-place job `queued`, writing `cleanup_mode=keep`
  removes it, emits `queue-updated`, and leaves `ctx.db` unlocked afterwards
  (the `try_lock` assertion `update_setting`'s existing deadlock test uses).
- `in_place_action(KeptFile::Converted, "keep") == RemoveTemp` — a table row on
  the release-mode backstop, so the safe arm cannot be dropped as "unreachable".
- Re-ingestion under keep: a converted-and-kept source with a fingerprinted
  `done` row is **not** re-added. This is the loop the whole design turns on;
  nothing else pins it. Asserted at `add_files_to_db`, which is the chokepoint
  every watcher rescan funnels through (`watcher.rs:438` → `add_files_inner` →
  `add_files_to_db`) — a `scan_all_enabled`-level test would add filesystem and
  timing setup without covering a single additional line of the skip rule.
- `trash` and `delete` regression tests unchanged and still green.

Fixture note: `get_handbrake_path` runs at `converter.rs:807`, *before* any
per-job gate, so no converter test can assert "HandBrake was never resolved".
These tests follow the pattern the existing converter tests already use — pin
`handbrake_path` to a fake script, which makes `resolve_with_locator`
short-circuit before consulting the locator at all (`handbrake.rs:136-146`).
The fixture default `PanickingLocator` is therefore correct and `StubLocator`
is unnecessary; declaring the world here means declaring the *path*.

Rust, `convertbar-server`:

- `normalize_server_settings` leaves a `'keep'` row untouched (alongside the
  existing trash→delete and delete-untouched cases).

Frontend:

- `SettingsPage` renders three cleanup radios on desktop and two (no Trash) on
  the server head.
- Selecting Keep writes `cleanup_mode=keep` through `updateSetting`.
- The in-place warning renders only when the resolved suffix is empty **and**
  the mode is `keep`.

---

## Part 4 — Web intake without drag-and-drop

### Current behavior

The OS drag-drop listener is already server-gated (`useFileIntake.ts:131`), so
nothing is broken — the UI just lies. `DropZone` renders "Drop video files or
folders here" (`DropZone.tsx:40`), the real intake is a separate button below
it (`QueuePage.tsx:96-102`), and the empty state says "Drag video files or
folders here to get started" (`QueuePage.tsx:172-177`).

### Design

`DropZone` takes an optional `onPick`. When present the label branch becomes a
clickable "Add files or folders…" surface (a `<button>` filling the zone, with
the dashed border retained as an affordance); the pending-confirm and status
branches are unchanged, since they are the same in both heads. `QueuePage`
passes `onPick` only under `isServerHead` and drops the now-redundant
`.intake-actions` block. The empty-state copy branches on head.

Queue-item drag-reordering (`QueuePage.tsx:63-73`, `QueueItem`) is untouched:
HTML5 drag works in a browser and there is no replacement worth building.

### Testing

- `DropZone` with `onPick` renders a button and calls it on click; without
  `onPick` it renders the drop label and no button.
- `DropZone` with a `pendingConfirm` renders the confirm prompt in both cases —
  `onPick` must not shadow it.
- `QueuePage` on the server head renders no separate `.intake-actions` button.

---

## Part 5 — Documentation

- **README**: the new "keep" cleanup option *and its intended lifecycle*
  (evaluate → delete originals by hand → switch to Delete), its in-place
  restriction, the
  picker's selection model (checkboxes, select-all, shift-range, jump-to-path),
  and the "clearing history re-converts kept sources" caveat from Part 3.
- **`unraid-template.xml:27-31`**: currently states "This server build always
  deletes replaced files rather than moving them to a trash folder." Part 3
  makes that false — it is the container's own destructive-behavior warning, so
  it must say that deletion is now the *default*, not the only option.
- **CLAUDE.md**: a short section on the three-mode cleanup contract, the
  two-point in-place prevention, and the non-destructive `in_place_action` arm —
  it belongs with the other destructive-path invariants already documented there
  (emit-under-db-lock, HandBrake locator fixtures).

---

## Sequencing

Five parts, independently shippable, ordered so the riskiest lands with the
most attention available and nothing depends on unmerged work:

1. **Part 3 (cleanup mode)** — core, destructive path, its own PR and review.
2. **Part 2 (file picker)** — largest frontend surface; `pathSelection.ts` first
   (TDD, pure), then the component.
3. **Part 1 (layout)** — restructures `App`/`TabBar` and how `QueuePage` is
   mounted; landing it first keeps Part 4's edits to `QueuePage` from
   conflicting with the column restructure.
4. **Part 4 (intake)** — small, and reads more clearly once the layout is in.
5. **Part 5 (docs)** — folded into each PR rather than deferred.

## Out of scope

- Recent/favourite paths in the picker, or path autocompletion.
- A file-type filter in the picker (it lists everything; `classifyPaths`
  already discards non-video files downstream).
- Making the desktop window resizable to reach the multi-column layouts.
- Replacing queue-item drag-reordering with buttons.
- Any change to `bad_source_action` or to the purge flow.
- Trash support on the server.
- Lifting `useSettings` out of `HistoryPage` into `App` to collapse the
  duplicate mount in `three-col` (see Part 1's mount cost). Worth doing, not
  worth entangling with this change.
- Re-ingestion protection for kept sources with a NULL identity fingerprint
  (Part 3's second accepted consequence) — that belongs to the fingerprint
  layer.

## Acceptance criteria

1. At ≥1300px the web UI shows Queue, History, and Watch+Settings side by side
   with no tab buttons; at ≥900px Queue plus one tabbed column; below that the
   current tab bar. The desktop app is visually unchanged.
2. The picker can select files and folders together, across more than one
   directory, via individual clicks, a select-all, and a shift-range over mixed
   types; the count is always visible.
3. Typing a path into the picker navigates there, and a path outside
   `browse_roots` shows the server's error without disturbing the listing.
4. Clicking the picker's backdrop does nothing.
5. Selecting "Keep" and converting leaves both the source and the output on
   disk, with the job recorded as `done` (or `skipped` when the output is
   larger) and the same `space_saved` any other mode would record — so History
   answers "was this encode worth it?" before anything is deleted.
6. Under "Keep", an empty-suffix source is never queued: adding it reports a
   skip, switching to Keep drops it from the queue, and no error row is ever
   written — so a watched folder cannot re-queue it on the next scan.
7. A watched folder rescan does not re-convert a source that was kept by a
   previous successful conversion.
8. A server restart does not rewrite a stored `keep` to `delete`.
9. The web UI never invites a drag-and-drop that cannot work.
10. `cargo test --workspace` and `npm test` are green; `npm run build` passes.
