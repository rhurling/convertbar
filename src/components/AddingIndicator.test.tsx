import { it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import AddingIndicator from "./AddingIndicator";

it("renders nothing when idle", () => {
  const { container } = render(<AddingIndicator activity={null} />);
  expect(container.firstChild).toBeNull();
});

it("shows an indeterminate scanning label before the first count", () => {
  render(<AddingIndicator activity={{ opId: "a", done: null, total: null }} />);
  expect(screen.getByText(/scanning/i)).toBeInTheDocument();
});

it("shows the count and a filled bar during probing", () => {
  render(<AddingIndicator activity={{ opId: "a", done: 3, total: 12 }} />);
  expect(screen.getByText(/checking 3 of 12/i)).toBeInTheDocument();
  const fill = document.querySelector(".progress-bar-fill") as HTMLElement;
  expect(fill.style.width).toBe("25%");
});
