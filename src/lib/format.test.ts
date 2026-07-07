import { describe, it, expect } from "vitest";
import { formatBytes, formatEta, formatPercent, fileName } from "./format";

describe("formatBytes", () => {
  it("returns '0 B' for zero (guards Math.log(0))", () => {
    expect(formatBytes(0)).toBe("0 B");
  });
  it("formats KB / MB / GB to one decimal", () => {
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(1048576)).toBe("1.0 MB");
    expect(formatBytes(1073741824)).toBe("1.0 GB");
  });
});

describe("formatEta", () => {
  it("uses m+s under an hour, zero-padding seconds", () => {
    expect(formatEta(150)).toBe("2m30s");
    expect(formatEta(59)).toBe("0m59s");
  });
  it("uses h+m at/over an hour, zero-padding minutes", () => {
    expect(formatEta(3661)).toBe("1h01m");
  });
});

describe("formatPercent", () => {
  it("guards division by zero", () => {
    expect(formatPercent(5, 0)).toBe("0%");
  });
  it("rounds to whole percent", () => {
    expect(formatPercent(50, 100)).toBe("50%");
  });
});

describe("fileName", () => {
  it("returns the last path segment", () => {
    expect(fileName("/Users/me/Movies/clip.mp4")).toBe("clip.mp4");
  });
  it("handles Windows backslash paths — the backend sends OS-native separators", () => {
    // Regression: splitting on "/" only made every queue/history row on Windows
    // show the full path instead of the file name.
    expect(fileName("C:\\Users\\me\\Videos\\clip.mp4")).toBe("clip.mp4");
  });
  it("ignores a trailing separator", () => {
    expect(fileName("/Users/me/Movies/")).toBe("Movies");
  });
  it("returns the input unchanged when there is no separator", () => {
    expect(fileName("clip.mp4")).toBe("clip.mp4");
  });
});
