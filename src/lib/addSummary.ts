import type { AddResult, SkipReason } from "./tauri";

const SKIP_LABELS: Record<SkipReason, string> = {
  output_exists: "output exists",
  already_converted: "already converted",
  already_queued: "already queued",
  not_video: "not a video",
  already_at_target: "already at target",
};

// Fixed order so the rendered summary is deterministic regardless of backend ordering.
const REASON_ORDER: SkipReason[] = [
  "output_exists",
  "already_converted",
  "already_at_target",
  "already_queued",
  "not_video",
];

/** Aggregate one or more AddResults into a single human-readable status line, or null if empty. */
export function summarizeAdds(results: AddResult[]): string | null {
  const added = results.reduce((n, r) => n + r.added.length, 0);
  const counts = new Map<SkipReason, number>();
  for (const r of results) {
    for (const s of r.skipped) {
      counts.set(s.reason, (counts.get(s.reason) ?? 0) + s.count);
    }
  }

  const parts: string[] = [];
  if (added > 0) parts.push(`Added ${added}`);
  for (const reason of REASON_ORDER) {
    const count = counts.get(reason);
    if (count) parts.push(`${count} skipped (${SKIP_LABELS[reason]})`);
  }

  return parts.length > 0 ? parts.join(" · ") : null;
}
