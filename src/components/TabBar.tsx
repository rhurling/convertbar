import { commands } from "../lib/tauri";
import { isServerHead } from "../lib/head";

export type Tab = "queue" | "history" | "watch" | "settings";

interface TabBarProps {
  tabs: Tab[];
  /** Undefined in three-col, where every panel is pinned and nothing is tabbed. */
  activeTab: Tab | undefined;
  onTabChange: (tab: Tab) => void;
  isAdding: boolean;
  updateAvailable: boolean;
}

export const TAB_LABELS: Record<Tab, string> = {
  queue: "Queue",
  history: "History",
  watch: "Watch",
  settings: "Settings",
};

export default function TabBar({ tabs, activeTab, onTabChange, isAdding, updateAvailable }: TabBarProps) {
  return (
    <div className="tab-bar" data-tauri-drag-region>
      {tabs.map((tab) => (
        <button
          key={tab}
          className={`tab-btn ${activeTab === tab ? "active" : ""}`}
          onClick={() => onTabChange(tab)}
        >
          {TAB_LABELS[tab]}
          {tab === "settings" && updateAvailable && (
            <span className="tab-badge" aria-label="Update available" />
          )}
        </button>
      ))}
      <div className="tab-spacer" data-tauri-drag-region />
      {isAdding && (
        <span className="tab-spinner" title="Adding files to the queue…" aria-label="Adding files" />
      )}
      {!isServerHead && (
        <button className="tab-btn close-tab-btn" onClick={() => commands.hideWindow()} title="Close">
          &times;
        </button>
      )}
    </div>
  );
}
