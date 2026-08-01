import { describe, it, expect } from "vitest";
import { summarizeAdds } from "./addSummary";
import type { AddResult } from "./tauri";

describe("summarizeAdds", () => {
  it("returns null when nothing was added or skipped", () => {
    expect(summarizeAdds([{ added: [], skipped: [] }])).toBeNull();
  });

  it("reports only the added count when there are no skips", () => {
    const r: AddResult = {
      added: [{} as never, {} as never],
      skipped: [],
    };
    expect(summarizeAdds([r])).toBe("Added 2");
  });

  it("sums added and merges skip reasons across results in a stable order", () => {
    const a: AddResult = {
      added: [{} as never],
      skipped: [{ reason: "output_exists", count: 1 }],
    };
    const b: AddResult = {
      added: [{} as never, {} as never],
      skipped: [
        { reason: "output_exists", count: 2 },
        { reason: "already_converted", count: 1 },
      ],
    };
    expect(summarizeAdds([a, b])).toBe(
      "Added 3 · 3 skipped (output exists) · 1 skipped (already converted)",
    );
  });

  it("renders a skips-only summary when nothing was added", () => {
    const r: AddResult = {
      added: [],
      skipped: [{ reason: "not_video", count: 2 }],
    };
    expect(summarizeAdds([r])).toBe("2 skipped (not a video)");
  });

  it("labels and orders the already-at-target reason from skip-by-source-media", () => {
    const r: AddResult = {
      added: [{} as never],
      skipped: [
        { reason: "already_at_target", count: 2 },
        { reason: "already_converted", count: 1 },
      ],
    };
    // already_converted precedes already_at_target in the stable REASON_ORDER.
    expect(summarizeAdds([r])).toBe(
      "Added 1 · 1 skipped (already converted) · 2 skipped (already at target)",
    );
  });

  it("labels an in-place-blocked skip and sorts it first", () => {
    const summary = summarizeAdds([
      {
        added: [],
        skipped: [
          { reason: "not_video", count: 2 },
          { reason: "in_place_keep_blocked", count: 3 },
        ],
      },
    ]);
    // The label names the CAUSE, not a remedy: "needs Delete" was wrong on the desktop head,
    // where Trash permits in-place jobs too (only `keep` blocks them, queue_ops.rs), and it
    // steered users toward permanent deletion when the recoverable option would have done.
    expect(summary).toBe(
      "3 skipped (in-place encode can't keep the original) · 2 skipped (not a video)",
    );
  });
});
