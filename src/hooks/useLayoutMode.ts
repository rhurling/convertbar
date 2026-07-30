import { useEffect, useState } from "react";

/** Which panels are pinned into their own column. `tabs` is the menu-bar popover layout. */
export type LayoutMode = "tabs" | "two-col" | "three-col";

const WIDE = "(min-width: 900px)";
const WIDER = "(min-width: 1300px)";

function currentMode(): LayoutMode {
  // matchMedia is absent in some jsdom configurations; the narrowest layout is the safe
  // fallback, and it is also what the desktop head always resolves to (fixed 400x500).
  if (typeof window.matchMedia !== "function") return "tabs";
  if (window.matchMedia(WIDER).matches) return "three-col";
  if (window.matchMedia(WIDE).matches) return "two-col";
  return "tabs";
}

/**
 * Subscribes `update` to `query`'s change event, preferring the standard EventTarget API and
 * falling back to the deprecated `addListener`/`removeListener` pair on hosts where
 * `MediaQueryList` isn't an `EventTarget` yet (Safari < 14 — pre-Chromium WebKit, which the
 * desktop head's system WebView can still be since `tauri.conf.json` sets no
 * `minimumSystemVersion`). Returns the matching unsubscribe function so callers never mix the
 * two APIs.
 */
function subscribe(query: MediaQueryList, update: () => void): () => void {
  if (typeof query.addEventListener === "function") {
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }
  query.addListener(update);
  return () => query.removeListener(update);
}

/**
 * The layout decision, as state rather than CSS: which pages *mount* changes with width,
 * and CSS cannot mount an unmounted component.
 */
export function useLayoutMode(): LayoutMode {
  const [mode, setMode] = useState<LayoutMode>(currentMode);

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const update = () => setMode(currentMode());
    const queries = [window.matchMedia(WIDE), window.matchMedia(WIDER)];
    const unsubscribers = queries.map((q) => subscribe(q, update));
    update();
    return () => {
      for (const unsubscribe of unsubscribers) unsubscribe();
    };
  }, []);

  return mode;
}
