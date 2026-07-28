import { useState, useEffect } from "react";
import TabBar from "./components/TabBar";
import QueuePage from "./pages/QueuePage";
import HistoryPage from "./pages/HistoryPage";
import WatchedFoldersPage from "./pages/WatchedFoldersPage";
import SettingsPage from "./pages/SettingsPage";
import { commands, type HandbrakeStatus } from "./lib/tauri";
import { useAddProgress } from "./hooks/useAddProgress";
import { useFileIntake } from "./hooks/useFileIntake";
import { useUpdate } from "./hooks/useUpdate";
import "./App.css";

type Tab = "queue" | "history" | "watch" | "settings";

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("queue");
  const [hbStatus, setHbStatus] = useState<HandbrakeStatus | null>(null);
  const { isAdding, activity } = useAddProgress();
  const intake = useFileIntake({ onDrop: () => setActiveTab("queue") });
  const { state: updateState } = useUpdate();

  const refreshHbStatus = async () => {
    const status = await commands.validateHandbrake();
    setHbStatus(status);
  };

  useEffect(() => {
    refreshHbStatus();
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        commands.hideWindow();
      }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, []);

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
