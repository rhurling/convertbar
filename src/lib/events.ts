// Single seam for backend events. Desktop: Tauri's event system. The server build
// (Plan 2) replaces the internals with one shared EventSource; consumers never change.
import { listen as tauriListen } from "@tauri-apps/api/event";

// Preserves the Tauri callback shape `{ payload }` both heads deliver to `listen()` handlers.
// Named `ListenEvent` (not `Event`) so it never shadows the DOM `Event` constructor used below
// for `convertbar:events-reconnected`/`convertbar:unauthorized`.
export interface ListenEvent<T> {
  payload: T;
}
export type UnlistenFn = () => void;

type Listen = <T>(event: string, handler: (event: ListenEvent<T>) => void) => Promise<UnlistenFn>;

const desktopListen: Listen = (event, handler) => tauriListen(event, handler);

// One shared connection for the whole app; every `listen()` call attaches to it. `null` on
// desktop, where this branch is never exercised.
const source: EventSource | null =
  import.meta.env.VITE_HEAD === "server" ? new EventSource("/api/events") : null;

// Tracks whether the connection has dropped since the last successful open, so `onopen` only
// dispatches the reconnect signal after a real reconnect — not on the initial connection.
let hadError = false;
source?.addEventListener("error", () => {
  hadError = true;
});
source?.addEventListener("open", () => {
  if (hadError) {
    hadError = false;
    window.dispatchEvent(new Event("convertbar:events-reconnected"));
  }
});

const serverListen: Listen = (event, handler) => {
  const wrapped = (e: MessageEvent) => handler({ payload: JSON.parse(e.data) });
  source!.addEventListener(event, wrapped);
  return Promise.resolve(() => source!.removeEventListener(event, wrapped));
};

export const listen: Listen =
  import.meta.env.VITE_HEAD === "server" ? serverListen : desktopListen;
