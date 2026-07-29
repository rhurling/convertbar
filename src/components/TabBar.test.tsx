import { it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("../lib/tauri", () => ({ commands: { hideWindow: vi.fn() } }));

import TabBar, { type Tab } from "./TabBar";

const noop = () => {};

const allTabs: Tab[] = ["queue", "history", "watch", "settings"];

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

it("stays mounted with its drag region and close button when no tabs are pinned", () => {
  // three-col pins every tab, so `tabs` arrives empty — the bar must still carry
  // data-tauri-drag-region, the adding spinner, and the desktop close button, none of
  // which have another home.
  const { container } = render(
    <TabBar tabs={[]} activeTab={undefined} onTabChange={noop} isAdding={true} updateAvailable={false} />,
  );
  expect(container.querySelectorAll(".tab-btn:not(.close-tab-btn)")).toHaveLength(0);
  expect(container.querySelector("[data-tauri-drag-region]")).not.toBeNull();
  expect(container.querySelector(".tab-spinner")).not.toBeNull();
  expect(screen.getByTitle("Close")).toBeInTheDocument();
});
