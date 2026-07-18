import type { AddActivity } from "../lib/tauri";

interface AddingIndicatorProps {
  activity: AddActivity | null;
}

export default function AddingIndicator({ activity }: AddingIndicatorProps) {
  if (!activity) return null;

  const determinate = activity.total !== null && activity.total > 0;
  const percent = determinate
    ? Math.min(100, Math.round((activity.done! / activity.total!) * 100))
    : 0;

  return (
    <div className="adding-indicator">
      <div className="adding-indicator-label">
        <span className="spinner" aria-hidden="true" />
        <span>
          {determinate ? `Checking ${activity.done} of ${activity.total}…` : "Scanning…"}
        </span>
      </div>
      {determinate && (
        <div className="progress-bar-track">
          <div className="progress-bar-fill" style={{ width: `${percent}%` }} />
        </div>
      )}
    </div>
  );
}
