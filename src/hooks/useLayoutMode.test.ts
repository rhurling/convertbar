import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useLayoutMode } from "./useLayoutMode";

type Listener = () => void;
const listeners = new Map<string, Set<Listener>>();
let matching: string[] = [];

function installMatchMedia() {
  window.matchMedia = ((query: string) => ({
    matches: matching.includes(query),
    media: query,
    addEventListener: (_: string, fn: Listener) => {
      if (!listeners.has(query)) listeners.set(query, new Set());
      listeners.get(query)!.add(fn);
    },
    removeEventListener: (_: string, fn: Listener) => listeners.get(query)?.delete(fn),
  })) as unknown as typeof window.matchMedia;
}

beforeEach(() => {
  listeners.clear();
  matching = [];
  installMatchMedia();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useLayoutMode", () => {
  it("is tabs below the first breakpoint", () => {
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("tabs");
  });

  it("is two-col at the 900px breakpoint", () => {
    matching = ["(min-width: 900px)"];
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("two-col");
  });

  it("is three-col when both breakpoints match", () => {
    matching = ["(min-width: 900px)", "(min-width: 1300px)"];
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("three-col");
  });

  it("updates when a query changes", () => {
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("tabs");

    act(() => {
      matching = ["(min-width: 900px)"];
      for (const set of listeners.values()) for (const fn of set) fn();
    });

    expect(result.current).toBe("two-col");
  });

  it("falls back to tabs when matchMedia is unavailable", () => {
    // Some jsdom configurations have no matchMedia. Throwing here would take down the
    // whole app shell, so the narrowest layout is the safe default.
    // @ts-expect-error deliberately removing the API
    window.matchMedia = undefined;
    const { result } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("tabs");
  });
});
