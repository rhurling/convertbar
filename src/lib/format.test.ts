import { describe, it, expect } from "vitest";
import { formatBytes, formatEta, formatPercent, fileName, durationSeconds, formatDuration } from "./format";

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

describe("durationSeconds", () => {
  it("parses the backend's own timestamp format, not a hand-typed one", () => {
    // chrono's to_rfc3339() emits up to nine fractional digits and a +00:00 offset.
    // Nothing else in src/ parses timestamps, so this assumption is otherwise unproven.
    expect(
      durationSeconds(
        "2026-08-01T10:00:00.123456789+00:00",
        "2026-08-01T10:12:34.123456789+00:00",
      ),
    ).toBeCloseTo(754, 3);
  });

  it("returns null when either stamp is missing", () => {
    expect(durationSeconds(null, "2026-08-01T10:00:00+00:00")).toBeNull();
    expect(durationSeconds("2026-08-01T10:00:00+00:00", null)).toBeNull();
  });

  it("returns null for an unparseable stamp rather than NaN", () => {
    expect(durationSeconds("not a date", "2026-08-01T10:00:00+00:00")).toBeNull();
  });

  it("returns null for a non-positive delta, so a clock jump shows nothing", () => {
    // An NTP correction between the two stamps must not render a negative duration.
    expect(
      durationSeconds("2026-08-01T10:05:00+00:00", "2026-08-01T10:00:00+00:00"),
    ).toBeNull();
    expect(
      durationSeconds("2026-08-01T10:00:00+00:00", "2026-08-01T10:00:00+00:00"),
    ).toBeNull();
  });
});

describe("formatDuration", () => {
  it.each([
    [0.3, "<1s"],
    [0.6, "1s"], // rounds up — Math.floor would say "<1s"
    [1, "1s"],
    [59, "59s"],
    [59.6, "1m 00s"], // rounds across the minute boundary — Math.floor would say "59s"
    [60, "1m 00s"],
    [754, "12m 34s"],
    [3599, "59m 59s"],
    [3600, "1h 00m"],
    [90000, "25h 00m"],
  ])("formats %ss as %s", (seconds, expected) => {
    expect(formatDuration(seconds)).toBe(expected);
  });

  it("never renders a sub-second encode as 0s", () => {
    // An instantly-failing encode is real and must stay distinguishable from "no data",
    // which renders nothing at all.
    expect(formatDuration(0.2)).toBe("<1s");
  });
});
