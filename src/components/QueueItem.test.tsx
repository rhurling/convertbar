import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import QueueItem from "./QueueItem";
import type { JobInfo } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);

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
    failure_class: null,
    queue_order: 0,
    created_at: "",
    started_at: null,
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

  it("removes a job only once when the × is double-clicked", async () => {
    let resolveRemove: () => void = () => {};
    invokeMock.mockImplementation(((cmd: string) => {
      if (cmd === "remove_job")
        return new Promise<void>((r) => {
          resolveRemove = () => r(undefined);
        });
      return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }) as typeof invoke);

    render(<QueueItem job={job({})} onRemoved={() => {}} />);
    const button = screen.getByTitle("Remove");

    // The first click puts the removal in flight; a second click must not fire again.
    await act(async () => {
      fireEvent.click(button);
    });
    await act(async () => {
      fireEvent.click(button);
    });
    await act(async () => {
      resolveRemove();
    });

    expect(
      invokeMock.mock.calls.filter((c) => c[0] === "remove_job"),
    ).toHaveLength(1);
  });
});
