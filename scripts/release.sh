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
build_app() {
  echo "Building (bakes the version into the binary)..."
  local out
  # Exit code ignored on purpose: the local build ends with an expected (CI-only)
  # TAURI_SIGNING_PRIVATE_KEY error after bundling. Success is detected by the
  # "Finished N bundles" line below, not the exit code.
  out="$(npm run tauri build 2>&1)" || true
  if printf '%s' "$out" | grep -qE "Finished [0-9]+ bundles"; then
    echo "Build OK — bundles produced."
  else
    printf '%s\n' "$out" | tail -30 >&2
    echo "error: build did not reach bundling — aborting before any commit or push." >&2
    # bump_manifests already dirtied the tree; restore it so the next run's clean-tree
    # preflight fails for the right reason (or not at all), not with an unrelated message.
    echo "Restoring bumped manifests to leave a clean tree..." >&2
    git checkout -- package.json package-lock.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock 2>/dev/null ||
      echo "warning: restore FAILED — clean up manually with: git checkout -- package.json package-lock.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock" >&2
    exit 1
  fi
}
commit_release() {
  local target="$1"
  if git rev-parse --verify "chore/release-$target" >/dev/null 2>&1; then
    echo "error: branch chore/release-$target already exists — delete it or finish the previous run" >&2
    exit 1
  fi
  git switch -c "chore/release-$target"
  git commit -S -am "chore: bump version to $target"
  echo "Committed (signed) on chore/release-$target."
}
push_and_pr() {
  local target="$1" notes_file="$2"
  local branch="chore/release-$target"
  if [ -n "$notes_file" ] && [ ! -f "$notes_file" ]; then
    echo "warning: --notes file '$notes_file' not found — using a default PR body" >&2
  fi
  git push -u origin "$branch"
  if [ -n "$notes_file" ] && [ -f "$notes_file" ]; then
    gh pr create --base main --head "$branch" --title "Release $target" --body-file "$notes_file"
  else
    gh pr create --base main --head "$branch" --title "Release $target" --body "Release $target"
  fi
}
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
merge_and_tag() {
  local target="$1" pr sha head
  local branch="chore/release-$target"
  git switch main
  # Capture the PR number before the merge deletes the branch, so mergeCommit stays queryable.
  pr="$(gh pr view "$branch" --json number -q .number)"
  gh pr merge "$branch" --admin --squash --delete-branch
  # Tag the exact squash-merge commit, never "whatever main HEAD is after the pull":
  # replication lag can serve a pre-merge ref (then --ff-only succeeds trivially and would
  # tag the OLD commit, whose manifests still hold the previous version — tauri-action then
  # builds under the wrong tag and publish-release fails), and a concurrent commit could land
  # on main between the merge and the pull.
  # The PR IS merged past this point — every failure branch must hand the operator
  # the recovery recipe, or the run strands merged-but-untagged.
  sha="$(gh pr view "$pr" --json mergeCommit -q .mergeCommit.oid)" ||
    { echo "error: could not query the squash-merge commit for PR #$pr. The merge landed; recover with: gh pr view $pr --json mergeCommit  then  git tag -s v$target -m v$target <sha> && git push origin v$target" >&2; exit 1; }
  [ -n "$sha" ] || { echo "error: PR #$pr returned an empty mergeCommit. The merge landed; recover with: gh pr view $pr --json mergeCommit  then  git tag -s v$target -m v$target <sha> && git push origin v$target" >&2; exit 1; }
  git pull --ff-only || { echo "error: could not fast-forward main after merge — once main is at $sha, recover with: git tag -s v$target -m v$target $sha && git push origin v$target" >&2; exit 1; }
  head="$(git rev-parse HEAD)"
  if [ "$head" != "$sha" ]; then
    echo "error: main HEAD ($head) is not the release merge commit ($sha) — a commit may have landed after the merge, or the fetch is stale. Not tagging automatically." >&2
    echo "Once main is at $sha, recover with: git tag -s v$target -m v$target $sha && git push origin v$target" >&2
    exit 1
  fi
  git branch -D "$branch" 2>/dev/null || true
  git tag -s "v$target" -m "v$target" "$sha"
  git push origin "v$target"
  echo "Released v$target — CI build triggered."
  gh release view "v$target" --json url -q .url 2>/dev/null || true
}

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
