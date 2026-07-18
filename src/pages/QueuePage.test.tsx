import { it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("../hooks/useQueue", () => ({
  useQueue: () => ({ activeJob: null, pendingJobs: [], progress: null, refresh: vi.fn() }),
}));
vi.mock("../components/DropZone", () => ({ default: () => <div data-testid="dropzone" /> }));

import QueuePage from "./QueuePage";

it("suppresses the empty-state while an add is in progress", () => {
  render(<QueuePage hbStatus={null} adding={{ opId: "a", done: 1, total: 5 }} isAdding={true} />);
  expect(screen.queryByText(/drag video files or folders here to get started/i)).toBeNull();
  expect(screen.getByText(/checking 1 of 5/i)).toBeInTheDocument();
});

it("shows the empty-state when idle", () => {
  render(<QueuePage hbStatus={null} adding={null} isAdding={false} />);
  expect(screen.getByText(/drag video files or folders here to get started/i)).toBeInTheDocument();
});

it("suppresses the empty-state while adding even before the first progress tick", () => {
  render(<QueuePage hbStatus={null} adding={null} isAdding={true} />);
  expect(screen.queryByText(/drag video files or folders here to get started/i)).toBeNull();
});
