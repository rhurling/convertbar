---
name: release-notes
description: Use when drafting release notes or a changelog for a ConvertBar release — summarizes commits since the previous git tag, grouped by conventional-commit type, into markdown ready to paste into the GitHub release.
---

# ConvertBar Release Notes

Draft human-readable release notes from the commits since the last release.

## Steps

1. **Pick the range.** To document the most recent release, diff the last two tags:
   ```
   PREV=$(git tag --sort=-version:refname | grep '^v' | sed -n '2p')
   CUR=$(git tag --sort=-version:refname | grep '^v' | sed -n '1p')
   ```
   For unreleased work, use `<latest-tag>..HEAD` instead.
2. **List commits:** `git log "$PREV..$CUR" --no-merges --pretty=format:'%s'`
3. **Group by conventional-commit prefix:**
   - `feat:` → **Features**
   - `fix:` → **Bug Fixes**
   - `refactor:` / `perf:` → **Improvements**
   - Omit pure `chore:` / `docs:` / `test:` (especially `chore: bump version`) unless notable.
4. **Rewrite each line** as a user-facing bullet — drop the prefix, make it readable. This is a menu-bar batch video-conversion app, so frame changes from the user's perspective.
5. **Output** the grouped markdown, plus a trailing compare link (mirrors what CI puts in the release body):
   `**Full changelog**: https://github.com/rhurling/convertbar/compare/$PREV...$CUR`

Print the notes — do not commit or create a release.
