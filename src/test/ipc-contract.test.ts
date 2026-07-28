import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

// The frontend/backend IPC contract is otherwise only asserted against strings the unit
// tests themselves define — renaming a Rust `#[tauri::command]` fn or an emitted event
// name breaks the running app while every unit suite stays green. This test pins the seam:
// every command the frontend invokes must be a registered command, and every event it
// listens for must be emitted somewhere in the backend.

const root = process.cwd();

function walk(dir: string, exts: string[], out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "target") continue;
      walk(path, exts, out);
    } else if (exts.some((e) => entry.name.endsWith(e))) {
      out.push(path);
    }
  }
  return out;
}

function collect(files: string[], re: RegExp): Set<string> {
  const found = new Set<string>();
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    for (const m of src.matchAll(re)) found.add(m[1]);
  }
  return found;
}

// Frontend sources, excluding test files (their mock dispatchers reference command names
// as bare switch-case strings, not via invoke()/listen()).
const frontendFiles = walk(join(root, "src"), [".ts", ".tsx"]).filter(
  (f) => !/\.(test|spec)\.tsx?$/.test(f) && !f.includes(join("src", "test")),
);
const rustFiles = [
  ...walk(join(root, "src-tauri", "src"), [".rs"]),
  ...walk(join(root, "crates", "convertbar-core", "src"), [".rs"]),
];

const invokedCommands = collect(
  frontendFiles,
  /\binvoke(?:<[^>]*>)?\s*\(\s*["'`]([^"'`]+)["'`]/g,
);
const listenedEvents = collect(
  frontendFiles,
  /\blisten(?:<[^>]*>)?\s*\(\s*["'`]([^"'`]+)["'`]/g,
);

// A registered command's name is its fn name (this codebase uses no command rename attr).
// #[tauri::command] may sit under other attributes (e.g. cfg_attr), so scan forward to the fn.
function collectCommandNames(files: string[]): Set<string> {
  const names = new Set<string>();
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    for (const m of src.matchAll(/#\[tauri::command\]/g)) {
      const fn = src.slice(m.index).match(/\bfn\s+(\w+)\s*[(<]/);
      if (fn) names.add(fn[1]);
    }
  }
  return names;
}

const registeredCommands = collectCommandNames(rustFiles);
const emittedEvents = collect(rustFiles, /\.emit(?:_t|_to|_filter)?\s*\(\s*"([^"]+)"/g);

describe("IPC contract", () => {
  it("extracts a non-empty surface from both sides (guards against a broken scan)", () => {
    expect(invokedCommands.size).toBeGreaterThan(10);
    expect(listenedEvents.size).toBeGreaterThan(0);
    expect(registeredCommands.size).toBeGreaterThan(10);
    expect(emittedEvents.size).toBeGreaterThan(0);
  });

  it("every invoked command is a registered #[tauri::command]", () => {
    const missing = [...invokedCommands].filter((c) => !registeredCommands.has(c));
    expect(missing, `invoked but not registered in src-tauri: ${missing.join(", ")}`).toEqual([]);
  });

  it("every listened event is emitted by the backend", () => {
    const missing = [...listenedEvents].filter((e) => !emittedEvents.has(e));
    expect(missing, `listened but never emitted in src-tauri: ${missing.join(", ")}`).toEqual([]);
  });
});

// --- Server-head sibling: HTTP transport <-> routes.json contract (Task 13) ----------------
//
// `src/lib/transport/http.ts` talks to the axum routes in `crates/convertbar-server/routes.json`
// by string literal (method + path), not by symbol — renaming a route on either side breaks the
// server head at runtime while every other suite, including the desktop contract above, stays
// green. This pins that seam: every fetch call must hit a route that's actually registered, and
// every registered command must have a frontend caller.

interface RouteRow {
  command: string;
  method: string;
  path: string;
}

const routesJson: RouteRow[] = JSON.parse(
  readFileSync(join(root, "crates", "convertbar-server", "routes.json"), "utf8"),
);

const httpTsPath = join(root, "src", "lib", "transport", "http.ts");
const httpTsSrc = readFileSync(httpTsPath, "utf8");

/**
 * Extracts every `api("METHOD", <path-expression>)` call in `http.ts`, returning the method and
 * the raw (unquoted) text of the path argument — a plain string, or the full body of a template
 * literal, backticks stripped, including any *nested* backticks (`getHistorySummary`'s path is a
 * template literal containing another template literal inside a ternary). A hand-rolled scan
 * rather than a single regex, because that nesting defeats a `` `([^`]*)` `` capture — it would
 * stop at the first inner backtick.
 */
function extractHttpApiCalls(src: string): { method: string; rawPath: string }[] {
  const calls: { method: string; rawPath: string }[] = [];
  const callRe = /\bapi\(\s*["'](GET|POST|PUT|DELETE|PATCH)["'],\s*/g;
  let m: RegExpExecArray | null;
  while ((m = callRe.exec(src))) {
    const method = m[1];
    const start = callRe.lastIndex;
    const quote = src[start];
    if (quote !== "`" && quote !== '"' && quote !== "'") continue;
    let i = start + 1;
    if (quote === "`") {
      // Track `${`/`{`/`}` depth so a nested template literal's own backtick doesn't look
      // like the close of the outer one.
      let depth = 0;
      while (i < src.length) {
        if (src[i] === "\\") {
          i += 2;
          continue;
        }
        if (depth === 0 && src[i] === "`") break;
        if (src[i] === "$" && src[i + 1] === "{") {
          depth++;
          i += 2;
          continue;
        }
        if (depth > 0 && src[i] === "{") {
          depth++;
          i++;
          continue;
        }
        if (depth > 0 && src[i] === "}") {
          depth--;
          i++;
          continue;
        }
        i++;
      }
    } else {
      while (i < src.length && src[i] !== quote) {
        if (src[i] === "\\") i++;
        i++;
      }
    }
    calls.push({ method, rawPath: src.slice(start + 1, i) });
  }
  return calls;
}

/**
 * True if a `${...}` group's source builds an optional query string rather than a path segment —
 * i.e. some backtick-quoted section inside it (with that section's own `${...}` expressions
 * blanked out first) contains a literal `?`. Catches `getHistorySummary`'s
 * `${search ? \`?search=${...}\` : ""}`, where the query-string `?` sits one template layer
 * below the ternary's own `?`.
 */
function nestedGroupBuildsQueryString(groupSrc: string): boolean {
  let i = 0;
  while (i < groupSrc.length) {
    const start = groupSrc.indexOf("`", i);
    if (start === -1) return false;
    const end = groupSrc.indexOf("`", start + 1);
    if (end === -1) return false;
    const inner = groupSrc.slice(start + 1, end).replace(/\$\{[^}]*\}/g, "");
    if (inner.includes("?")) return true;
    i = end + 1;
  }
  return false;
}

/**
 * Normalizes a raw path expression (from `extractHttpApiCalls`) to a routes.json-comparable
 * shape: every `${...}` interpolation becomes the single token `{}` (routes.json's `{id}` etc.
 * normalize the same way in `normalizeRoutesPath`), and anything from a query string onward is
 * dropped entirely — including one assembled conditionally, several `${}` layers deep. That's
 * why this walks the template structurally instead of doing two independent
 * string.replace passes: a plain "strip from the first `?`, then replace `${...}`" can't tell
 * the `?` in `search ? ... : ""` (a ternary, not a query string) from the `?` that starts
 * `?search=` one level further inside the same expression.
 */
function normalizeHttpPath(rawPath: string): string {
  let result = "";
  let i = 0;
  while (i < rawPath.length) {
    const nextGroup = rawPath.indexOf("${", i);
    const staticPart = nextGroup === -1 ? rawPath.slice(i) : rawPath.slice(i, nextGroup);
    const qIdx = staticPart.indexOf("?");
    if (qIdx !== -1) return result + staticPart.slice(0, qIdx);
    result += staticPart;
    if (nextGroup === -1) break;
    let depth = 1;
    let j = nextGroup + 2;
    while (j < rawPath.length && depth > 0) {
      if (rawPath[j] === "{") depth++;
      else if (rawPath[j] === "}") depth--;
      j++;
    }
    const groupSrc = rawPath.slice(nextGroup + 2, j - 1);
    if (groupSrc.includes("`") && nestedGroupBuildsQueryString(groupSrc)) return result;
    result += "{}";
    i = j;
  }
  return result;
}

function normalizeRoutesPath(path: string): string {
  const qIdx = path.indexOf("?");
  return (qIdx === -1 ? path : path.slice(0, qIdx)).replace(/\{[^}]*\}/g, "{}");
}

const httpCalls = extractHttpApiCalls(httpTsSrc).map((c) => ({
  method: c.method,
  path: normalizeHttpPath(c.rawPath),
}));
const normalizedRoutes = routesJson.map((r) => ({ ...r, path: normalizeRoutesPath(r.path) }));

// `httpCommands`'s top-level keys, including the desktop-only `notAvailable` stubs (fine: this
// set is only ever checked for routes.json COVERAGE — routes.json's commands must be a subset of
// these names, never the reverse).
function httpCommandsKeys(src: string): string[] {
  const start = src.indexOf("export const httpCommands = {");
  const end = src.indexOf("\n} satisfies", start);
  const body = src.slice(start, end);
  return [...body.matchAll(/^ {2}(\w+):/gm)].map((m) => m[1]);
}

const normalizeName = (s: string): string => s.replace(/_/g, "").toLowerCase();
const httpKeyNames = new Set(httpCommandsKeys(httpTsSrc).map(normalizeName));

describe("http transport <-> routes.json contract", () => {
  it("extracts a non-empty surface from both sides (guards against a broken scan)", () => {
    expect(httpCalls.length).toBeGreaterThan(10);
    expect(routesJson.length).toBeGreaterThan(10);
    expect(httpKeyNames.size).toBeGreaterThan(10);
  });

  it("every http.ts api() call matches a routes.json row (method + normalized path)", () => {
    const unmatched = httpCalls.filter(
      (c) => !normalizedRoutes.some((r) => r.method === c.method && r.path === c.path),
    );
    expect(unmatched, `http.ts calls with no routes.json row: ${JSON.stringify(unmatched)}`).toEqual(
      [],
    );
  });

  it("every routes.json command has a matching httpCommands member (routes.json only lists commands with a server route, so none are desktop-only)", () => {
    const missing = routesJson.filter((r) => !httpKeyNames.has(normalizeName(r.command)));
    expect(
      missing,
      `routes.json commands missing from http.ts: ${missing.map((r) => r.command).join(", ")}`,
    ).toEqual([]);
  });

  it("the SSE stream route is registered (transport, not a routes.json command)", () => {
    const eventsRs = readFileSync(
      join(root, "crates", "convertbar-server", "src", "routes", "events.rs"),
      "utf8",
    );
    expect(eventsRs.includes('"/api/events"')).toBe(true);
  });
});
