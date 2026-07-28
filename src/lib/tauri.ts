export * from "./transport/types";
import { tauriCommands } from "./transport/tauri";
import { httpCommands } from "./transport/http";

// Synchronous form (import both, select by ternary) rather than a top-level `await import(...)`:
// tree-shaking of the unused branch is a nice-to-have, not a requirement here.
export const commands = import.meta.env.VITE_HEAD === "server" ? httpCommands : tauriCommands;
