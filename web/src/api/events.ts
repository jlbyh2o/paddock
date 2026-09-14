/**
 * The `GET /api/events` Server-Sent Events stream.
 *
 * `EventSource` cannot send an `Authorization` header, so the cookie is what
 * authenticates it (§1.3) and `withCredentials` keeps the browser sending it.
 *
 * Three things this layer owns:
 *  - **reconnection.** The daemon sends `retry: 2000`, but a stream that is closed
 *    outright (the daemon stopped, a proxy dropped it) needs a fresh `EventSource`.
 *    It backs off 1s → 10s and reports the gap so the chrome can show its banner.
 *  - **patience.** `EventSource` fires `error` for every hiccup, including the ones it
 *    recovers from by itself while `readyState` is still `CONNECTING`. Announcing a
 *    disconnection on the first of those makes the banner flicker on a stream that is
 *    fine, so the banner waits for a `CLOSED` stream or for `STALE_MS` with no frame.
 *  - **`seq` reset detection.** `seq` is monotonic for the life of the process and
 *    restarts at 1 when the daemon does (§2.2). A snapshot whose `seq` is *lower*
 *    than the one held means everything must be re-fetched, including the
 *    incremental streams of §3, which are numbered by the same process lifetime.
 */

import type { Snapshot } from "./types.ts";
import { api } from "./client.ts";

export interface EventStreamHandlers {
  /** A fresh, complete state document. */
  onSnapshot(snapshot: Snapshot): void;
  /** The daemon restarted: drop every incremental buffer and start from `after=0`. */
  onReset(): void;
  onConnected(): void;
  onDisconnected(): void;
  /** Auth expired while the stream was open. */
  onUnauthorized(): void;
}

const MIN_BACKOFF_MS = 1000;
const MAX_BACKOFF_MS = 10_000;

/**
 * How long a stream may go without a frame before the reader is told. The daemon
 * heartbeats once a second (§2.1), so three missed beats is a real gap rather than a
 * retry in flight.
 */
const STALE_MS = 3000;

/**
 * Open the stream. Returns a function that closes it and cancels any pending
 * reconnect or auth probe.
 *
 * In a mock build this is the fixture's ticker instead; `__PADDOCK_MOCK__` is a
 * compile-time constant, so a production build keeps only the live half.
 */
export function openEventStream(handlers: EventStreamHandlers): () => void {
  return __PADDOCK_MOCK__ ? openMockStream(handlers) : openLiveStream(handlers);
}

/**
 * The real `EventSource` half, exported so the tests can drive it directly: under
 * vitest `openEventStream` takes the fixture branch, and the reconnection rules are
 * exactly what wants testing.
 */
export function openLiveStream(handlers: EventStreamHandlers): () => void {
  let source: EventSource | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let staleTimer: ReturnType<typeof setTimeout> | null = null;
  let backoff = MIN_BACKOFF_MS;
  let heldSeq = 0;
  let closed = false;
  let live = false;
  /** Bumped to abandon an auth probe whose answer no longer matters. */
  let probeGeneration = 0;

  const clearStale = () => {
    if (staleTimer !== null) clearTimeout(staleTimer);
    staleTimer = null;
  };

  const markDown = () => {
    clearStale();
    if (!live) return;
    live = false;
    handlers.onDisconnected();
  };

  const markUp = () => {
    clearStale();
    if (live) return;
    live = true;
    backoff = MIN_BACKOFF_MS;
    handlers.onConnected();
  };

  /** A transient error: let `EventSource` retry, and only complain if it cannot. */
  const armStale = () => {
    if (closed || !live || staleTimer !== null) return;
    staleTimer = setTimeout(() => {
      staleTimer = null;
      markDown();
    }, STALE_MS);
  };

  const deliver = (raw: string) => {
    let parsed: Snapshot;
    try {
      parsed = JSON.parse(raw) as Snapshot;
    } catch {
      return; // A truncated frame: the next one will be whole.
    }
    if (typeof parsed.seq !== "number") return;
    if (heldSeq > 0 && parsed.seq < heldSeq) handlers.onReset();
    heldSeq = parsed.seq;
    markUp();
    handlers.onSnapshot(parsed);
  };

  const scheduleReconnect = () => {
    if (closed || timer !== null) return;
    const delay = backoff;
    backoff = Math.min(backoff * 2, MAX_BACKOFF_MS);
    timer = setTimeout(() => {
      timer = null;
      connect();
    }, delay);
  };

  /**
   * Ask whether the session is still good. `EventSource` never exposes a status, so a
   * closed stream and an expired token look identical from here (§1.3) — hence the
   * probe, and hence reconnecting only when it comes back authorized.
   */
  const probeAuth = (whenAuthorized: () => void) => {
    const generation = (probeGeneration += 1);
    const stale = () => closed || generation !== probeGeneration;
    void api
      .auth()
      .then((status) => {
        if (stale()) return;
        if (status.auth_required && !status.authorized) {
          handlers.onUnauthorized();
          return;
        }
        whenAuthorized();
      })
      .catch(() => {
        // The daemon is unreachable rather than refusing us; retrying is the answer.
        if (!stale()) whenAuthorized();
      });
  };

  const connect = () => {
    if (closed) return;
    try {
      source = new EventSource("/api/events", { withCredentials: true });
    } catch {
      scheduleReconnect();
      return;
    }

    source.addEventListener("snapshot", (ev) => {
      deliver((ev as MessageEvent<string>).data);
    });
    // A heartbeat only proves the stream is alive; there is nothing to apply.
    source.addEventListener("heartbeat", markUp);
    source.onerror = () => {
      if (closed) return;
      if (source && source.readyState === EventSource.CLOSED) {
        source.close();
        source = null;
        markDown();
        // The probe comes first: a stream refused because the session expired must
        // send the reader to the login page, not queue a reconnect that is refused
        // the same way.
        probeAuth(scheduleReconnect);
        return;
      }
      // CONNECTING: `EventSource` is retrying on its own and usually wins. Say
      // nothing unless the silence lasts.
      armStale();
    };
  };

  connect();

  return () => {
    closed = true;
    probeGeneration += 1; // abandon any auth probe still in flight
    if (timer !== null) clearTimeout(timer);
    timer = null;
    clearStale();
    if (source) source.close();
    source = null;
  };
}

function openMockStream(handlers: EventStreamHandlers): () => void {
  let stop: (() => void) | null = null;
  let cancelled = false;
  void import("../mock/server.ts").then((mod) => {
    if (cancelled) return;
    handlers.onConnected();
    stop = mod.subscribe((snapshot) => {
      handlers.onSnapshot(snapshot);
    });
  });
  return () => {
    cancelled = true;
    if (stop) stop();
  };
}
