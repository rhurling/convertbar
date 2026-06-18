# Release Script Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move ConvertBar's deterministic release mechanics into `scripts/release.sh`, gated by a local `ask` permission, and shrink the `/release` skill to a thin orchestrator.

**Architecture:** A single bash script owns bump → build → commit → push → PR → merge → tag. It runs interactively by default (prompts before the irreversible merge/tag/push) and end-to-end with `--yes`; `--dry-run` previews without mutating. The `/release` skill supplies judgment (version, notes) and calls the script with `--yes`. Each Claude invocation is approved via an `ask` permission — the deliberate, visible release gate (the global `git push` deny never sees the script's internal pushes, so the user's approval of the script run is the gate).

**Tech Stack:** Bash, `npm version`, `gh` CLI, git (signed commits/tags), vitest (test harness, shells out to the script).

**Spec:** `docs/superpowers/specs/2026-06-18-release-script-design.md`

---

## File Structure

- **Create** `scripts/release.sh` — the release mechanics (one file, function-per-stage + `main` dispatcher).
- **Create** `src/test/release-script.test.ts` — vitest test exercising the two safe surfaces: `--dry-run` (pure) and a guaranteed preflight abort.
- **Modify** `.claude/skills/release/SKILL.md` — rewrite to thin orchestrator.
- **Create** `/Users/rhurling/Sites/convertbar/.claude/settings.local.json` — local `ask` permission (gitignored; written to the **main checkout**, not the worktree, so it survives worktree cleanup; NOT part of the PR).

**Safe-testing rule (applies to every task):** the only script invocations a test may make are `--dry-run` (mutates nothing) and a version that is guaranteed older than current (`0.0.1`, which fails preflight's first check before any mutation/network). Never invoke the script in real mode with a newer version from a test.

---

### Task 1: Script skeleton — arg parsing, version resolution, `--dry-run` plan

**Files:**
- Create: `scripts/release.sh`
- Test: `src/test/release-script.test.ts`

- [ ] **Step 1: Write the failing test**

Create `src/test/release-script.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

function git(args: string[]): string {
  return execFileSync("git", args, { encoding: "utf8" }).trim();
}

function runScript(args: string[]): { status: number; stdout: string; stderr: string } {
  try {
    const stdout = execFileSync("bash", ["scripts/release.sh", ...args], { encoding: "utf8" });
    return { status: 0, stdout, stderr: "" };
  } catch (e: any) {
    return {
      status: e.status ?? 1,
      stdout: e.stdout?.toString() ?? "",
      stderr: e.stderr?.toString() ?? "",
    };
  }
}

const currentVersion = (): string =>
  JSON.parse(readFileSync("package.json", "utf8")).version;

function bumpMinor(v: string): string {
  const [maj, min] = v.split(".").map(Number);
  return `${maj}.${min + 1}.0`;
}

describe("release.sh", () => {
  it("--dry-run prints the plan for the resolved version and mutates nothing", () => {
    const target = bumpMinor(currentVersion());
    const before = git(["status", "--porcelain"]);

    const { status, stdout } = runScript(["minor", "--dry-run"]);

    expect(status).toBe(0);
    expect(stdout).toContain("DRY RUN");
    expect(stdout).toContain(`Target version: ${target}`);
    expect(stdout).toContain(`v${target}`);
    expect(git(["status", "--porcelain"])).toBe(before);
    expect(git(["branch", "--list", `chore/release-${target}`])).toBe("");
    expect(git(["tag", "--list", `v${target}`])).toBe("");
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npx vitest run src/test/release-script.test.ts`
Expected: FAIL — `bash: scripts/release.sh: No such file or directory` (non-zero status, no "DRY RUN" output).

- [ ] **Step 3: Write minimal implementation**

Create `scripts/release.sh` with the full skeleton. Later tasks replace the `# STUB` functions:

```bash
#!/usr/bin/env bash
# Cut a ConvertBar release: bump → build → commit → push → PR → merge → tag.
# See docs/superpowers/specs/2026-06-18-release-script-design.md
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

YES=0

usage() {
  cat <<'EOF'
Usage: scripts/release.sh <X.Y.Z | patch | minor | major> [--yes|-y|--force] [--dry-run] [--notes <file>]

  <version>     explicit X.Y.Z, or a bump keyword (patch/minor/major)
  --yes, -y     run end-to-end without the merge/tag/push confirmation (used by Claude)
  --force       alias for --yes
  --dry-run     print the plan and exit; changes nothing
  --notes FILE  PR body source (defaults to a minimal body)
EOF
}

current_version() { node -p "require('./package.json').version"; }

# Is $1 strictly greater than $2 (semver)?
semver_gt() {
  [ "$1" != "$2" ] && \
    [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]
}

resolve_version() {
  local arg="$1" cur major minor patch
  cur="$(current_version)"
  case "$arg" in
    major|minor|patch)
      IFS=. read -r major minor patch <<< "$cur"
      case "$arg" in
        major) major=$((major + 1)); minor=0; patch=0 ;;
        minor) minor=$((minor + 1)); patch=0 ;;
        patch) patch=$((patch + 1)) ;;
      esac
      echo "${major}.${minor}.${patch}"
      ;;
    *)
      if [[ "$arg" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "$arg"
      else
        echo "error: version must be X.Y.Z or patch|minor|major (got '$arg')" >&2
        exit 1
      fi
      ;;
  esac
}

print_plan() {
  local target="$1"
  cat <<EOF
DRY RUN — no changes will be made.
Target version: $target  (current: $(current_version))
Planned steps:
  1. Bump manifests (tauri.conf.json, package.json, Cargo.toml) + lockfile to $target
  2. Build: npm run tauri build
  3. Commit (signed) on branch chore/release-$target
  4. Push branch + open PR
  5. Confirm checkpoint (skipped with --yes)
  6. Merge PR (admin squash) + tag v$target + push tag (triggers CI release)
EOF
}

# STUB — replaced in Task 2
preflight() { echo "stub: preflight not implemented" >&2; exit 1; }
# STUB — replaced in Task 3
bump_manifests() { echo "stub: bump_manifests not implemented" >&2; exit 1; }
# STUB — replaced in Task 3
build_app() { echo "stub: build_app not implemented" >&2; exit 1; }
# STUB — replaced in Task 3
commit_release() { echo "stub: commit_release not implemented" >&2; exit 1; }
# STUB — replaced in Task 4
push_and_pr() { echo "stub: push_and_pr not implemented" >&2; exit 1; }
# STUB — replaced in Task 4
confirm_checkpoint() { echo "stub: confirm_checkpoint not implemented" >&2; exit 1; }
# STUB — replaced in Task 4
merge_and_tag() { echo "stub: merge_and_tag not implemented" >&2; exit 1; }

main() {
  local version_arg="" dry=0 notes=""
  while [ $# -gt 0 ]; do
    case "$1" in
      -y|--yes|--force) YES=1 ;;
      --dry-run) dry=1 ;;
      --notes)
        [ $# -ge 2 ] || { echo "error: --notes requires a file argument" >&2; usage >&2; exit 1; }
        notes="$2"; shift ;;
      -h|--help) usage; exit 0 ;;
      -*) echo "error: unknown option '$1'" >&2; usage >&2; exit 1 ;;
      *)
        if [ -z "$version_arg" ]; then version_arg="$1";
        else echo "error: unexpected argument '$1'" >&2; exit 1; fi
        ;;
    esac
    shift
  done

  [ -n "$version_arg" ] || { echo "error: version argument required" >&2; usage >&2; exit 1; }

  local target
  target="$(resolve_version "$version_arg")"

  if [ "$dry" = "1" ]; then print_plan "$target"; exit 0; fi

  preflight "$target"
  bump_manifests "$target"
  build_app
  commit_release "$target"
  push_and_pr "$target" "$notes"
  confirm_checkpoint "$target"
  merge_and_tag "$target"
}

main "$@"
```

Then make it executable: `chmod +x scripts/release.sh`

- [ ] **Step 4: Run test to verify it passes**

Run: `npx vitest run src/test/release-script.test.ts`
Expected: PASS (1 test). The `--dry-run` path resolves `minor` → `0.10.0`, prints the plan, and exits before any stub is reached.

- [ ] **Step 5: Commit**

```bash
git add scripts/release.sh src/test/release-script.test.ts
git commit -S -m "feat(release): script skeleton with --dry-run plan"
```

---

### Task 2: Preflight checks (real mode aborts; version check first)

**Files:**
- Modify: `scripts/release.sh` (replace the `preflight` stub)
- Test: `src/test/release-script.test.ts` (add a case)

- [ ] **Step 1: Write the failing test**

Append this `it(...)` inside the `describe("release.sh", ...)` block in `src/test/release-script.test.ts`:

```ts
  it("aborts in preflight when the version is not newer, mutating nothing", () => {
    const before = git(["status", "--porcelain"]);

    const { status, stderr } = runScript(["0.0.1"]);

    expect(status).not.toBe(0);
    expect(stderr).toContain("not newer");
    expect(git(["status", "--porcelain"])).toBe(before);
    expect(git(["branch", "--list", "chore/release-0.0.1"])).toBe("");
    expect(git(["tag", "--list", "v0.0.1"])).toBe("");
  });
```

- [ ] **Step 2: Run test to verify it fails**

Run: `npx vitest run src/test/release-script.test.ts`
Expected: FAIL — current code reaches the `preflight` stub, which prints `stub: preflight not implemented` (not `not newer`). Status is non-zero but `stderr` lacks `"not newer"`.

- [ ] **Step 3: Write minimal implementation**

In `scripts/release.sh`, replace the preflight stub line:

```bash
# STUB — replaced in Task 2
preflight() { echo "stub: preflight not implemented" >&2; exit 1; }
```

with the real implementation (version check FIRST so the test aborts deterministically before any network/mutation):

```bash
preflight() {
  local target="$1" cur branch
  cur="$(current_version)"

  # 1. Version must be strictly newer (checked first — deterministic).
  if ! semver_gt "$target" "$cur"; then
    echo "error: version $target is not newer than current $cur" >&2
    exit 1
  fi
  # 2. Required tools.
  local t
  for t in node npm git gh; do
    command -v "$t" >/dev/null 2>&1 || { echo "error: required tool '$t' not found" >&2; exit 1; }
  done
  # 3. gh authenticated.
  gh auth status >/dev/null 2>&1 || { echo "error: gh not authenticated (run: gh auth login)" >&2; exit 1; }
  # 4. On main.
  branch="$(git branch --show-current)"
  [ -n "$branch" ] || { echo "error: HEAD is detached — checkout main first" >&2; exit 1; }
  [ "$branch" = "main" ] || { echo "error: must be on main (currently on '$branch')" >&2; exit 1; }
  # 5. Clean working tree.
  [ -z "$(git status --porcelain)" ] || { echo "error: working tree is not clean" >&2; exit 1; }
  # 6. In sync with origin/main.
  git fetch --quiet origin main || { echo "error: could not fetch from origin (are you online?)" >&2; exit 1; }
  local head origin_head
  head="$(git rev-parse HEAD)"
  origin_head="$(git rev-parse origin/main 2>/dev/null)" || \
    { echo "error: could not resolve origin/main — does the remote use a different default branch?" >&2; exit 1; }
  [ "$head" = "$origin_head" ] || { echo "error: local main is not in sync with origin/main" >&2; exit 1; }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `npx vitest run src/test/release-script.test.ts`
Expected: PASS (2 tests). `0.0.1` fails the version check immediately — no tools/gh/git-state/network checks run, nothing mutated.

- [ ] **Step 5: Commit**

```bash
git add scripts/release.sh src/test/release-script.test.ts
git commit -S -m "feat(release): preflight checks with fail-fast version guard"
```

---

### Task 3: Local mutation pipeline — bump, build, signed commit

No safe automated test (this builds and commits). Verify by reading the diff and by the Task 7 dry-run smoke. Keep changes minimal and exact.

**Files:**
- Modify: `scripts/release.sh` (replace `bump_manifests`, `build_app`, `commit_release` stubs)

- [ ] **Step 1: Replace `bump_manifests`**

Replace:

```bash
# STUB — replaced in Task 3
bump_manifests() { echo "stub: bump_manifests not implemented" >&2; exit 1; }
```

with:

```bash
bump_manifests() {
  local target="$1"
  # package.json + package-lock.json in one step (no git tag/commit).
  npm version "$target" --no-git-tag-version >/dev/null
  # tauri.conf.json: first "version" key is the top-level app version.
  perl -0pi -e 's/("version":\s*")[^"]*"/${1}'"$target"'"/' src-tauri/tauri.conf.json
  # Cargo.toml: first line-anchored version is the [package] version.
  perl -0pi -e 's/^version = "[^"]*"/version = "'"$target"'"/m' src-tauri/Cargo.toml
  echo "Bumped manifests + lockfile to $target."
}
```

- [ ] **Step 2: Replace `build_app`**

Replace:

```bash
# STUB — replaced in Task 3
build_app() { echo "stub: build_app not implemented" >&2; exit 1; }
```

with (success = reaching `Finished N bundles`; the trailing signing-key error is expected locally and ignored regardless of exit code):

```bash
build_app() {
  echo "Building (bakes the version into the binary)..."
  local out
  out="$(npm run tauri build 2>&1)" || true
  if printf '%s' "$out" | grep -qE "Finished [0-9]+ bundles"; then
    echo "Build OK — bundles produced."
  else
    printf '%s\n' "$out" | tail -30 >&2
    echo "error: build did not reach bundling — aborting before any commit or push." >&2
    exit 1
  fi
}
```

- [ ] **Step 3: Replace `commit_release`**

Replace:

```bash
# STUB — replaced in Task 3
commit_release() { echo "stub: commit_release not implemented" >&2; exit 1; }
```

with (the `-am` sweeps in the bumped manifests plus the build-refreshed `Cargo.lock` and the `npm version`-refreshed `package-lock.json` — all tracked):

```bash
commit_release() {
  local target="$1"
  git switch -c "chore/release-$target"
  git commit -S -am "chore: bump version to $target"
  echo "Committed (signed) on chore/release-$target."
}
```

- [ ] **Step 4: Verify the script still parses and dry-run is unaffected**

Run: `bash -n scripts/release.sh` (syntax check) — Expected: no output, exit 0.
Run: `npx vitest run src/test/release-script.test.ts` — Expected: PASS (2 tests; dry-run and preflight paths never call these functions).

- [ ] **Step 5: Commit**

```bash
git add scripts/release.sh
git commit -S -m "feat(release): bump, build, and signed-commit stages"
```

---

### Task 4: Remote pipeline — push, PR, checkpoint, merge, tag

No safe automated test (pushes and merges). Verify by reading; exercised for real only during an actual release.

**Files:**
- Modify: `scripts/release.sh` (replace `push_and_pr`, `confirm_checkpoint`, `merge_and_tag` stubs)

- [ ] **Step 1: Replace `push_and_pr`**

Replace:

```bash
# STUB — replaced in Task 4
push_and_pr() { echo "stub: push_and_pr not implemented" >&2; exit 1; }
```

with:

```bash
push_and_pr() {
  local target="$1" notes_file="$2" branch="chore/release-$1"
  git push -u origin "$branch"
  if [ -n "$notes_file" ] && [ -f "$notes_file" ]; then
    gh pr create --base main --head "$branch" --title "Release $target" --body-file "$notes_file"
  else
    gh pr create --base main --head "$branch" --title "Release $target" --body "Release $target"
  fi
}
```

- [ ] **Step 2: Replace `confirm_checkpoint`**

Replace:

```bash
# STUB — replaced in Task 4
confirm_checkpoint() { echo "stub: confirm_checkpoint not implemented" >&2; exit 1; }
```

with (EOF/non-tty input → treated as "no", the safe default):

```bash
confirm_checkpoint() {
  local target="$1" answer
  [ "$YES" = "1" ] && return 0
  printf 'Merge PR, tag v%s and push (triggers CI release)? [y/N] ' "$target"
  read -r answer || answer=""
  case "$answer" in
    y|Y|yes|YES) return 0 ;;
    *) echo "Aborted at checkpoint — PR left open for manual merge."; exit 0 ;;
  esac
}
```

- [ ] **Step 3: Replace `merge_and_tag`**

Replace:

```bash
# STUB — replaced in Task 4
merge_and_tag() { echo "stub: merge_and_tag not implemented" >&2; exit 1; }
```

with (switch to main BEFORE merge so `--delete-branch` can drop the local branch; tag the squash-merged commit; the tag push triggers CI):

```bash
merge_and_tag() {
  local target="$1"
  git switch main
  gh pr merge "chore/release-$target" --admin --squash --delete-branch
  git pull --ff-only
  git tag -s "v$target" -m "v$target"
  git push origin "v$target"
  echo "Released v$target — CI build triggered."
  gh release view "v$target" --json url -q .url 2>/dev/null || true
}
```

- [ ] **Step 4: Verify parse + tests unaffected**

Run: `bash -n scripts/release.sh` — Expected: exit 0, no output.
Run: `npx vitest run src/test/release-script.test.ts` — Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add scripts/release.sh
git commit -S -m "feat(release): push, PR, checkpoint, merge, and tag stages"
```

---

### Task 5: Rewrite the `/release` skill as a thin orchestrator

**Files:**
- Modify: `.claude/skills/release/SKILL.md` (replace entire contents)

- [ ] **Step 1: Replace the skill file contents**

Overwrite `.claude/skills/release/SKILL.md` with:

```markdown
---
name: release
description: Use when cutting a new ConvertBar release or bumping its version — picks the version and release notes, then hands the mechanical bump/build/commit/PR/merge/tag/push to scripts/release.sh.
disable-model-invocation: true
---

# Release ConvertBar

The deterministic mechanics live in `scripts/release.sh`. This skill supplies the judgment around it and interprets the result.

## Steps

1. **Determine the target version.** Use the argument if given. Otherwise read the current version from `package.json` and ask which part to bump (major/minor/patch).
2. **Draft release notes.** Run the `/release-notes` skill over `<previous-tag>..HEAD` and write its markdown to a temp file, e.g. `/tmp/release-notes.md`.
3. **Run the release script.** You will be prompted to approve it — that approval is the release gate:
   ```
   ./scripts/release.sh <X.Y.Z|patch|minor|major> --yes --notes /tmp/release-notes.md
   ```
   `--yes` runs it non-interactively. The script bumps all manifests + the lockfile, rebuilds (baking the version into the binary), commits signed on a release branch, opens a PR, admin-squash-merges it, then tags the merged commit and pushes the tag — which triggers the CI release.
4. **Interpret the result.**
   - Success is the script reaching `Finished N bundles`; it already ignores the trailing `TAURI_SIGNING_PRIVATE_KEY` error (a CI-only secret, absent locally). Do not flag that error.
   - On any non-zero exit, STOP and report what failed. The script makes no commit or push until the build succeeds; if it aborts after pushing, it leaves an open PR.
   - On success, report the printed release URL.

## Preview / manual use

- `./scripts/release.sh <version> --dry-run` prints the plan and changes nothing.
- Run without `--yes` to get a `[y/N]` confirmation before the merge/tag/push (for a human running it directly).

## Notes

- `main` is protected (PR-only, no merge commits, signed commits). The script honours this: it lands the bump via an admin squash-merge and tags the merged commit; the **tag** push (not a branch push) triggers `.github/workflows/build.yml`.
- The script is registered as an `ask` permission in `.claude/settings.local.json`, so every run prompts for approval before anything happens.
```

- [ ] **Step 2: Commit**

```bash
git add .claude/skills/release/SKILL.md
git commit -S -m "docs(release): rewrite skill as thin orchestrator over release.sh"
```

---

### Task 6: Local `ask` permission (main checkout, not committed)

`.claude/settings.local.json` is gitignored, so it is NOT part of the PR. Write it to the **main checkout** (not the worktree) so it survives worktree cleanup and applies when releases run from the main repo.

**Files:**
- Create/modify: `/Users/rhurling/Sites/convertbar/.claude/settings.local.json`

- [ ] **Step 1: Check whether the file already exists**

Run: `cat /Users/rhurling/Sites/convertbar/.claude/settings.local.json 2>/dev/null || echo "MISSING"`

- [ ] **Step 2: Write the ask permission**

If MISSING, create `/Users/rhurling/Sites/convertbar/.claude/settings.local.json` with:

```json
{
  "permissions": {
    "ask": [
      "Bash(./scripts/release.sh:*)",
      "Bash(scripts/release.sh:*)",
      "Bash(bash scripts/release.sh:*)"
    ]
  }
}
```

If it already exists, merge these three entries into the existing `permissions.ask` array (create `permissions`/`ask` if absent), preserving all other keys.

- [ ] **Step 3: Verify**

Run: `node -e "const c=require('/Users/rhurling/Sites/convertbar/.claude/settings.local.json'); if(!c.permissions.ask.includes('Bash(./scripts/release.sh:*)')) process.exit(1); console.log('ask permission present')"`
Expected: `ask permission present`

No commit — the file is gitignored and intentionally not in the PR.

---

### Task 7: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full test suite**

Run: `npm test`
Expected: all tests pass, including the 2 new `release.sh` cases (36 total, up from 34).

- [ ] **Step 2: Dry-run smoke for each version form**

Run:
```bash
bash scripts/release.sh patch --dry-run
bash scripts/release.sh minor --dry-run
bash scripts/release.sh major --dry-run
bash scripts/release.sh 1.2.3 --dry-run
```
Expected: each prints a plan with the correct resolved target (`patch`→0.9.6, `minor`→0.10.0, `major`→1.0.0, explicit→1.2.3) and exits 0. `git status --porcelain` is unchanged afterward.

- [ ] **Step 3: Bad-input smoke**

Run: `bash scripts/release.sh banana --dry-run; echo "exit=$?"`
Expected: error `version must be X.Y.Z or patch|minor|major (got 'banana')`, `exit=1`.

- [ ] **Step 4: Confirm no stray mutations**

Run: `git status --porcelain`
Expected: empty (all work committed; dry-runs changed nothing).

---

## Out of scope (YAGNI)

- `--resume` for a half-finished release (documented manual recovery only).
- Auto-reverting manifest edits on build failure (leaves them for inspection).
- Non-`main` base branches.
- Extending `.claude/hooks/check-version-sync.sh` to also check `package-lock.json` (the script now keeps it in sync; the hook stays as-is).
