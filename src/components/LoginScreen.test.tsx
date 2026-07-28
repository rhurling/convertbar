import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const loginMock = vi.fn();
vi.mock("../lib/transport/http", () => ({ httpCommands: { login: (token: string) => loginMock(token) } }));

import LoginScreen from "./LoginScreen";

beforeEach(() => {
  vi.clearAllMocks();
  Object.defineProperty(window, "location", {
    value: { ...window.location, reload: vi.fn() },
    writable: true,
  });
});

describe("LoginScreen", () => {
  it("renders a token input and a submit button", () => {
    render(<LoginScreen />);
    expect(screen.getByLabelText(/access token/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /sign in/i })).toBeInTheDocument();
  });

  it("submits the entered token and reloads the page on success", async () => {
    loginMock.mockResolvedValue(undefined);
    const user = userEvent.setup();
    render(<LoginScreen />);

    await user.type(screen.getByLabelText(/access token/i), "secret");
    await user.click(screen.getByRole("button", { name: /sign in/i }));

    await waitFor(() => expect(loginMock).toHaveBeenCalledWith("secret"));
    await waitFor(() => expect(window.location.reload).toHaveBeenCalled());
  });

  it("shows the error message when login fails and does not reload", async () => {
    loginMock.mockRejectedValue(new Error("unauthorized"));
    const user = userEvent.setup();
    render(<LoginScreen />);

    await user.type(screen.getByLabelText(/access token/i), "wrong");
    await user.click(screen.getByRole("button", { name: /sign in/i }));

    expect(await screen.findByText("unauthorized")).toBeInTheDocument();
    expect(window.location.reload).not.toHaveBeenCalled();
  });
});
