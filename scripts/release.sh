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
