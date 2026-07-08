import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { invoke } from "@tauri-apps/api/core";
import ActiveJob from "./ActiveJob";
import type { JobInfo } from "../lib/tauri";

const invokeMock = vi.mocked(invoke);

function job(overrides: Partial<JobInfo> = {}): JobInfo {
  return {
    id: "1",
    source_path: "/in/clip.mp4",
    output_path: "/out/clip.mp4",
    preset: "p",
    status: "encoding",
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

// Backend state the mount reads; a test mutates these before rendering.
let canPause: boolean;
let pauseAfter: boolean;
let rejectCancel: boolean;

beforeEach(() => {
  vi.clearAllMocks();
  canPause = true;
  pauseAfter = false;
  rejectCancel = false;
  invokeMock.mockImplementation(((cmd: string) => {
    switch (cmd) {
      case "get_platform_capabilities":
        return Promise.resolve({ can_pause_process: canPause });
      case "get_pause_after_current":
        return Promise.resolve(pauseAfter);
      case "cancel_conversion":
        return rejectCancel
          ? Promise.reject(new Error("no active process"))
          : Promise.resolve(undefined);
      case "pause_conversion":
      case "resume_conversion":
      case "pause_after_current":
      case "cancel_pause_after_current":
        return Promise.resolve(undefined);
      default:
        return Promise.reject(new Error(`unexpected invoke: ${cmd}`));
    }
  }) as typeof invoke);
});

describe("ActiveJob", () => {
  it("seeds the pause-after-current button from the backend flag on mount", async () => {
    pauseAfter = true; // queue already armed elsewhere (e.g. the updater flow)

    render(<ActiveJob job={job()} progress={null} />);

    // Without reading the backend flag the button would wrongly read "Pause after this".
    await waitFor(() =>
      expect(screen.getByText("Will pause")).toBeInTheDocument(),
    );
  });

  it("surfaces an error when a control invoke rejects", async () => {
    rejectCancel = true;
    render(<ActiveJob job={job()} progress={null} />);
    await waitFor(() =>
      expect(screen.getByText("Cancel")).toBeInTheDocument(),
    );

    fireEvent.click(screen.getByText("Cancel"));

    await waitFor(() =>
      expect(screen.getByText(/no active process/)).toBeInTheDocument(),
    );
  });
});
