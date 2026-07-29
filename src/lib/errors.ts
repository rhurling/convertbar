// The single place a backend failure becomes text for a human.
//
// Both heads fail with the same shape — desktop's `CommandError`
// (src-tauri/src/commands/mod.rs) and the server's error body
// (crates/convertbar-server/src/routes/mod.rs) each serialize to `{error, kind?}` — so one
// helper reads both. `kind` is the whole discriminator: it is absent for a failure the backend
// means (HandBrakeCLI missing, an id that matches no row) and `"panic"` when a blocking task
// died, which is a bug in ConvertBar rather than anything the user can act on.
//
// This replaced a bare `String(e)` at every display site, which was wrong twice over: it cannot
// see `kind`, and on desktop `invoke` rejects with the raw object, so it renders
// "[object Object]".

const PANIC = "panic";

// Says plainly that the app broke, so a user does not read a Rust panic string as something
// they misconfigured. The detail follows it rather than being swallowed — it is the only thing
// that makes the bug reportable.
export const INTERNAL_ERROR_PREFIX = "Internal error (this is a bug): ";

const asRecord = (e: unknown): Record<string, unknown> | null =>
  typeof e === "object" && e !== null ? (e as Record<string, unknown>) : null;

export function errorText(e: unknown): string {
  const record = asRecord(e);
  const message =
    // Desktop rejects with the failure body itself; the server transport wraps it in an Error.
    typeof record?.error === "string"
      ? record.error
      : typeof record?.message === "string"
        ? record.message
        : String(e);

  return record?.kind === PANIC ? `${INTERNAL_ERROR_PREFIX}${message}` : message;
}
