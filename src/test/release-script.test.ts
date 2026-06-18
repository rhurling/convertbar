import { describe, it, expect } from "vitest";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

function git(args: string[]): string {
  return execFileSync("git", args, { encoding: "utf8" }).trim();
}

function runScript(args: string[]): { status: number; stdout: string; stderr: string } {
  try {
    const stdout = execFileSync("bash", ["scripts/release.sh", ...args], { encoding: "utf8" });
    return { status: 0, stdout, stderr: "" };
  } catch (e: any) {
    return {
      status: e.status ?? 1,
      stdout: e.stdout?.toString() ?? "",
      stderr: e.stderr?.toString() ?? "",
    };
  }
}

const currentVersion = (): string =>
  JSON.parse(readFileSync("package.json", "utf8")).version;

function bumpMinor(v: string): string {
  const [maj, min] = v.split(".").map(Number);
  return `${maj}.${min + 1}.0`;
}

describe("release.sh", () => {
  it("--dry-run prints the plan for the resolved version and mutates nothing", () => {
    const target = bumpMinor(currentVersion());
    const before = git(["status", "--porcelain"]);

    const { status, stdout } = runScript(["minor", "--dry-run"]);

    expect(status).toBe(0);
    expect(stdout).toContain("DRY RUN");
    expect(stdout).toContain(`Target version: ${target}`);
    expect(stdout).toContain(`v${target}`);
    expect(git(["status", "--porcelain"])).toBe(before);
    expect(git(["branch", "--list", `chore/release-${target}`])).toBe("");
    expect(git(["tag", "--list", `v${target}`])).toBe("");
  });
});
