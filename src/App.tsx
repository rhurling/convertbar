import { useState, useEffect } from "react";
import TabBar from "./components/TabBar";
import QueuePage from "./pages/QueuePage";
import HistoryPage from "./pages/HistoryPage";
import SettingsPage from "./pages/SettingsPage";
import { commands, type HandbrakeStatus } from "./lib/tauri";
import "./App.css";

type Tab = "queue" | "history" | "settings";

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("queue");
  const [hbStatus, setHbStatus] = useState<HandbrakeStatus | null>(null);

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
      <TabBar activeTab={activeTab} onTabChange={setActiveTab} />
      <div className="page">
        {activeTab === "queue" && <QueuePage hbStatus={hbStatus} />}
        {activeTab === "history" && <HistoryPage />}
        {activeTab === "settings" && <SettingsPage onHbPathChanged={refreshHbStatus} />}
      </div>
    </div>
  );
}

export default App;
