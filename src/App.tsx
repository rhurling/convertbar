import { useState, useEffect } from "react";
import TabBar from "./components/TabBar";
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
import "./App.css";

type Tab = "queue" | "history" | "watch" | "settings";

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("queue");
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

  if (unauthorized) return <LoginScreen />;

  return (
    <div className="app">
      <TabBar
        activeTab={activeTab}
        onTabChange={setActiveTab}
        isAdding={isAdding}
        updateAvailable={updateState?.status === "available"}
      />
      <div className="page">
        {activeTab === "queue" && (
          <QueuePage hbStatus={hbStatus} adding={activity} isAdding={isAdding} intake={intake} />
        )}
        {activeTab === "history" && <HistoryPage />}
        {activeTab === "watch" && <WatchedFoldersPage />}
        {activeTab === "settings" && <SettingsPage onHbPathChanged={refreshHbStatus} />}
      </div>
    </div>
  );
}

export default App;
