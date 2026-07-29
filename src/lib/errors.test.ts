import { describe, it, expect } from "vitest";
import { errorText, INTERNAL_ERROR_PREFIX } from "./errors";

// The desktop head rejects with the serialized `CommandError` itself, so these objects are the
// literal values `invoke` hands a `catch` block.
const deliberate = { error: "HandBrakeCLI not found" };
const panicked = {
  error: 'task panicked: task 12 panicked with message "boom"',
  kind: "panic",
};

describe("errorText", () => {
  it("renders a failure the backend means as its message alone", () => {
    // No decoration: "HandBrakeCLI not found" is already the thing the user has to act on, and
    // dressing it up as an internal error would send them hunting for a bug that isn't there.
    expect(errorText(deliberate)).toBe("HandBrakeCLI not found");
  });

  it("marks a panic as a bug and keeps the detail", () => {
    const text = errorText(panicked);
    expect(text.startsWith(INTERNAL_ERROR_PREFIX)).toBe(true);
    // The detail is what makes the bug reportable — a prefix on its own would tell the user
    // something broke and give them nothing to send.
    expect(text).toContain('panicked with message "boom"');
  });

  it("never renders a failure body as [object Object]", () => {
    // The regression this helper exists to prevent. Every display site used to spell this
    // `String(e)`, which is correct only while the backend fails with a bare string; the moment
    // it gained a `kind` field the whole UI would have shown "[object Object]" instead of the
    // error — for ordinary failures, not just panics.
    for (const failure of [deliberate, panicked]) {
      expect(errorText(failure)).not.toContain("[object Object]");
    }
  });

  it("reads the server head's Error the same way", () => {
    // The HTTP transport throws a real Error and hangs `kind` off it, so one helper covers both
    // heads and a panic reads identically wherever the UI is running.
    const bug = Object.assign(new Error("task panicked: task 3 panicked"), {
      kind: "panic",
    });
    expect(errorText(bug)).toBe(`${INTERNAL_ERROR_PREFIX}task panicked: task 3 panicked`);
    expect(errorText(new Error("unauthorized"))).toBe("unauthorized");
  });

  it("falls back to stringifying anything that is not a failure body", () => {
    // A rejection can come from outside the transport entirely (a thrown string, a null from a
    // library); the UI still has to show something rather than crash in the catch block.
    expect(errorText("plain string")).toBe("plain string");
    expect(errorText(null)).toBe("null");
    expect(errorText(undefined)).toBe("undefined");
  });

  it("treats only the exact panic discriminator as a bug", () => {
    // Guards against a truthiness check creeping in: an unknown kind is not something the UI
    // may relabel as an internal error.
    expect(errorText({ error: "nope", kind: "validation" })).toBe("nope");
  });
});
