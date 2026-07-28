import { describe, it, expect } from "vitest";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const SCRIPT = resolve(__dirname, "../../scripts/release-notes.sh");

function repoWithCommits(subjects: string[], tagAt: number): string {
  const dir = mkdtempSync(join(tmpdir(), "notes-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, { cwd: dir, encoding: "utf8" });

  git("init", "-q", "-b", "main");
  git("config", "user.email", "t@example.com");
  git("config", "user.name", "T");
  git("config", "commit.gpgsign", "false");
  git("config", "tag.gpgsign", "false");

  git("commit", "-q", "--allow-empty", "-m", "chore: base");
  if (tagAt === 0) git("tag", "v0.1.0");

  subjects.forEach((s, i) => {
    git("commit", "-q", "--allow-empty", "-m", s);
    if (tagAt === i + 1) git("tag", "v0.1.0");
  });
  git("tag", "v0.2.0");
  return dir;
}

function run(dir: string, prev: string, current: string): string {
  return execFileSync("bash", [SCRIPT, prev, current], { cwd: dir, encoding: "utf8" });
}

describe("release-notes.sh", () => {
  it("groups feat, fix and perf under headings and names each change", () => {
    // These are what a user actually wants to read before deciding to install.
    const dir = repoWithCommits(
      ["feat: add dark mode", "fix: stop crash on empty queue", "perf: cache probes"],
      0,
    );
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out).toContain("### Features");
      expect(out).toContain("add dark mode");
      expect(out).toContain("### Fixes");
      expect(out).toContain("stop crash on empty queue");
      expect(out).toContain("### Performance");
      expect(out).toContain("cache probes");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("collapses maintenance commits to a count instead of listing them", () => {
    // Without the collapse, dependabot subjects dominate every release body and bury the
    // handful of changes a user cares about.
    const dir = repoWithCommits(
      [
        "feat: add dark mode",
        "chore(deps): bump react",
        "chore(deps): bump vite",
        "docs: tweak readme",
        "refactor: tidy converter",
      ],
      0,
    );
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out).toMatch(/4 maintenance (change|commit)/i);
      expect(out).not.toContain("bump react");
      expect(out).not.toContain("tweak readme");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("always appends the full-changelog compare link", () => {
    const dir = repoWithCommits(["feat: add dark mode"], 0);
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out).toContain(
        "**Full changelog**: https://github.com/rhurling/convertbar/compare/v0.1.0...v0.2.0",
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("prints Initial release. when there is no previous tag", () => {
    // Preserves the existing build.yml behaviour for the very first release.
    const dir = repoWithCommits(["feat: first"], -1);
    try {
      const out = run(dir, "", "v0.2.0");
      expect(out.trim()).toBe("Initial release.");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("still emits the compare link when the range is empty", () => {
    // A retagged or empty range must not produce a blank release body.
    const dir = repoWithCommits([], 0);
    try {
      const out = run(dir, "v0.1.0", "v0.1.0");
      expect(out).toContain("**Full changelog**");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("emits no bare line that GitHub Actions would reject as an output", () => {
    // $GITHUB_OUTPUT parses `key=value`; a heredoc protects multi-line values, but a line
    // equal to the delimiter would terminate it early. A commit subject that happens to
    // collide with the delimiter after prefix-stripping must render as a bulleted line
    // ("- CONVERTBAR_NOTES_EOF"), never as a bare one — otherwise this would silently
    // truncate the release body in CI and nothing in PR CI would catch it.
    const dir = repoWithCommits(["feat: add dark mode", "fix: CONVERTBAR_NOTES_EOF"], 0);
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out.split("\n")).not.toContain("CONVERTBAR_NOTES_EOF");
      expect(out).toContain("- CONVERTBAR_NOTES_EOF");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("pluralizes the maintenance count correctly for exactly one commit", () => {
    // This line is read by every user in the update panel before deciding to install —
    // "1 maintenance changes" is an agreement error worth its own regression test.
    const dir = repoWithCommits(["feat: add dark mode", "chore(deps): bump react"], 0);
    try {
      const out = run(dir, "v0.1.0", "v0.2.0");
      expect(out).toContain("1 maintenance change (");
      expect(out).not.toContain("1 maintenance changes");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
