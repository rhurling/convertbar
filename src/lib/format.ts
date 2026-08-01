export function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(Math.abs(bytes)) / Math.log(k));
  return `${(Math.abs(bytes) / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

export function formatEta(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.round(seconds % 60);
  if (h > 0) return `${h}h${String(m).padStart(2, "0")}m`;
  return `${m}m${String(s).padStart(2, "0")}s`;
}

export function formatPercent(saved: number, original: number): string {
  if (original === 0) return "0%";
  return `${Math.round((saved / original) * 100)}%`;
}

export function fileName(path: string): string {
  // The backend sends OS-native paths, so Windows arrives with backslashes.
  const parts = path.split(/[/\\]/).filter(Boolean);
  return parts[parts.length - 1] || path;
}

/**
 * Seconds between a job's encode start and its completion, or null when there is no
 * duration to show: a missing stamp (never claimed, paused mid-encode, or a row predating
 * the column), an unparseable stamp, or a non-positive delta from a clock adjustment.
 */
export function durationSeconds(
  startedAt: string | null,
  completedAt: string | null,
): number | null {
  if (!startedAt || !completedAt) return null;
  const start = Date.parse(startedAt);
  const end = Date.parse(completedAt);
  if (Number.isNaN(start) || Number.isNaN(end)) return null;
  const delta = (end - start) / 1000;
  return delta > 0 ? delta : null;
}

// Deliberately not formatEta: the menu bar's ETA format is load-bearing elsewhere and
// must not shift to suit History.
export function formatDuration(seconds: number): string {
  const total = Math.round(seconds);
  if (total < 1) return "<1s";
  if (total < 60) return `${total}s`;
  if (total < 3600) {
    return `${Math.floor(total / 60)}m ${String(total % 60).padStart(2, "0")}s`;
  }
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  return `${h}h ${String(m).padStart(2, "0")}m`;
}
