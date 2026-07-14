import type { JobInfo, PathsExist } from "./tauri";

/**
 * Path a history entry's context menu should act on, or null when no surviving
 * file exists on disk.
 *
 * Survival rules (mirroring the converter's cleanup):
 * - in-place (source === output): the single path holds the result
 * - kept_file "converted": the source was trashed/deleted, the output survives
 * - kept_file "original", "skipped", or "error": the output was removed (or
 *   never produced), the source survives
 * In the distinct-path cases the other path is used as a fallback when the
 * preferred one is gone — covering error-status ambiguity and files moved
 * since conversion.
 */
export function resolveTargetPath(
  job: JobInfo,
  exists: PathsExist,
): string | null {
  if (job.source_path === job.output_path) {
    return exists.source_exists ? job.source_path : null;
  }
  const preferOutput = job.kept_file === "converted";
  const [primary, primaryExists, fallback, fallbackExists] = preferOutput
    ? [job.output_path, exists.output_exists, job.source_path, exists.source_exists]
    : [job.source_path, exists.source_exists, job.output_path, exists.output_exists];
  if (primaryExists) return primary;
  if (fallbackExists) return fallback;
  return null;
}
