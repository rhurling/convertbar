import { useState, useEffect } from "react";
import TabBar, { type Tab, TAB_LABELS } from "./components/TabBar";
import QueuePage from "./pages/QueuePage";
import HistoryPage from "./pages/HistoryPage";
import WatchedFoldersPage from "./pages/WatchedFoldersPage";
import SettingsPage from "./pages/SettingsPage";
import LoginScreen from "./components/LoginScreen";
import { commands, type HandbrakeStatus } from "./lib/tauri";
import { isServerHead } from "./lib/head";
import { useAddProgress } from "./hooks/useAddProgress";
import { useFileIntake } from "./hooks/useFileIntake";
import { useUpdate } from "./hooks/useUpdate";
import { useLayoutMode, type LayoutMode } from "./hooks/useLayoutMode";
import "./App.css";

const PINNED: Record<LayoutMode, Tab[]> = {
  tabs: [],
  "two-col": ["queue"],
  "three-col": ["queue", "history", "watch", "settings"],
};

const ALL_TABS: Tab[] = ["queue", "history", "watch", "settings"];

function App() {
  const layout = useLayoutMode();
  // Deliberately still named setActiveTab: `useFileIntake({ onDrop: () => setActiveTab("queue") })`
  // below stays exactly as it is. Renaming the setter here would break that line,
  // and the derived `activeTab` below already absorbs a request for a pinned tab.
  const [requestedTab, setActiveTab] = useState<Tab>("queue");
  const [hbStatus, setHbStatus] = useState<HandbrakeStatus | null>(null);
  const [unauthorized, setUnauthorized] = useState(false);
  const { isAdding, activity } = useAddProgress();
  const intake = useFileIntake({ onDrop: () => setActiveTab("queue") });
  const { state: updateState } = useUpdate();

  // Server head only: desktop never dispatches this event.
  useEffect(() => {
    const handler = () => setUnauthorized(true);
    window.addEventListener("convertbar:unauthorized", handler);
    return () => window.removeEventListener("convertbar:unauthorized", handler);
  }, []);

  const refreshHbStatus = async () => {
    const status = await commands.validateHandbrake();
    setHbStatus(status);
  };

  useEffect(() => {
    refreshHbStatus();
  }, []);

  useEffect(() => {
    // Desktop-only: hideWindow() has no server equivalent (there's no window to hide), so the
    // listener itself is never registered on the server build rather than gating the call inside it.
    if (isServerHead) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        commands.hideWindow();
      }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, []);

  const pinned = PINNED[layout];
  const tabbed = ALL_TABS.filter((t) => !pinned.includes(t));
  // Derived, never stored: selecting a pinned tab resolves to a visible one instead of
  // blanking the tabbed column. This also covers useFileIntake's drop-to-Queue switch.
  // Undefined only in three-col, where `tabbed` is empty and nothing is tabbed at all.
  const activeTab: Tab | undefined = tabbed.includes(requestedTab) ? requestedTab : tabbed[0];

  const panel = (tab: Tab) => {
    switch (tab) {
      case "queue":
        return <QueuePage hbStatus={hbStatus} adding={activity} isAdding={isAdding} intake={intake} />;
      case "history":
        return <HistoryPage />;
      case "watch":
        return <WatchedFoldersPage />;
      case "settings":
        return <SettingsPage onHbPathChanged={refreshHbStatus} />;
    }
  };

  if (unauthorized) return <LoginScreen />;

  return (
    <div className={`app app-${layout}`}>
      <TabBar
        tabs={tabbed}
        activeTab={activeTab}
        onTabChange={setActiveTab}
        isAdding={isAdding}
        updateAvailable={updateState?.status === "available"}
      />
      <div className="app-columns">
        {/* three-col groups Watch and Settings into one column: Settings is by far the
            longest panel, and pairing it with the shortest balances the row. */}
        {layout === "three-col" ? (
          <>
            <section className="app-column">
              <h2 className="app-column-title">Queue</h2>
              {panel("queue")}
            </section>
            <section className="app-column">
              <h2 className="app-column-title">History</h2>
              {panel("history")}
            </section>
            <section className="app-column">
              <h2 className="app-column-title">Watch</h2>
              {panel("watch")}
              <h2 className="app-column-title">Settings</h2>
              {panel("settings")}
            </section>
          </>
        ) : (
          <>
            {pinned.map((tab) => (
              <section className="app-column" key={tab}>
                <h2 className="app-column-title">{TAB_LABELS[tab]}</h2>
                {panel(tab)}
              </section>
            ))}
            <section className="app-column page">{activeTab && panel(activeTab)}</section>
          </>
        )}
      </div>
    </div>
  );
}

export default App;
