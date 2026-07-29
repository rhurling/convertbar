import { it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("../lib/tauri", () => ({ commands: { hideWindow: vi.fn() } }));

import TabBar from "./TabBar";

const noop = () => {};

const allTabs: Array<"queue" | "history" | "watch" | "settings"> = ["queue", "history", "watch", "settings"];

it("shows the spinner only while adding", () => {
  const { container, rerender } = render(
    <TabBar tabs={allTabs} activeTab="queue" onTabChange={noop} isAdding={false} updateAvailable={false} />,
  );
  expect(container.querySelector(".tab-spinner")).toBeNull();

  rerender(
    <TabBar tabs={allTabs} activeTab="queue" onTabChange={noop} isAdding={true} updateAvailable={false} />,
  );
  expect(container.querySelector(".tab-spinner")).not.toBeNull();
});

it("badges the Settings tab when an update is pending", () => {
  // A missed OS notification must not be the only signal that an update is waiting.
  const { rerender } = render(
    <TabBar tabs={allTabs} activeTab="queue" onTabChange={() => {}} isAdding={false} updateAvailable={false} />,
  );
  expect(screen.queryByLabelText(/update available/i)).toBeNull();

  rerender(
    <TabBar tabs={allTabs} activeTab="queue" onTabChange={() => {}} isAdding={false} updateAvailable={true} />,
  );
  expect(screen.getByLabelText(/update available/i)).toBeInTheDocument();
});
