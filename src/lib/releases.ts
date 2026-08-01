// GitHub has no way to filter the releases page to "newer than the version I'm running" — its
// release search only understands `prerelease:`/`draft:` and tag text, not a version range. So
// the desktop head doesn't try: it only offers a link when its own update check has already
// found something newer, and links straight at that release. The server head has no updater at
// all, so it falls back to the unfiltered index.
const REPO_URL = "https://github.com/rhurling/convertbar";

export const RELEASES_URL = `${REPO_URL}/releases`;

// Release tags are `v__VERSION__` (build.yml), while the updater reports the bare version from
// latest.json ("2.3.0"). The strip keeps the URL right if either side ever hands over a
// v-prefixed string.
export function releaseTagUrl(version: string): string {
  return `${RELEASES_URL}/tag/v${version.replace(/^v/, "")}`;
}
