import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { httpCommands } from "./http";

function mockResponse(status: number, body?: unknown): Response {
  return {
    status,
    ok: status >= 200 && status < 300,
    json: () => Promise.resolve(body),
  } as Response;
}

const fetchMock = vi.fn();

beforeEach(() => {
  vi.stubGlobal("fetch", fetchMock);
  fetchMock.mockReset();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("http transport", () => {
  it("sends camelCase body keys (reorderQueue)", async () => {
    fetchMock.mockResolvedValue(mockResponse(204));

    await httpCommands.reorderQueue(["a", "b"]);

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/queue/order",
      expect.objectContaining({
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ jobIds: ["a", "b"] }),
        credentials: "same-origin",
      }),
    );
  });

  it("still sends the JSON content-type on a bodyless POST (startQueue — the CSRF-guard contract)", async () => {
    fetchMock.mockResolvedValue(mockResponse(204));

    await httpCommands.startQueue();

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/converter/start",
      expect.objectContaining({
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: undefined,
      }),
    );
  });

  it("resolves a 204 response to undefined", async () => {
    fetchMock.mockResolvedValue(mockResponse(204));

    await expect(httpCommands.clearQueue()).resolves.toBeUndefined();
  });

  it("throws the server's error message on a non-ok response", async () => {
    fetchMock.mockResolvedValue(mockResponse(500, { error: "boom" }));

    await expect(httpCommands.getQueue()).rejects.toThrow("boom");
  });

  it("dispatches convertbar:unauthorized and throws on a 401", async () => {
    fetchMock.mockResolvedValue(mockResponse(401, { error: "unauthorized" }));
    const handler = vi.fn();
    window.addEventListener("convertbar:unauthorized", handler);

    await expect(httpCommands.getQueue()).rejects.toThrow("unauthorized");
    expect(handler).toHaveBeenCalledTimes(1);

    window.removeEventListener("convertbar:unauthorized", handler);
  });

  it("desktop-only members throw a tripwire error", () => {
    expect(() => httpCommands.hideWindow()).toThrow("not available on server");
  });
});
