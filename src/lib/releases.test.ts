import { describe, it, expect } from "vitest";
import { RELEASES_URL, releaseTagUrl } from "./releases";

describe("releaseTagUrl", () => {
  it("builds the v-prefixed tag URL the release workflow actually publishes", () => {
    // build.yml tags releases `v__VERSION__`, while the updater reports the bare version from
    // latest.json — a link built from the raw string would 404 on every single update.
    expect(releaseTagUrl("2.3.0")).toBe(`${RELEASES_URL}/tag/v2.3.0`);
  });

  it("does not double the v if the caller already passed a tag-shaped version", () => {
    expect(releaseTagUrl("v2.3.0")).toBe(`${RELEASES_URL}/tag/v2.3.0`);
  });
});
