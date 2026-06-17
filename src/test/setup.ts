import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// Vitest doesn't run with `globals`, so Testing Library's auto-cleanup hook
// isn't registered for us — unmount rendered trees between tests by hand.
afterEach(() => {
  cleanup();
});
