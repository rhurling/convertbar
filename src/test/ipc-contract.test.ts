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
const rustFiles = walk(join(root, "src-tauri", "src"), [".rs"]);

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
const emittedEvents = collect(rustFiles, /\.emit(?:_to|_filter)?\s*\(\s*"([^"]+)"/g);

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
