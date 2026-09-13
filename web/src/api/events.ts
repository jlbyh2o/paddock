/**
 * The `GET /api/events` Server-Sent Events stream.
 *
 * `EventSource` cannot send an `Authorization` header, so the cookie is what
 * authenticates it (§1.3) and `withCredentials` keeps the browser sending it.
 *
 * Two things this layer owns:
 *  - **reconnection.** The daemon sends `retry: 2000`, but a stream that is closed
 *    outright (the daemon stopped, a proxy dropped it) needs a fresh `EventSource`.
 *    It backs off 1s → 10s and reports the gap so the chrome can show its banner.
 *  - **`seq` reset detection.** `seq` is monotonic for the life of the process and
 *    restarts at 1 when the daemon does (§2.2). A snapshot whose `seq` is *lower*
 *    than the one held means everything must be re-fetched, including the
 *    incremental streams of §3, which are numbered by the same process lifetime.
 */

import type { Snapshot } from "./types.ts";
import { MOCK, api } from "./client.ts";

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
 * Open the stream. Returns a function that closes it and cancels any pending
 * reconnect.
 */
export function openEventStream(handlers: EventStreamHandlers): () => void {
  if (MOCK) return openMockStream(handlers);

  let source: EventSource | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let backoff = MIN_BACKOFF_MS;
  let heldSeq = 0;
  let closed = false;
  let live = false;

  const markDown = () => {
    if (!live) return;
    live = false;
    handlers.onDisconnected();
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
    if (!live) {
      live = true;
      backoff = MIN_BACKOFF_MS;
      handlers.onConnected();
    }
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

  const checkAuth = () => {
    void api
      .auth()
      .then((status) => {
        if (status.auth_required && !status.authorized) handlers.onUnauthorized();
      })
      .catch(() => {
        // Still unreachable; the reconnect timer is the retry path.
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
    source.addEventListener("heartbeat", () => {
      if (!live) {
        live = true;
        backoff = MIN_BACKOFF_MS;
        handlers.onConnected();
      }
    });
    source.onerror = () => {
      markDown();
      // readyState CONNECTING means EventSource is retrying on its own; only a
      // closed stream needs a new one. Either way, a dead stream may mean the
      // session expired, so ask.
      if (source && source.readyState === EventSource.CLOSED) {
        source.close();
        source = null;
        checkAuth();
        scheduleReconnect();
      }
    };
  };

  connect();

  return () => {
    closed = true;
    if (timer !== null) clearTimeout(timer);
    timer = null;
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
