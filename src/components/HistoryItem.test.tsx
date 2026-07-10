import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";

import HistoryItem from "./HistoryItem";
import type { JobInfo } from "../lib/tauri";

function job(overrides: Partial<JobInfo>): JobInfo {
  return {
    id: "1",
    source_path: "/m/clip.mp4",
    output_path: "/m/clip-conv.mp4",
    preset: "p",
    status: "done",
    original_size: 1000,
    converted_size: 500,
    kept_file: "converted",
    space_saved: 500,
    error_message: null,
    queue_order: 0,
    created_at: "",
    completed_at: null,
    ...overrides,
  };
}

describe("HistoryItem", () => {
  it("exposes the full error message on hover, since the row shows only one truncated line", () => {
    // The .history-item-error-msg row is nowrap+ellipsis, so only the first line is
    // visible. The whole message must still be reachable via the native title tooltip.
    const full =
      "Conversion failed: moov atom not found\n[mov] moov atom not found\nNo title found.";
    render(<HistoryItem job={job({ status: "error", error_message: full })} />);
    expect(screen.getByText(/moov atom not found/).title).toBe(full);
  });
});
