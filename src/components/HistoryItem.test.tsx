import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

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
    failure_class: null,
    queue_order: 0,
    created_at: "",
    started_at: null,
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

  it("forwards right-click to onContextMenu and suppresses the native menu", () => {
    const onContextMenu = vi.fn();
    const j = job({});
    render(<HistoryItem job={j} onContextMenu={onContextMenu} />);

    const prevented = !fireEvent.contextMenu(screen.getByText("clip.mp4"));

    expect(onContextMenu).toHaveBeenCalledWith(expect.anything(), j);
    expect(prevented).toBe(true);
  });

  it("still suppresses the native menu without an onContextMenu handler", () => {
    render(<HistoryItem job={job({})} />);
    expect(!fireEvent.contextMenu(screen.getByText("clip.mp4"))).toBe(true);
  });

  const timed = { started_at: "2026-08-01T10:00:00+00:00", completed_at: "2026-08-01T10:12:34+00:00" };

  it("shows the encode duration when the setting is on", () => {
    render(<HistoryItem job={job(timed)} showDuration />);
    expect(screen.getByText("12m 34s")).toBeInTheDocument();
  });

  it("shows nothing when the setting is off", () => {
    render(<HistoryItem job={job(timed)} showDuration={false} />);
    expect(screen.queryByText("12m 34s")).toBeNull();
  });

  it("shows nothing for a job with no start time", () => {
    // A row predating the column, or one whose encode was paused. Blank is honest;
    // a fabricated time is not.
    render(<HistoryItem job={job({ started_at: null, completed_at: "2026-08-01T10:12:34+00:00" })} showDuration />);
    expect(screen.queryByText(/\ds/)).toBeNull();
  });

  it("shows nothing when the stamps run backwards", () => {
    // A clock adjustment between the two stamps. Rendering "-3m 00s" or "0s" would look
    // like a bug in the encode rather than in the clock.
    render(
      <HistoryItem
        job={job({ started_at: "2026-08-01T10:12:34+00:00", completed_at: "2026-08-01T10:00:00+00:00" })}
        showDuration
      />,
    );
    // Probe by BOTH title and rendered text: a title-only probe passes vacuously forever if
    // the implementation ever drops or rewords the attribute, including while it happily
    // renders a bogus negative duration.
    expect(screen.queryByTitle("Encode time")).toBeNull();
    expect(screen.queryByText(/^(<1s|\d+[smh])/)).toBeNull();
  });

  it("shows the duration of a skipped encode, which is the point of the feature", () => {
    // 'skipped' is a POST-encode status: the encode ran to completion and the output came
    // out no smaller. That wasted time is exactly what the user wants to see.
    render(<HistoryItem job={job({ ...timed, status: "skipped", kept_file: "original" })} showDuration />);
    expect(screen.getByText("12m 34s")).toBeInTheDocument();
  });

  it("puts the duration BESIDE the error message, not inside the ellipsised element", () => {
    // The load-bearing requirement is topological. If the duration ends up INSIDE the
    // element that owns overflow:hidden + text-overflow:ellipsis, it gets clipped out of
    // sight at every width — and an assertion that merely finds both on screen passes
    // happily in exactly that broken state.
    const full = "Conversion failed: moov atom not found\n[mov] moov atom not found";
    render(<HistoryItem job={job({ ...timed, status: "error", error_message: full })} showDuration />);

    const duration = screen.getByText("12m 34s");
    const msg = screen.getByText(/moov atom not found/);
    expect(msg.title).toBe(full);
    expect(msg).not.toContainElement(duration); // the ellipsis owner must be a leaf
    expect(msg.parentElement).toContainElement(duration); // and they share the row
  });

  it("renders a bottom row for the duration when there are no sizes to show", () => {
    // original_size is null when both stat fallbacks failed at add time. The duration
    // still has to land somewhere.
    render(<HistoryItem job={job({ ...timed, original_size: null, converted_size: null, space_saved: null })} showDuration />);
    expect(screen.getByText("12m 34s")).toBeInTheDocument();
  });
});
