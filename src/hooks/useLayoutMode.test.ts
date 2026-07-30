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

  it("subscribes and updates via the legacy addListener/removeListener API", () => {
    // MediaQueryList only became an EventTarget in Safari 14; before that only the
    // deprecated addListener/removeListener pair exists. The desktop head runs the system
    // WebView with no minimumSystemVersion pin, so a host without addEventListener is
    // reachable in practice. Calling addEventListener unconditionally throws a TypeError
    // from this passive effect during mount, and with no error boundary React unmounts the
    // whole root — a permanently blank popover. This double exposes ONLY the legacy API to
    // prove the hook falls back instead of throwing.
    const legacyListeners = new Map<string, Set<Listener>>();
    let legacyMatching: string[] = [];
    window.matchMedia = ((query: string) => ({
      matches: legacyMatching.includes(query),
      media: query,
      addListener: (fn: Listener) => {
        if (!legacyListeners.has(query)) legacyListeners.set(query, new Set());
        legacyListeners.get(query)!.add(fn);
      },
      removeListener: (fn: Listener) => legacyListeners.get(query)?.delete(fn),
      // Deliberately no addEventListener/removeEventListener.
    })) as unknown as typeof window.matchMedia;

    const { result, unmount } = renderHook(() => useLayoutMode());
    expect(result.current).toBe("tabs");

    act(() => {
      legacyMatching = ["(min-width: 900px)"];
      for (const set of legacyListeners.values()) for (const fn of set) fn();
    });
    expect(result.current).toBe("two-col");

    const subscribedCount = [...legacyListeners.values()].reduce((n, s) => n + s.size, 0);
    expect(subscribedCount).toBeGreaterThan(0);
    unmount();
    const remainingCount = [...legacyListeners.values()].reduce((n, s) => n + s.size, 0);
    expect(remainingCount).toBe(0);
  });
});
