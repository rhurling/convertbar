import { useState } from "react";
import TabBar from "./components/TabBar";
import QueuePage from "./pages/QueuePage";
import HistoryPage from "./pages/HistoryPage";
import SettingsPage from "./pages/SettingsPage";
import "./App.css";

type Tab = "queue" | "history" | "settings";

function App() {
  const [activeTab, setActiveTab] = useState<Tab>("queue");
  return (
    <div className="app">
      <TabBar activeTab={activeTab} onTabChange={setActiveTab} />
      <div className="page">
        {activeTab === "queue" && <QueuePage />}
        {activeTab === "history" && <HistoryPage />}
        {activeTab === "settings" && <SettingsPage />}
      </div>
    </div>
  );
}

export default App;
