import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import QueueItem from "./QueueItem";
import type { JobInfo } from "../lib/tauri";

function job(overrides: Partial<JobInfo>): JobInfo {
  return {
    id: "1",
    source_path: "/m/clip.mp4",
    output_path: "/m/clip-conv.mp4",
    preset: "p",
    status: "queued",
    original_size: null,
    converted_size: null,
    kept_file: null,
    space_saved: null,
    error_message: null,
    queue_order: 0,
    created_at: "",
    completed_at: null,
    ...overrides,
  };
}

describe("QueueItem", () => {
  it("shows an In place badge when output equals source", () => {
    render(<QueueItem job={job({ output_path: "/m/clip.mp4" })} onRemoved={() => {}} />);
    expect(screen.getByText("In place")).toBeInTheDocument();
  });

  it("shows no In place badge for a distinct output", () => {
    render(<QueueItem job={job({ output_path: "/m/clip-conv.mp4" })} onRemoved={() => {}} />);
    expect(screen.queryByText("In place")).not.toBeInTheDocument();
  });
});
