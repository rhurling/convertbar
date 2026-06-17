# Open Issues

## Clear dropdown hidden when History contains only errors

The summary/Clear bar in History is gated on `summary.total_files > 0`
(`src/pages/HistoryPage.tsx:14`), and `get_history_summary` counts only
`status = 'done'` jobs (`src-tauri/src/commands/queue.rs:450`, `:457`).

As a result, when History contains **only** errored jobs (no successful
conversions), the entire summary bar — including the "Clear Errors Only"
button — is hidden, so those errors can't be cleared from the UI.

**Proposed fix:** gate the bar on `history.length > 0` instead of
`summary.total_files > 0`, or include errors/skipped in the summary count.

_Deferred from the fix for "errored files don't appear in History"._
