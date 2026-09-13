/**
 * The stream's recovery rules (§1.3, §2.1).
 *
 * `openEventStream` takes the fixture branch under vitest, so these drive
 * `openLiveStream` directly against a stand-in `EventSource` — the point of the
 * exercise being what the browser does when the stream misbehaves, which no fixture
 * can reproduce.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "./client.ts";
import { openLiveStream } from "./events.ts";
import type { EventStreamHandlers } from "./events.ts";

type Listener = (event: unknown) => void;

/** Just enough of `EventSource` to fail in the ways the real one fails. */
class FakeEventSource {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;

  readyState = FakeEventSource.CONNECTING;
  onerror: (() => void) | null = null;
  closed = false;
  private readonly listeners = new Map<string, Listener[]>();

  constructor(readonly url: string) {
    opened.push(this);
  }

  addEventListener(type: string, listener: Listener): void {
    const bucket = this.listeners.get(type) ?? [];
    bucket.push(listener);
    this.listeners.set(type, bucket);
  }

  close(): void {
    this.closed = true;
    this.readyState = FakeEventSource.CLOSED;
  }

  /** Deliver one frame, as the daemon would. */
  send(type: string, data: unknown): void {
    this.readyState = FakeEventSource.OPEN;
    for (const listener of this.listeners.get(type) ?? []) listener({ data: JSON.stringify(data) });
  }

  /** Fail the way `EventSource` does, in whichever state the browser would be in. */
  fail(state: number): void {
    this.readyState = state;
    this.onerror?.();
  }
}

let opened: FakeEventSource[] = [];
let originalEventSource: unknown;

function handlers(): EventStreamHandlers & { calls: Record<string, number> } {
  const calls = { snapshot: 0, reset: 0, connected: 0, disconnected: 0, unauthorized: 0 };
  return {
    calls,
    onSnapshot: () => {
      calls.snapshot += 1;
    },
    onReset: () => {
      calls.reset += 1;
    },
    onConnected: () => {
      calls.connected += 1;
    },
    onDisconnected: () => {
      calls.disconnected += 1;
    },
    onUnauthorized: () => {
      calls.unauthorized += 1;
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  opened = [];
  originalEventSource = (globalThis as Record<string, unknown>)["EventSource"];
  (globalThis as Record<string, unknown>)["EventSource"] = FakeEventSource;
});

afterEach(() => {
  (globalThis as Record<string, unknown>)["EventSource"] = originalEventSource;
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("openLiveStream", () => {
  /**
   * `EventSource` reports an `error` for every hiccup, including the ones it recovers
   * from by itself. Banner-on-first-error made a healthy stream flicker "Disconnected"
   * several times an hour.
   */
  it("stays quiet through a transient error and speaks up when the silence lasts", async () => {
    const h = handlers();
    const close = openLiveStream(h);
    const source = opened[0];
    if (!source) throw new Error("no stream was opened");

    source.send("snapshot", { seq: 5 });
    expect(h.calls.connected).toBe(1);

    source.fail(FakeEventSource.CONNECTING);
    expect(h.calls.disconnected).toBe(0);

    // It recovers on its own: nothing was ever said.
    source.send("heartbeat", {});
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.calls.disconnected).toBe(0);

    // This time it does not come back.
    source.fail(FakeEventSource.CONNECTING);
    await vi.advanceTimersByTimeAsync(2900);
    expect(h.calls.disconnected).toBe(0);
    await vi.advanceTimersByTimeAsync(200);
    expect(h.calls.disconnected).toBe(1);

    close();
  });

  it("probes auth before reconnecting, and does not reconnect when the session is gone", async () => {
    const auth = vi
      .spyOn(api, "auth")
      .mockResolvedValue({ auth_required: true, authorized: false });
    const h = handlers();
    const close = openLiveStream(h);
    const source = opened[0];
    if (!source) throw new Error("no stream was opened");
    source.send("snapshot", { seq: 5 });

    source.fail(FakeEventSource.CLOSED);
    await vi.advanceTimersByTimeAsync(0);
    expect(auth).toHaveBeenCalledTimes(1);
    expect(h.calls.unauthorized).toBe(1);
    expect(h.calls.disconnected).toBe(1);

    // No reconnect: another stream would be refused exactly the same way, and the
    // login page is already on its way.
    await vi.advanceTimersByTimeAsync(30_000);
    expect(opened).toHaveLength(1);

    close();
  });

  it("reconnects once the probe says the session is still good", async () => {
    vi.spyOn(api, "auth").mockResolvedValue({ auth_required: true, authorized: true });
    const h = handlers();
    const close = openLiveStream(h);
    const source = opened[0];
    if (!source) throw new Error("no stream was opened");
    source.send("snapshot", { seq: 5 });

    source.fail(FakeEventSource.CLOSED);
    await vi.advanceTimersByTimeAsync(0);
    expect(h.calls.unauthorized).toBe(0);
    expect(opened).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(1000);
    expect(opened).toHaveLength(2);

    close();
  });

  it("abandons an auth probe that outlives the stream", async () => {
    let answer: (() => void) | null = null;
    vi.spyOn(api, "auth").mockImplementation(
      () =>
        new Promise((resolve) => {
          answer = () => {
            resolve({ auth_required: true, authorized: false });
          };
        }),
    );
    const h = handlers();
    const close = openLiveStream(h);
    const source = opened[0];
    if (!source) throw new Error("no stream was opened");
    source.send("snapshot", { seq: 5 });

    source.fail(FakeEventSource.CLOSED);
    await vi.advanceTimersByTimeAsync(0);

    // The component unmounted while the probe was in flight: its answer is about a
    // stream that no longer exists and must not send anyone to the login page.
    close();
    (answer as (() => void) | null)?.();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(h.calls.unauthorized).toBe(0);
    expect(opened).toHaveLength(1);
  });
});
