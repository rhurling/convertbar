import { it, expect, vi, afterEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

vi.mock("../lib/tauri", () => ({ commands: { hideWindow: vi.fn() } }));

import TabBar, { tabId, tabPanelId, type Tab } from "./TabBar";

const noop = () => {};

const allTabs: Tab[] = ["queue", "history", "watch", "settings"];

afterEach(() => {
  vi.unstubAllEnvs();
});

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

it("exposes the tabs as a named tablist wired to the panel each one controls", () => {
  // Plain buttons told assistive tech nothing about the relationship: no set membership,
  // no which-one-is-showing, no link to the panel that appears when you press one.
  render(
    <TabBar tabs={allTabs} activeTab="history" onTabChange={noop} isAdding={false} updateAvailable={false} />,
  );

  expect(screen.getByRole("tablist")).toHaveAccessibleName("Panels");
  expect(screen.getAllByRole("tab")).toHaveLength(4);

  const history = screen.getByRole("tab", { name: "History" });
  expect(history).toHaveAttribute("aria-selected", "true");
  expect(history).toHaveAttribute("aria-controls", tabPanelId("history"));
  expect(history).toHaveAttribute("id", tabId("history"));

  const queue = screen.getByRole("tab", { name: "Queue" });
  expect(queue).toHaveAttribute("aria-selected", "false");
  // The unselected panels are unmounted, so pointing at their ids would dangle.
  expect(queue).not.toHaveAttribute("aria-controls");
});

it("moves focus with the arrow keys without switching panels until the tab is pressed", async () => {
  // Half the tablist pattern is worse than none: a screen-reader user told "tab, 2 of 4"
  // reaches for the arrow keys. Activation stays manual because switching tabs unmounts a
  // panel — Settings commits its drafts on the way out.
  const onTabChange = vi.fn();
  render(
    <TabBar tabs={allTabs} activeTab="queue" onTabChange={onTabChange} isAdding={false} updateAvailable={false} />,
  );

  const user = userEvent.setup();
  await user.tab();
  expect(screen.getByRole("tab", { name: "Queue" })).toHaveFocus();

  await user.keyboard("{ArrowRight}");
  expect(screen.getByRole("tab", { name: "History" })).toHaveFocus();
  expect(onTabChange).not.toHaveBeenCalled();

  await user.keyboard("{Enter}");
  expect(onTabChange).toHaveBeenCalledWith("history");

  // Only the selected tab is in the page's tab order; the arrow keys own the rest.
  expect(screen.getByRole("tab", { name: "Watch" })).toHaveAttribute("tabindex", "-1");
});

it("leaves modified arrow keys to the browser", async () => {
  // The server head runs in a real browser, where Cmd/Alt+Left is history-back. Claiming it
  // for tab navigation would strand the user on the page with no way back.
  render(
    <TabBar tabs={allTabs} activeTab="queue" onTabChange={noop} isAdding={false} updateAvailable={false} />,
  );

  const user = userEvent.setup();
  await user.tab();
  const queue = screen.getByRole("tab", { name: "Queue" });
  expect(queue).toHaveFocus();

  await user.keyboard("{Alt>}{ArrowRight}{/Alt}");
  expect(queue).toHaveFocus();
  await user.keyboard("{Meta>}{ArrowRight}{/Meta}");
  expect(queue).toHaveFocus();
});

it("titles the bar on the server head, which is the document's only h1 and its only content at three-col", async () => {
  // At three-col the server head has no tab buttons and no close button, so the bar
  // rendered as a 1px border — and every column jumped down 12px the moment the adding
  // spinner appeared. Measured in Chromium: 1px idle -> 13px adding. isServerHead is a
  // module-level const, so the env has to be stubbed and the module graph reloaded.
  vi.stubEnv("VITE_HEAD", "server");
  vi.resetModules();
  const { default: ServerTabBar } = await import("./TabBar");

  render(
    <ServerTabBar tabs={[]} activeTab={undefined} onTabChange={noop} isAdding={false} updateAvailable={false} />,
  );

  expect(screen.getByRole("heading", { level: 1, name: "ConvertBar" })).toBeInTheDocument();
  // The bar is the server head's only chrome: there is no window title bar to name the app.
  expect(screen.queryByTitle("Close")).not.toBeInTheDocument();
});
