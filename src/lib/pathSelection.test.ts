import { describe, it, expect } from "vitest";
import { rangeBetween } from "./pathSelection";

const listing = [
  { path: "/m/2024" },
  { path: "/m/archive" },
  { path: "/m/a.mkv" },
  { path: "/m/b.mp4" },
  { path: "/m/c.mp4" },
];

describe("rangeBetween", () => {
  it("returns the inclusive slice between two paths", () => {
    expect(rangeBetween(listing, "/m/archive", "/m/b.mp4")).toEqual([
      "/m/archive",
      "/m/a.mkv",
      "/m/b.mp4",
    ]);
  });

  it("is order-agnostic — a shift-click above the anchor works the same", () => {
    expect(rangeBetween(listing, "/m/b.mp4", "/m/archive")).toEqual([
      "/m/archive",
      "/m/a.mkv",
      "/m/b.mp4",
    ]);
  });

  it("spans folders and files alike", () => {
    // The whole reason the row model puts a checkbox on every row: a range must not
    // stop at the folder/file boundary.
    expect(rangeBetween(listing, "/m/2024", "/m/a.mkv")).toEqual([
      "/m/2024",
      "/m/archive",
      "/m/a.mkv",
    ]);
  });

  it("returns just the row when anchor and target are the same", () => {
    expect(rangeBetween(listing, "/m/b.mp4", "/m/b.mp4")).toEqual(["/m/b.mp4"]);
  });

  it("returns nothing when either end is not in the listing", () => {
    // The anchor is cleared on navigation, so a stale anchor from a previous
    // directory must degrade to "no range", never to a wrong range.
    expect(rangeBetween(listing, "/other/x.mp4", "/m/b.mp4")).toEqual([]);
    expect(rangeBetween(listing, "/m/b.mp4", "/other/x.mp4")).toEqual([]);
  });
});
