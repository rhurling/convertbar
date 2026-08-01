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

  it("carries the server's panic discriminator onto the thrown error", async () => {
    // Both shapes are 500s with an `error` field, so the status and the message cannot tell
    // them apart — `kind` is the entire distinction, and dropping it here would silently
    // undo the route helpers' work: `errorText` would label a bug on desktop and not on this
    // head, from the same UI code.
    fetchMock.mockResolvedValue(
      mockResponse(500, { error: "task panicked: task 3 panicked", kind: "panic" }),
    );
    await expect(httpCommands.getQueue()).rejects.toMatchObject({ kind: "panic" });

    fetchMock.mockResolvedValue(mockResponse(500, { error: "HandBrakeCLI not found" }));
    await expect(httpCommands.getQueue()).rejects.not.toHaveProperty("kind");
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

  describe("deadlines", () => {
    // A stalled connection: headers never arrive and the socket is never closed, so the
    // fetch promise only ever settles if the transport itself aborts it.
    const stall = () =>
      fetchMock.mockImplementation(
        (_path: string, init: RequestInit) =>
          new Promise((_resolve, reject) => {
            init.signal?.addEventListener("abort", () =>
              reject(new DOMException("aborted", "AbortError")),
            );
          }),
      );

    beforeEach(() => vi.useFakeTimers());
    afterEach(() => vi.useRealTimers());

    it("aborts a request that never settles instead of awaiting it forever", async () => {
      stall();
      const pending = httpCommands.getSettings();
      const rejects = expect(pending).rejects.toThrow(/timed out/i);

      await vi.advanceTimersByTimeAsync(30_000);
      await rejects;
    });

    it("gives the filesystem-walking intake routes a longer deadline than ordinary calls", async () => {
      stall();
      const pending = httpCommands.classifyPaths(["/media"]);
      let settled = false;
      pending.catch(() => {
        settled = true;
      });

      // A recursive video scan of a large tree on a network mount routinely outlives the
      // ordinary deadline. Cutting intake off at 30s would trade a rare hang for a
      // guaranteed failure on exactly the libraries this head exists to serve.
      await vi.advanceTimersByTimeAsync(60_000);
      expect(settled).toBe(false);

      await vi.advanceTimersByTimeAsync(10 * 60_000);
      await expect(pending).rejects.toThrow(/timed out/i);
    });

    it("clears the deadline timer once a request completes", async () => {
      fetchMock.mockResolvedValue(mockResponse(200, { ok: true }));
      await httpCommands.getSettings();

      // Every call arms a timer holding an AbortController; leaving them pending would
      // accumulate one per request across a long-lived tab (the queue refetches constantly).
      expect(vi.getTimerCount()).toBe(0);
    });
  });
});
