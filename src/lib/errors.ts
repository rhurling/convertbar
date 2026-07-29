// The single place a backend failure becomes text for a human.
//
// Both heads fail with the same shape — desktop's `CommandError`
// (src-tauri/src/commands/error.rs) and the server's error body
// (crates/convertbar-server/src/routes/mod.rs) each serialize to `{error, kind?}` — so one
// helper reads both. `kind` is the whole discriminator: it is absent for a failure the backend
// means (HandBrakeCLI missing, an id that matches no row) and `"panic"` when a blocking task
// died, which is a bug in ConvertBar rather than anything the user can act on.
//
// This replaced a bare `String(e)` at every display site, which was wrong twice over: it cannot
// see `kind`, and on desktop `invoke` rejects with the raw object, so it renders
// "[object Object]". `no_display_site_stringifies_a_caught_error` keeps it replaced.

const PANIC = "panic";

// Says plainly that the app broke, so a user does not read a Rust panic string as something
// they misconfigured. The detail follows it rather than being swallowed — it is the only thing
// that makes the bug reportable.
// Deliberately not exported: a test that interpolates this constant asserts nothing about its
// content and stays green if it is emptied, which would silently delete the only thing that
// separates a bug from an ordinary failure on screen. Tests pin the rendered literal instead.
const INTERNAL_ERROR_PREFIX = "Internal error (this is a bug): ";

// A rejection can come from outside either transport (a library throwing a bare object, a null).
// Those carry nothing worth showing, and "[object Object]" or "null" is worse than admitting we
// do not know — the raw value is still in the console at every site that logs one.
const UNKNOWN = "Unknown error";

const asRecord = (e: unknown): Record<string, unknown> | null =>
  typeof e === "object" && e !== null ? (e as Record<string, unknown>) : null;

/// True only for a panicked backend task. Callers that show fixed copy for an expected failure
/// use this to avoid explaining a bug as something the user can fix.
export function isPanic(e: unknown): boolean {
  return asRecord(e)?.kind === PANIC;
}

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;

  const record = asRecord(e);
  const message =
    // Desktop rejects with the failure body itself; the server transport wraps it in an Error.
    typeof record?.error === "string"
      ? record.error
      : typeof record?.message === "string"
        ? record.message
        : UNKNOWN;

  return isPanic(e) ? `${INTERNAL_ERROR_PREFIX}${message}` : message;
}
