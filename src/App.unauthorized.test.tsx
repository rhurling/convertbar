import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";

vi.mock("./hooks/useLayoutMode", () => ({ useLayoutMode: () => "tabs" }));
vi.mock("./pages/QueuePage", () => ({ default: () => <div data-testid="queue-page" /> }));
vi.mock("./pages/HistoryPage", () => ({ default: () => <div data-testid="history-page" /> }));
vi.mock("./pages/WatchedFoldersPage", () => ({ default: () => <div data-testid="watch-page" /> }));
vi.mock("./pages/SettingsPage", () => ({ default: () => <div data-testid="settings-page" /> }));
vi.mock("./hooks/useAddProgress", () => ({ useAddProgress: () => ({ isAdding: false, activity: null }) }));
vi.mock("./hooks/useUpdate", () => ({ useUpdate: () => ({ state: null }) }));
vi.mock("./hooks/useFileIntake", () => ({
  useFileIntake: () => ({
    pendingConfirm: null,
    onAdd: vi.fn(),
    onSkip: vi.fn(),
    status: null,
    isDragOver: false,
    addPaths: vi.fn(),
  }),
}));
vi.mock("./lib/tauri", () => ({
  commands: {
    validateHandbrake: () => Promise.resolve({ found: true, path: "/usr/bin/HandBrakeCLI" }),
    hideWindow: vi.fn(),
  },
}));

import App from "./App";

const fetchMock = vi.fn();

beforeEach(() => {
  vi.stubGlobal("fetch", fetchMock);
  fetchMock.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const status = (code: number) => Promise.resolve({ status: code } as Response);
const raiseUnauthorized = () => act(() => void window.dispatchEvent(new Event("convertbar:unauthorized")));

// The token form replaces the whole app, so it must follow a confirmed rejection, never a
// lone 401: the refetch burst when a job finishes once produced one spurious 401 and the form
// stayed until a manual sign-in.
describe("App unauthorized handling", () => {
  it("keeps the app when one 401 is not repeated on the re-check", async () => {
    fetchMock.mockImplementation(() => status(200));
    render(<App />);
    expect(await screen.findByTestId("queue-page")).toBeInTheDocument();

    raiseUnauthorized();

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    expect(fetchMock.mock.calls[0][0]).toBe("/api/status");
    await act(async () => {});
    expect(screen.queryByLabelText("Access token")).not.toBeInTheDocument();
    expect(screen.getByTestId("queue-page")).toBeInTheDocument();
  });

  it("shows the token form when the re-check is refused too", async () => {
    fetchMock.mockImplementation(() => status(401));
    render(<App />);
    expect(await screen.findByTestId("queue-page")).toBeInTheDocument();

    raiseUnauthorized();

    expect(await screen.findByLabelText("Access token")).toBeInTheDocument();
    expect(screen.queryByTestId("queue-page")).not.toBeInTheDocument();
  });

  it("does not show the form when the re-check cannot reach the server", async () => {
    fetchMock.mockImplementation(() => Promise.reject(new TypeError("Failed to fetch")));
    render(<App />);
    expect(await screen.findByTestId("queue-page")).toBeInTheDocument();

    raiseUnauthorized();

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    await act(async () => {});
    expect(screen.queryByLabelText("Access token")).not.toBeInTheDocument();
  });

  it("answers a burst of 401s with a single re-check", async () => {
    fetchMock.mockImplementation(() => status(401));
    render(<App />);
    expect(await screen.findByTestId("queue-page")).toBeInTheDocument();

    raiseUnauthorized();
    raiseUnauthorized();
    raiseUnauthorized();

    expect(await screen.findByLabelText("Access token")).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
