import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { errorText, isPanic } from "./errors";

// The desktop head rejects with the serialized `CommandError` itself, so these objects are the
// literal values `invoke` hands a `catch` block.
const deliberate = { error: "HandBrakeCLI not found" };
const panicked = { error: "task panicked: boom", kind: "panic" };

describe("errorText", () => {
  it("renders a failure the backend means as its message alone", () => {
    // No decoration: "HandBrakeCLI not found" is already the thing the user has to act on, and
    // dressing it up as an internal error would send them hunting for a bug that isn't there.
    expect(errorText(deliberate)).toBe("HandBrakeCLI not found");
  });

  it("marks a panic as a bug and keeps the detail", () => {
    // Pinned as a literal rather than by interpolating INTERNAL_ERROR_PREFIX: a test that
    // interpolates the same constant the implementation does tracks any change to it, including
    // emptying it — which would silently delete the one thing separating the two renderings.
    expect(errorText(panicked)).toBe(
      "Internal error (this is a bug): task panicked: boom",
    );
    // And the two really do render differently — the property the prefix exists for.
    expect(errorText(panicked)).not.toBe(errorText(deliberate));
  });

  it("never renders a failure body as [object Object]", () => {
    // The regression this helper exists to prevent. Every display site used to spell this
    // `String(e)`, which is correct only while the backend fails with a bare string; the moment
    // it gained a `kind` field the whole UI would have shown "[object Object]" instead of the
    // error — for ordinary failures, not just panics. The empty object is here because it is
    // the one input that reaches the fallback: `String({})` is exactly that string.
    for (const failure of [deliberate, panicked, {}, { error: 42 }]) {
      expect(errorText(failure)).not.toContain("[object Object]");
    }
  });

  it("reads the server head's Error the same way", () => {
    // The HTTP transport throws a real Error and hangs `kind` off it, so one helper covers both
    // heads and a panic reads identically wherever the UI is running.
    const bug = Object.assign(new Error("task panicked: boom"), { kind: "panic" });
    expect(errorText(bug)).toBe("Internal error (this is a bug): task panicked: boom");
    expect(errorText(new Error("unauthorized"))).toBe("unauthorized");
  });

  it("shows a thrown string but refuses to render a value carrying no message", () => {
    // A thrown string is someone's message and survives; null/undefined/a bare object are not,
    // and "null" or "[object Object]" on screen is worse than admitting we do not know. The raw
    // value still reaches the console at every site that logs one.
    expect(errorText("plain string")).toBe("plain string");
    for (const nothing of [null, undefined, {}, 42, []]) {
      expect(errorText(nothing)).toBe("Unknown error");
    }
  });

  it("treats only the exact panic discriminator as a bug", () => {
    // Guards against a truthiness check creeping in: an unknown kind is not something the UI
    // may relabel as an internal error.
    expect(errorText({ error: "nope", kind: "validation" })).toBe("nope");
    expect(isPanic({ error: "nope", kind: "validation" })).toBe(false);
    expect(isPanic(panicked)).toBe(true);
  });
});

describe("display sites", () => {
  it("no_display_site_stringifies_a_caught_error", () => {
    // The frontend twin of the Rust tripwire in src-tauri/src/commands/mod.rs, and it exists for
    // the same reason: the 13-site sweep that introduced `errorText` was applied, not enforced.
    // `String(e)` is now actively wrong rather than merely kind-blind — desktop `invoke` rejects
    // with an object, so a new one renders "[object Object]" for every failure. Nothing else
    // catches that: the unit tests above only pin `errorText` itself.
    const root = join(process.cwd(), "src");

    const walk = (dir: string, out: string[] = []): string[] => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const path = join(dir, entry.name);
        if (entry.isDirectory()) walk(path, out);
        else if (/\.tsx?$/.test(path) && !/\.(test|spec)\.tsx?$/.test(path)) out.push(path);
      }
      return out;
    };

    // Line comments go first, like the Rust twin: a comment quoting `String(e)` is prose, and
    // failing on it would train people to work around the tripwire. Collapsing the whitespace
    // inside `${ … }` closes the spaced spelling, which no formatter here would normalise —
    // there is no Prettier or ESLint in this repo.
    const codeOnly = (source: string): string =>
      source
        .split("\n")
        .map((line) => (line.includes("//") ? line.slice(0, line.indexOf("//")) : line))
        .join("\n")
        .replace(/\$\{\s*([\w$]+)\s*\}/g, "${$1}");

    // A needle that begins with an identifier character can match the tail of a longer name:
    // `e.toString()` occurs inside `fileSize.toString()`, and `String(e)` inside `toString(e)`.
    // Neither is a caught error, so those needles only count when the character before them ends
    // a token. The Rust twin guards the same way.
    //
    // `${e}` is the exception and must NOT be guarded: inside a template literal `${` opens an
    // interpolation whatever precedes it, so guarding it silently waved through `` `Error${e}` ``
    // — a real offence, caught by round three's plainer check and missed by this one until a
    // probe went looking for the difference between "before a space" and "before a letter".
    const usesToken = (source: string, needle: string): boolean => {
      const guarded = !needle.startsWith("${");
      for (let at = source.indexOf(needle); at !== -1; at = source.indexOf(needle, at + 1)) {
        if (!guarded || at === 0 || !/[\w$.]/.test(source[at - 1])) return true;
      }
      return false;
    };

    // Compared whole, not by suffix: `endsWith("lib/errors.ts")` would also exempt a future
    // `src/datalib/errors.ts`. The Rust twin avoids the same trap by not using `Path::ends_with`.
    const helper = join(root, "lib", "errors.ts");
    const files = walk(root).filter((f) => f !== helper);
    let bindingsChecked = 0;

    for (const file of files) {
      const source = codeOnly(readFileSync(file, "utf8"));
      // Derived from the file's own bindings rather than a fixed list of names: a fixed list is
      // dodged by renaming the binding, which is a rename away from `${error}` — a spelling a
      // hardcoded ["e", "err"] waves straight through. Both a `catch` block and a promise
      // `.catch(cb)` count: several files have only the latter, and "surface this error" lands
      // there just as easily.
      const bindings = new Set([
        // The lookbehind keeps `p.catch(refresh)` from reading as a catch *block* binding `refresh`
        // — which would then fail the file for interpolating its own handler's name, exactly the
        // cry-wolf this scan is supposed to avoid.
        ...[
          ...source.matchAll(/(?<![.\w$])catch\s*\(\s*([A-Za-z_$][\w$]*)\s*(?::[^)]*)?\)/g),
        ].map((m) => m[1]),
        ...[
          ...source.matchAll(
            /\.catch\s*\(\s*(?:async\s+)?\(?\s*([A-Za-z_$][\w$]*)\s*(?::[^)=]*)?\)?\s*=>/g,
          ),
        ].map((m) => m[1]),
      ]);

      for (const binding of bindings) {
        bindingsChecked++;
        // Every way to turn a value into display text. `errorText` and `isPanic` take the
        // binding directly, so they are unaffected. Known misses, none present today: string
        // concatenation (`"failed: " + e`), which cannot be told from arithmetic without
        // parsing; `catch ({ message })` destructuring; and `.catch(function (e) {…})`.
        for (const needle of [
          `String(${binding})`,
          `\${${binding}}`,
          `${binding}.toString()`,
          `JSON.stringify(${binding})`,
        ]) {
          expect(
            usesToken(source, needle),
            `${file} renders a caught error with ${needle}; use errorText(${binding}) so a ` +
              `panic stays distinguishable and an object body does not render as [object Object]`,
          ).toBe(false);
        }
      }
    }

    // Guards the walk and the binding scan: either matching nothing would pass every assertion
    // above while checking no file, or no binding, at all.
    expect(files.length).toBeGreaterThan(20);
    // 12 today. The bound is loose because it exists to catch the regexes collapsing to nothing,
    // not to track the count — and a tripwire that fails on every refactor gets deleted.
    expect(bindingsChecked).toBeGreaterThan(8);
  });
});
