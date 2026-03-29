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
  return path.split("/").pop() || path;
}
