import { it, expect, vi } from "vitest";
import { render } from "@testing-library/react";

vi.mock("../lib/tauri", () => ({ commands: { hideWindow: vi.fn() } }));

import TabBar from "./TabBar";

const noop = () => {};

it("shows the spinner only while adding", () => {
  const { container, rerender } = render(
    <TabBar activeTab="queue" onTabChange={noop} isAdding={false} />,
  );
  expect(container.querySelector(".tab-spinner")).toBeNull();

  rerender(<TabBar activeTab="queue" onTabChange={noop} isAdding={true} />);
  expect(container.querySelector(".tab-spinner")).not.toBeNull();
});
