// Build-time flag for UI presence (which controls/sections render). Runtime data (version,
// can_pause_process, ...) always comes from `commands.getAppInfo()` regardless of head — see
// the plan's gating rule: BUILD-TIME `isServerHead` for UI presence, RUNTIME `getAppInfo()`
// for data.
export const isServerHead = import.meta.env.VITE_HEAD === "server";
