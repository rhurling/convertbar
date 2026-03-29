import { commands } from "../lib/tauri";

type Tab = "queue" | "history" | "settings";

interface TabBarProps {
  activeTab: Tab;
  onTabChange: (tab: Tab) => void;
}

const tabs: { id: Tab; label: string }[] = [
  { id: "queue", label: "Queue" },
  { id: "history", label: "History" },
  { id: "settings", label: "Settings" },
];

export default function TabBar({ activeTab, onTabChange }: TabBarProps) {
  return (
    <div className="tab-bar" data-tauri-drag-region>
      {tabs.map((tab) => (
        <button
          key={tab.id}
          className={`tab-btn ${activeTab === tab.id ? "active" : ""}`}
          onClick={() => onTabChange(tab.id)}
        >
          {tab.label}
        </button>
      ))}
      <div className="tab-spacer" data-tauri-drag-region />
      <button className="tab-btn close-tab-btn" onClick={() => commands.hideWindow()} title="Close">
        &times;
      </button>
    </div>
  );
}
