import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

type Listener = (e: { data: string }) => void;

class MockEventSource {
  url: string;
  listeners = new Map<string, Set<Listener>>();

  constructor(url: string) {
    this.url = url;
    instances.push(this);
  }

  addEventListener(type: string, cb: Listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type)!.add(cb);
  }

  removeEventListener(type: string, cb: Listener) {
    this.listeners.get(type)?.delete(cb);
  }

  emit(type: string, event: { data: string }) {
    this.listeners.get(type)?.forEach((cb) => cb(event));
  }
}

let instances: MockEventSource[] = [];

beforeEach(() => {
  vi.resetModules();
  instances = [];
  vi.stubGlobal("EventSource", MockEventSource);
  vi.stubEnv("VITE_HEAD", "server");
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

describe("events.ts (server head)", () => {
  it("opens exactly one shared EventSource against /api/events", async () => {
    await import("./events");

    expect(instances).toHaveLength(1);
    expect(instances[0].url).toBe("/api/events");
  });

  it("listen wraps the raw SSE payload in { payload } and unlisten removes the listener", async () => {
    const { listen } = await import("./events");
    const es = instances[0];
    const handler = vi.fn();

    const unlisten = await listen("job-completed", handler);
    es.emit("job-completed", { data: JSON.stringify({ id: "42" }) });
    expect(handler).toHaveBeenCalledWith({ payload: { id: "42" } });

    unlisten();
    handler.mockClear();
    es.emit("job-completed", { data: JSON.stringify({ id: "43" }) });
    expect(handler).not.toHaveBeenCalled();
  });

  it("listen returns a Promise (consumers rely on .then(unlisten => unlisten()))", async () => {
    const { listen } = await import("./events");
    const result = listen("job-completed", vi.fn());
    expect(result).toBeInstanceOf(Promise);
  });

  it("dispatches convertbar:events-reconnected only on an open that follows an error", async () => {
    await import("./events");
    const es = instances[0];
    const reconnectHandler = vi.fn();
    window.addEventListener("convertbar:events-reconnected", reconnectHandler);

    es.emit("open", { data: "" }); // initial connection: not a reconnect
    expect(reconnectHandler).not.toHaveBeenCalled();

    es.emit("error", { data: "" });
    es.emit("open", { data: "" }); // reconnect after a drop
    expect(reconnectHandler).toHaveBeenCalledTimes(1);

    window.removeEventListener("convertbar:events-reconnected", reconnectHandler);
  });
});
