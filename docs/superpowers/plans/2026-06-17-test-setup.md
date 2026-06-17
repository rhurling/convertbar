# ConvertBar Test Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a test harness for both the Rust backend and the React/TypeScript frontend, then progressively cover the logic that matters — starting with zero-friction pure functions and ending with the destructive cleanup-decision and process-orchestration paths.

**Architecture:** Rust uses built-in `cargo test` with in-module `#[cfg(test)]` blocks (so private fns like `parse_progress` are reachable) and `Connection::open_in_memory()` for DB-backed tests. Frontend uses Vitest (shares the Vite pipeline) with jsdom for the later hook/component phases. CI gains a `push`/`pull_request` job so tests actually gate — today CI only fires on version tags.

**Tech Stack:** Rust + `cargo test` + `rusqlite` (in-memory) + `tempfile` (dev-dep, Phase 2). Frontend: Vitest + jsdom + `@testing-library/react` (Phase 3). GitHub Actions.

**Note on test style:** Phases 1–2 are mostly *characterization tests* of existing, working code — the test should PASS on first run. The "run the test" step confirms behavior matches intent; a failure means we found a real bug (surface it, don't paper over it). This inverts the usual TDD RED→GREEN, which is expected here.

---

## File Structure

| File | Responsibility | Phase |
|------|----------------|-------|
| `vite.config.ts` (modify) | Add Vitest `test` config (jsdom env, setup file) | 0 |
| `package.json` (modify) | Add `test` / `test:watch` scripts + dev-deps | 0 |
| `src/test/smoke.test.ts` (create) | Frontend harness smoke test | 0 |
| `.github/workflows/test.yml` (create) | PR/push CI gate (frontend + rust jobs) | 0 |
| `src/lib/format.test.ts` (create) | Pure formatter tests | 1 |
| `src-tauri/src/handbrake.rs` (modify) | In-module tests for `slugify`, `resolve_suffix_template` | 1 |
| `src-tauri/src/converter.rs` (modify) | In-module tests for `parse_progress`, `format_bytes_short` | 1 |
| `src-tauri/src/commands/queue.rs` (modify) | In-module test for `is_video_file` | 1 |
| `src-tauri/src/converter.rs` (refactor) | Extract pure cleanup-decision fn from `process_queue` | 2 |
| `src-tauri/src/handbrake.rs` (refactor) | Split classification out of `get_preset_metadata` | 2 |
| `src-tauri/src/commands/queue.rs` (tests) | In-memory DB tests for `add_files_inner`, history, ordering | 2 |
| `src/hooks/*.test.ts`, `src/components/*.test.tsx` (create) | Mocked-Tauri hook/component tests | 3 |

---

## Phase 0 — Scaffolding

Goal: one passing test on each side + a CI gate. No production logic touched.

### Task 0.1: Branch

**Files:** none (git)

- [ ] **Step 1: Create a feature branch** (we're on `main`; never commit test scaffolding directly to it)

Run: `git checkout -b feature/test-setup`
Expected: `Switched to a new branch 'feature/test-setup'`

### Task 0.2: Frontend test harness

**Files:**
- Modify: `package.json`
- Modify: `vite.config.ts`
- Create: `src/test/smoke.test.ts`

- [ ] **Step 1: Install dev dependencies**

Run: `npm install -D vitest jsdom`
Expected: both added to `devDependencies`, lockfile updated.

- [ ] **Step 2: Add test scripts to `package.json`**

In the `"scripts"` block add:
```json
    "test": "vitest run",
    "test:watch": "vitest"
```

- [ ] **Step 3: Add Vitest config to `vite.config.ts`**

Change the import line:
```ts
import { defineConfig } from "vitest/config";
```
Add a `test` key to the returned config object (sibling of `plugins`):
```ts
  test: {
    environment: "jsdom",
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
  },
```

- [ ] **Step 4: Write the smoke test**

`src/test/smoke.test.ts`:
```ts
import { describe, it, expect } from "vitest";

describe("test harness", () => {
  it("runs", () => {
    expect(1 + 1).toBe(2);
  });
});
```

- [ ] **Step 5: Run it**

Run: `npm test`
Expected: 1 passed.

- [ ] **Step 6: Confirm the production build still type-checks**

Run: `npm run build`
Expected: `tsc` passes (it now also type-checks test files under `src/`) and Vite builds.

### Task 0.3: Rust test harness

**Files:** none new — `cargo test` is built in. Verify it runs.

- [ ] **Step 1: Confirm cargo test runs**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: compiles and reports `0 passed` (no tests yet). This proves the toolchain before Phase 1 adds real tests.

### Task 0.4: CI gate

**Files:**
- Create: `.github/workflows/test.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: test

on:
  push:
    branches: [main]
  pull_request:

jobs:
  frontend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: actions/setup-node@v5
        with:
          node-version: lts/*
      - run: npm ci
      - run: npm test

  rust:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v5
      - uses: dtolnay/rust-toolchain@stable
      - uses: swatinem/rust-cache@v2
        with:
          workspaces: "./src-tauri -> target"
      - run: cargo test --manifest-path src-tauri/Cargo.toml
```

Rationale: Rust tests run on `macos-latest` (the dev/primary platform) to avoid pulling the Linux webkit2gtk/appindicator system deps just to compile the Tauri crate. Frontend runs on ubuntu (fast/cheap).

- [ ] **Step 2: Commit Phase 0**

```bash
git add package.json package-lock.json vite.config.ts src/test/smoke.test.ts .github/workflows/test.yml
git commit -m "test: scaffold vitest + cargo test harness and CI gate"
```

---

## Phase 1 — Pure functions (no refactor needed)

Goal: cover the genuinely tricky pure logic. Each Rust test lives in an in-module `#[cfg(test)] mod tests` block so private functions are reachable.

### Task 1.1: Frontend formatters

**Files:**
- Create: `src/lib/format.test.ts`

- [ ] **Step 1: Write the tests** (encode intent, incl. the divide-by-zero and zero-byte guards)

```ts
import { describe, it, expect } from "vitest";
import { formatBytes, formatEta, formatPercent, fileName } from "./format";

describe("formatBytes", () => {
  it("returns '0 B' for zero (guards Math.log(0))", () => {
    expect(formatBytes(0)).toBe("0 B");
  });
  it("formats KB / MB / GB to one decimal", () => {
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(1048576)).toBe("1.0 MB");
    expect(formatBytes(1073741824)).toBe("1.0 GB");
  });
});

describe("formatEta", () => {
  it("uses m+s under an hour, zero-padding seconds", () => {
    expect(formatEta(150)).toBe("2m30s");
    expect(formatEta(59)).toBe("0m59s");
  });
  it("uses h+m at/over an hour, zero-padding minutes", () => {
    expect(formatEta(3661)).toBe("1h01m");
  });
});

describe("formatPercent", () => {
  it("guards division by zero", () => {
    expect(formatPercent(5, 0)).toBe("0%");
  });
  it("rounds to whole percent", () => {
    expect(formatPercent(50, 100)).toBe("50%");
  });
});

describe("fileName", () => {
  it("returns the last path segment", () => {
    expect(fileName("/Users/me/Movies/clip.mp4")).toBe("clip.mp4");
  });
  it("returns the input unchanged when there is no separator", () => {
    expect(fileName("clip.mp4")).toBe("clip.mp4");
  });
});
```

- [ ] **Step 2: Run**

Run: `npm test`
Expected: all pass. If `formatEta(3661)` or any byte case fails, that is a real bug — stop and report it.

### Task 1.2: Rust `parse_progress` + `format_bytes_short`

**Files:**
- Modify: `src-tauri/src/converter.rs` (append a test module at end of file)

- [ ] **Step 1: Add the test module**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_progress_line() {
        let line = "Encoding: task 1 of 1, 42.50 % (123.45 fps, avg 120.00 fps, ETA 00h02m30s)";
        let (percent, fps, avg_fps, eta) = parse_progress(line).unwrap();
        assert_eq!(percent, 42.5);
        assert_eq!(fps, 123.45);
        assert_eq!(avg_fps, 120.0);
        assert_eq!(eta, 150); // 2m30s
    }

    #[test]
    fn falls_back_to_percent_only() {
        let line = "Encoding: task 1 of 1, 5.00 %";
        let (percent, fps, avg_fps, eta) = parse_progress(line).unwrap();
        assert_eq!(percent, 5.0);
        assert_eq!(fps, 0.0);
        assert_eq!(avg_fps, 0.0);
        assert_eq!(eta, 0);
    }

    #[test]
    fn ignores_non_encoding_lines() {
        assert!(parse_progress("Scanning title 1 of 1").is_none());
    }

    #[test]
    fn format_bytes_short_picks_units() {
        assert_eq!(format_bytes_short(0), "0B");
        assert_eq!(format_bytes_short(1024), "1KB");
        assert_eq!(format_bytes_short(1_048_576), "1MB");
        assert_eq!(format_bytes_short(1_073_741_824), "1.0GB");
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo test --manifest-path src-tauri/Cargo.toml parse_progress format_bytes`
Expected: all pass.

### Task 1.3: Rust `slugify` + `resolve_suffix_template`

**Files:**
- Modify: `src-tauri/src/handbrake.rs` (append a test module at end of file)

- [ ] **Step 1: Add the test module**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn meta(codec: &str, resolution: &str, device: &str) -> PresetMetadata {
        PresetMetadata {
            codec: codec.into(),
            resolution: resolution.into(),
            quality: "hq".into(),
            preset: "preset".into(),
            device: device.into(),
        }
    }

    #[test]
    fn slugify_collapses_separators_and_trims() {
        assert_eq!(slugify("H.265 Apple VideoToolbox 1080p"), "h-265-apple-videotoolbox-1080p");
        assert_eq!(slugify("  Fast 1080p30  "), "fast-1080p30");
    }

    #[test]
    fn resolves_full_template() {
        let m = meta("h265", "1080p", "apple-videotoolbox");
        assert_eq!(resolve_suffix_template(".{resolution}-{codec}", &m), ".1080p-h265");
    }

    #[test]
    fn drops_empty_var_and_its_trailing_separator_but_keeps_leading_dot() {
        let m = meta("h265", "", "apple-videotoolbox"); // empty resolution
        assert_eq!(resolve_suffix_template(".{resolution}-{codec}", &m), ".h265");
    }

    #[test]
    fn drops_empty_var_and_its_leading_separator() {
        let m = meta("", "1080p", ""); // empty codec
        assert_eq!(resolve_suffix_template(".{resolution}-{codec}", &m), ".1080p");
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib handbrake`
Expected: all pass. The separator-removal cases (`handbrake.rs:207-259`) are the gnarly part — if any fail, that's a suffix-naming bug worth reporting.

### Task 1.4: Rust `is_video_file`

**Files:**
- Modify: `src-tauri/src/commands/queue.rs` (append a test module at end of file)

- [ ] **Step 1: Add the test module**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn accepts_known_video_extensions_case_insensitively() {
        assert!(is_video_file(Path::new("movie.mp4")));
        assert!(is_video_file(Path::new("movie.MKV")));
        assert!(is_video_file(Path::new("/a/b/c.MoV")));
    }

    #[test]
    fn rejects_non_video_and_extensionless() {
        assert!(!is_video_file(Path::new("notes.txt")));
        assert!(!is_video_file(Path::new("README")));
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib queue`
Expected: all pass.

- [ ] **Step 3: Run the full suite + commit Phase 1**

Run: `cargo test --manifest-path src-tauri/Cargo.toml && npm test`
Expected: everything green.

```bash
git add src/lib/format.test.ts src-tauri/src/converter.rs src-tauri/src/handbrake.rs src-tauri/src/commands/queue.rs
git commit -m "test: cover pure formatting, progress parsing, and file-type logic"
```

---

## Phase 2 — Rust DB logic (in-memory) + testability refactors *(outline)*

Designed when started; needs small refactors first. Each refactor is surgical: move logic, don't change behavior.

- **Refactor A — cleanup decision (highest value, `converter.rs:371-402`).** Extract a pure fn
  `decide_cleanup(original_size: i64, converted_size: i64) -> (KeptFile, i64, &'static str)` returning
  which file to keep, `space_saved`, and status (`done`/`skipped`). Leave the actual `trash`/`remove_file`
  at the call site. Test the matrix: converted smaller, converted larger (→ skipped), converted equal,
  zero converted size. This path deletes a file, so its decision must be locked down.
- **Refactor B — preset classification (`handbrake.rs:61-185`).** Split codec/quality/device mapping into a
  pure fn over a parsed `serde_json::Value` + preset name. Table-test encoder→codec (h265/hevc/x265 → h265,
  etc.) and name→device mappings.
- **DB tests (in-memory).** Helper that opens `Connection::open_in_memory()` and runs `db::init_db`.
  - `init_db` seeds expected defaults and is idempotent.
  - `add_files_inner` skip rules (`queue.rs:142-194`): already-in-queue dedup, output-exists, source-has-suffix, `skip_already_converted` UNION. Use `tempfile` for real files where size/existence is read.
  - `get_next_queue_order`, `reorder_queue` (transaction), `get_history` search/sort/pagination, `get_history_summary`.

Add `tempfile` to `[dev-dependencies]` in `src-tauri/Cargo.toml` when this phase starts.

## Phase 3 — Frontend hooks & components (mocked Tauri) *(outline)*

Add `@testing-library/react @testing-library/jest-dom @testing-library/user-event` and a `src/test/setup.ts`
importing `@testing-library/jest-dom/vitest`. Mock `@tauri-apps/api/core` (`invoke`) and `/event` (`listen`)
with `vi.mock`.
- `useQueue`/`useHistory`/`useSettings`: assert `activeJob`/`pendingJobs` derivation and event-driven refresh.
- `DropZone`: the ≤5 auto-add vs. >5 confirm threshold and the "start queue once folders resolved" flow.

## Phase 4 — Process orchestration (optional) *(outline)*

> **Status (2026-06-17): intentionally skipped.** Phases 0–3 are complete and merged to `main`.
> This phase is gated on "only if orchestration keeps regressing" — no regressions observed, and the
> integration test of `process_queue` is high-effort (live `AppHandle`, spawned processes/threads).
> Revisit if orchestration starts regressing.

Integration test of `process_queue` against a fake `HandBrakeCLI` shell script emitting canned progress lines
and a known-size output. High effort; only if orchestration keeps regressing. Do **not** test `libc::kill`
pause/resume or `AppHandle.emit` directly — cover the decision around them (`can_pause_process` gating, the
non-macOS `pause_after_current` fallback) and leave syscalls to manual verification.

---

## Self-Review

- **Spec coverage:** Phase 0 (harness + CI), Phase 1 (all pure fns named in the review: `parse_progress`,
  `slugify`, `resolve_suffix_template`, `format_bytes_short`, `is_video_file`, `lib/format.ts`),
  Phases 2–4 (refactors + DB + frontend + orchestration) all map to the original recommendation. ✓
- **Placeholders:** Phase 0–1 steps contain complete, runnable code and exact commands. Phases 2–4 are
  intentionally outline-level (flagged as such) because they depend on refactors to be designed at start time.
- **Type/name consistency:** `decide_cleanup`/`KeptFile` introduced in Phase 2 only; `PresetMetadata`,
  `parse_progress`, `format_bytes_short`, `is_video_file`, `slugify`, `resolve_suffix_template` match the
  signatures in the current source. ✓
