/**
 * The inclusive slice of `entries` between two paths, in listing order. Order-agnostic:
 * the anchor may sit above or below the target. Returns `[]` when either endpoint is
 * absent, so a stale anchor left over from another directory degrades to "no range"
 * rather than to a wrong one.
 *
 * Compares paths as opaque strings, so it is separator-agnostic and holds if a server
 * head ever runs on Windows.
 */
export function rangeBetween(
  entries: { path: string }[],
  anchorPath: string,
  targetPath: string,
): string[] {
  const anchor = entries.findIndex((e) => e.path === anchorPath);
  const target = entries.findIndex((e) => e.path === targetPath);
  if (anchor === -1 || target === -1) return [];
  const [from, to] = anchor <= target ? [anchor, target] : [target, anchor];
  return entries.slice(from, to + 1).map((e) => e.path);
}
