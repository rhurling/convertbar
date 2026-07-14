import { describe, it, expect } from "vitest";

import { resolveTargetPath } from "./historyTarget";
import type { JobInfo, PathsExist } from "./tauri";

function job(overrides: Partial<JobInfo>): JobInfo {
  return {
    id: "1",
    source_path: "/m/clip.mp4",
    output_path: "/m/clip-conv.mp4",
    preset: "p",
    status: "done",
    original_size: 1000,
    converted_size: 500,
    kept_file: "converted",
    space_saved: 500,
    error_message: null,
    queue_order: 0,
    created_at: "",
    completed_at: null,
    ...overrides,
  };
}

function exists(source_exists: boolean, output_exists: boolean): PathsExist {
  return { source_exists, output_exists };
}

describe("resolveTargetPath", () => {
  it("uses the single path for in-place conversions", () => {
    const j = job({ source_path: "/m/a.mp4", output_path: "/m/a.mp4" });
    expect(resolveTargetPath(j, exists(true, true))).toBe("/m/a.mp4");
    expect(resolveTargetPath(j, exists(false, false))).toBeNull();
  });

  it("prefers the output when the converted file was kept", () => {
    expect(resolveTargetPath(job({ kept_file: "converted" }), exists(false, true))).toBe(
      "/m/clip-conv.mp4",
    );
    // Even with a stale source still on disk, the converted file wins.
    expect(resolveTargetPath(job({ kept_file: "converted" }), exists(true, true))).toBe(
      "/m/clip-conv.mp4",
    );
  });

  it("falls back to the source when the kept output has since been moved", () => {
    expect(resolveTargetPath(job({ kept_file: "converted" }), exists(true, false))).toBe(
      "/m/clip.mp4",
    );
  });

  it("prefers the source when the original was kept", () => {
    expect(resolveTargetPath(job({ kept_file: "original" }), exists(true, false))).toBe(
      "/m/clip.mp4",
    );
  });

  it("prefers the source for skipped jobs", () => {
    const j = job({ status: "skipped", kept_file: null });
    expect(resolveTargetPath(j, exists(true, false))).toBe("/m/clip.mp4");
  });

  it("prefers the source for errored jobs but falls back to a leftover output", () => {
    const j = job({ status: "error", kept_file: null });
    expect(resolveTargetPath(j, exists(true, false))).toBe("/m/clip.mp4");
    expect(resolveTargetPath(j, exists(false, true))).toBe("/m/clip-conv.mp4");
  });

  it("returns null when neither file exists", () => {
    expect(resolveTargetPath(job({}), exists(false, false))).toBeNull();
  });
});
