import { useState, useEffect } from "react";
import TabBar, { type Tab, TAB_LABELS, tabId, tabPanelId } from "./components/TabBar";
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

const panelTitleId = (tab: Tab) => `panel-title-${tab}`;

/** One rendered column. `tabbed` is the single column the TabBar switches between (no title of
 *  its own — the TabBar already says which panel it holds). */
interface Column {
  key: string;
  tabs: Tab[];
  tabbed: boolean;
}

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
      default: {
        // noImplicitReturns is off, so without this the return type would silently widen to
        // `JSX.Element | undefined` (a legal ReactNode) if a fifth Tab member were ever added,
        // and the new tab would render a blank column with no compiler error.
        const exhaustive: never = tab;
        throw new Error(`Unhandled tab: ${exhaustive}`);
      }
    }
  };

  // One keyed list for every layout, rather than a `layout === "three-col" ? … : …` swap between
  // two JSX trees. Two trees do not reconcile: their child slots line up by position, so crossing
  // 900px or 1300px tore down and rebuilt every panel — and browser zoom (Cmd +/-) crosses those
  // widths without the window moving. Queue paid for it every time, since it renders in all three
  // layouts: an open file picker and the cross-folder selection it was gathering both vanished.
  // Keying each column by the panels it holds makes a crossing a *reorder* of the same columns,
  // so any panel present on both sides keeps its instance. Panels genuinely absent from the
  // target layout still unmount — Settings commits its drafts on the way out for that.
  const columns: Column[] =
    layout === "three-col"
      ? [
          // Watch and Settings share the last column: Settings is by far the longest panel, and
          // pairing it with the shortest balances the row.
          { key: "queue", tabs: ["queue"], tabbed: false },
          { key: "history", tabs: ["history"], tabbed: false },
          { key: "watch+settings", tabs: ["watch", "settings"], tabbed: false },
        ]
      : [
          ...pinned.map((tab) => ({ key: tab, tabs: [tab], tabbed: false })),
          ...(activeTab ? [{ key: activeTab, tabs: [activeTab], tabbed: true }] : []),
        ];

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
        {columns.map((column) => (
          <div className={column.tabbed ? "app-column page" : "app-column"} key={column.key}>
            {column.tabs.map((tab) => (
              // A panel, not a column, is the unit worth naming: the last column holds two of
              // them, and a <section> is only a landmark once it has a label — an h2 inside it
              // names nothing. The tabbed column's panel is instead the tabpanel its tab button
              // controls, which is where its name comes from since no title is rendered there.
              // The title occupies a slot even when it isn't rendered, so a panel that changes
              // column kind (pinned <-> tabbed) still finds itself at the same position.
              <section
                key={tab}
                id={column.tabbed ? tabPanelId(tab) : undefined}
                role={column.tabbed ? "tabpanel" : undefined}
                aria-labelledby={column.tabbed ? tabId(tab) : panelTitleId(tab)}
              >
                {!column.tabbed && (
                  <h2 className="app-column-title" id={panelTitleId(tab)}>
                    {TAB_LABELS[tab]}
                  </h2>
                )}
                {panel(tab)}
              </section>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

export default App;
