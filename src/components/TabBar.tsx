import { useRef, type KeyboardEvent } from "react";
import { commands } from "../lib/tauri";
import { isServerHead } from "../lib/head";

export type Tab = "queue" | "history" | "watch" | "settings";

/** Shared with App.tsx, which renders the panel each tab controls. */
export const tabId = (tab: Tab) => `tab-${tab}`;
export const tabPanelId = (tab: Tab) => `panel-${tab}`;

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
  const list = useRef<HTMLDivElement>(null);

  // Arrows move focus; the tab still has to be pressed to switch panels. Selection-follows-focus
  // is the ARIA default, but switching unmounts the panel that was showing — and Settings commits
  // its drafts on the way out — so arrowing past it would fire that on every keystroke.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    // Modified arrows belong to the browser: Cmd/Alt+Left is history-back on the server head,
    // and swallowing it would strand the user on the page.
    if (e.altKey || e.metaKey || e.ctrlKey || e.shiftKey) return;
    const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (step === 0 && e.key !== "Home" && e.key !== "End") return;
    const buttons = Array.from(list.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]') ?? []);
    const from = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (from === -1) return;
    e.preventDefault();
    const to =
      e.key === "Home"
        ? 0
        : e.key === "End"
          ? buttons.length - 1
          : (from + step + buttons.length) % buttons.length;
    buttons[to]?.focus();
  };

  return (
    <div className="tab-bar" data-tauri-drag-region>
      {/* The server head has no window title bar, and at three-col no tab buttons and no close
          button either — so the bar collapsed to its 1px border and then shoved all three
          columns down 12px the instant the adding spinner appeared. The title gives it a height
          that doesn't depend on what is transiently in it, and is the document's only h1:
          every panel title is an h2. */}
      {isServerHead && <h1 className="app-title">ConvertBar</h1>}
      {tabs.length > 0 && (
        <div
          className="tab-list"
          role="tablist"
          aria-label="Panels"
          ref={list}
          onKeyDown={onKeyDown}
          // Keeps every tab exactly the width it had before this wrapper existed: the bar used
          // to split itself between each tab and the drag spacer equally.
          style={{ flexGrow: tabs.length }}
        >
          {tabs.map((tab) => (
            <button
              key={tab}
              id={tabId(tab)}
              role="tab"
              aria-selected={activeTab === tab}
              // Only the selected panel is mounted, so it is the only one there is to point at.
              aria-controls={activeTab === tab ? tabPanelId(tab) : undefined}
              tabIndex={activeTab === tab ? 0 : -1}
              className={`tab-btn ${activeTab === tab ? "active" : ""}`}
              onClick={() => onTabChange(tab)}
            >
              {TAB_LABELS[tab]}
              {tab === "settings" && updateAvailable && (
                // role="img" so the label is legal ARIA on an empty element — it folds into
                // the tab's name ("Settings, update available"), which is the point of it.
                <span className="tab-badge" role="img" aria-label="Update available" />
              )}
            </button>
          ))}
        </div>
      )}
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
