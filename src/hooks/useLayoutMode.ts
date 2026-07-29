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
 * The layout decision, as state rather than CSS: which pages *mount* changes with width,
 * and CSS cannot mount an unmounted component.
 */
export function useLayoutMode(): LayoutMode {
  const [mode, setMode] = useState<LayoutMode>(currentMode);

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const update = () => setMode(currentMode());
    const queries = [window.matchMedia(WIDE), window.matchMedia(WIDER)];
    for (const q of queries) q.addEventListener("change", update);
    update();
    return () => {
      for (const q of queries) q.removeEventListener("change", update);
    };
  }, []);

  return mode;
}
