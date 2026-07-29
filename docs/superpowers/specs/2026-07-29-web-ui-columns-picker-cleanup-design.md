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
| Keep + in-place (empty suffix) | Refuse the job before encoding, record an error naming the fix |
| Unknown `cleanup_mode` value | Falls back to `"trash"` — preserves today's behavior and mirrors `normalize_bad_source_action` |
| Whole-directory selection | One model: a checkbox per row (files and folders) plus a header "Select all" over the current listing |
| Selection persistence | Persists across navigation, with a visible count and Clear; descendants of a selected folder are dropped at confirm time |
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
  bar — Watch in `two-col`. Computed from the mode, not stored, so shrinking
  the window back restores a sensible tab without a second effect. In
  `three-col` nothing is tabbed, so `activeTab` is simply unused.
- **Drop-to-Queue is conditional.** `useFileIntake({ onDrop: () => setActiveTab("queue") })`
  becomes a no-op when Queue is pinned — switching to a tab that is not in the
  bar would blank the tabbed column.

**`TabBar`** takes the tab subset as a prop. It stays mounted in every mode:
it carries `data-tauri-drag-region`, the adding-spinner, and the desktop close
button, none of which have another home. In `three-col` it renders no tab
buttons — just the spacer and the status affordances.

**Panel headers.** Each pinned *panel* gets a header with its name — so the
third `three-col` column shows two of them, "Watch" above "Settings" — and a
four-panel view is self-describing. The update badge follows Settings: on its
tab button when Settings is tabbed, in the Settings panel header when pinned.

**CSS.** `.app-columns` is the flex row; each column is a `flex: 1` scroll
container (`overflow-y: auto`), replacing `.page` as the scroll owner in the
multi-column modes. `.page` is retained unchanged for `tabs`.

### Behavior delta

- Desktop: none. `tauri.conf.json:16-20` fixes the window at 400×500 with
  `resizable: false`, so the hook always resolves to `tabs`.
- Web ≥900px: Queue is always visible; the tab bar loses the Queue button.
- Web ≥1300px: no tab bar buttons at all; all four panels live.
- **Mount cost:** `three-col` mounts all four pages simultaneously, so
  `useQueue`, `useHistory`, `useWatchedDirectories`, and `useSettings` all fire
  their initial fetch on load. That is four HTTP requests, not four SSE
  connections — `lib/events.ts` shares one stream. Accepted.

### Testing

- `useLayoutMode` returns `tabs` / `two-col` / `three-col` for representative
  widths, updates on a `matchMedia` `change` event, and falls back to `tabs`
  when `matchMedia` is undefined.
- `App` in `two-col` renders Queue and exactly one tabbed panel; the tab bar
  has no Queue button.
- `App` in `three-col` renders all four panels and no tab buttons.
- Active-tab fallback: with `activeTab === "queue"`, entering `two-col` renders
  History (not a blank column) in the tabbed slot.
- The update badge appears in the Settings *column header* in `three-col` and
  on the Settings *tab button* in `tabs`.

---

## Part 2 — File-picker overhaul

Scope is `FileBrowserModal.tsx`, `mode: "files"` only. `mode: "directory"`
(the watched-folders picker) keeps its current single-directory behavior. The
component is server-head-only — it imports `httpCommands` directly — so none of
this reaches the desktop native dialog.

### Current behavior

- `handleEntryClick` (`FileBrowserModal.tsx:92-98`): a directory navigates, a
  file toggles. Directories are never selectable.
- `load()` calls `setSelected(new Set())` (`:52`), so navigating anywhere
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

**Confirm-time dedup.** Any selected path that lives under another selected
path is dropped before `onSelect`, so selecting a folder and a file inside it
does not queue the file twice.

**Inert backdrop.** `.modal-overlay` loses its `onClick`; the inner
`stopPropagation` on `.file-browser-modal` becomes unnecessary and is removed
with it. × and Cancel remain. No Esc handler is added: `App.tsx:41-52` binds
Esc to `hideWindow()` on desktop only, and adding a browser Esc-closes-modal
binding would re-introduce the accidental-close class this change removes.

**New module — `src/lib/pathSelection.ts`.** The two pure functions live
outside the component so they are testable without rendering:

- `rangeBetween(entries, anchorPath, targetPath): string[]` — the inclusive
  slice of `entries` between two paths, in listing order; empty when either is
  absent.
- `dropDescendants(paths): string[]` — removes any path that is a descendant of
  another path in the set. Uses component-boundary comparison (`a === b` or
  `b.startsWith(a + "/")`), so `/media` never swallows `/media2` — the same
  trap `fs.rs`'s `path_allowed` documents.

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
- `dropDescendants`: file under a selected folder is dropped; two sibling
  folders both survive; `/media` does not swallow `/media2`; the selected
  folder itself is kept.
- Select-all ticks every row and reflects tri-state after one is unticked.
- Shift-click selects a mixed file/folder range and does not deselect anything
  already selected outside it.
- Selection survives navigating into a subfolder and back; the footer count
  reflects the total across directories.
- Jump-to-path loads a valid path; a 403 renders the server's error and leaves
  the current listing intact.
- A click on `.modal-overlay` does **not** call `onClose`.
- Confirming with a folder and a file inside it passes only the folder to
  `onSelect`.

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
removes nothing, in either `KeptFile` direction. `decide_cleanup` is untouched,
so a larger output still records `space_saved` as the negative delta and status
`"skipped"` — the user simply keeps both files.

**In-place refusal, before the encode.** `in_place` is computed at
`converter.rs:955`; `cleanup_mode` is already in scope from `:808`. The guard
goes immediately after `:955` — before the stale-temp cleanup at `:961-964`
and before the HandBrake spawn at `:967`, so a job that will be refused never
burns an encode. It follows the shape of the existing spawn-failure arm
(`:979-993`): `record_job_error` with `FailureClass::Environment`, clear
`current_job_id`, `continue`.

The message names the fix rather than describing the fault:

> In-place re-encode (empty output suffix) cannot keep the original — choose
> Delete in Settings, or set an output suffix for this preset.

`in_place_action` (`:116-127`) is therefore never reached with `"keep"`; it
gains a `debug_assert!` documenting that, rather than a silent fallthrough into
`TrashSourceThenRename`.

**Server boot.** `FORCED_DELETE_KEYS` needs no code change — it only rewrites
rows equal to `'trash'`, so `'keep'` survives. That gets a regression test,
because it is precisely the kind of invariant that breaks silently.

**`bad_source_action` is unchanged.** Nothing is removed there until the user
presses purge, so "no delete" is already its default; the server keeps forcing
`delete` for the action it takes when purge *is* pressed.

**Settings UI** (`SettingsPage.tsx:231-260`). The "After conversion" group
becomes three radios on desktop (Trash / Delete / Keep) and two on the server
(Delete / Keep), replacing the server's current static "originals are deleted
permanently" hint. The existing empty-suffix note (`:221-226`) already renders
when the active preset's resolved suffix is blank; it extends with the
keep-specific warning when `cleanup_mode === "keep"`, so the impossible
combination is visible at the moment it is configured rather than at the moment
a job fails.

### Behavior delta

- A new terminal state exists: both files on disk, job status `done` or
  `skipped`, nothing disposed.
- A new refusal exists: in-place + keep errors out without encoding.
- Existing `trash` and `delete` rows behave identically to today.
- The desktop UI gains one radio; the server UI gains a real choice where it
  previously had a sentence.

### Testing

Rust, `convertbar-core`:

- `normalize_cleanup_mode` table test: `keep`, `delete`, `trash`, `""`,
  `"KEEP"`, `"nonsense"`.
- Keep, converted smaller: both files still exist after the job, the
  `RecordingDisposer` recorded nothing, status `done`.
- Keep, converted larger: both files still exist, status `skipped`,
  `space_saved` is the negative delta (pins that `decide_cleanup` was not
  touched).
- In-place + keep: the job is recorded as an error, the source is byte-identical
  afterwards, no temp is left, and no HandBrake resolution is attempted — using
  `PanickingLocator` so a regression that reaches the spawn fails loud.
- `trash` and `delete` regression tests unchanged and still green.

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

- **README**: the new "keep" cleanup option, its in-place restriction, and the
  picker's selection model (checkboxes, select-all, shift-range, jump-to-path).
- **CLAUDE.md**: a short section on the three-mode cleanup contract and the
  in-place refusal invariant — it belongs with the other destructive-path
  invariants already documented there (emit-under-db-lock, HandBrake locator
  fixtures).

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
   larger).
6. Selecting "Keep" with an empty output suffix produces a job error naming the
   fix, with the source untouched and no encode attempted.
7. A server restart does not rewrite a stored `keep` to `delete`.
8. The web UI never invites a drag-and-drop that cannot work.
9. `cargo test --workspace` and `npm test` are green; `npm run build` passes.
