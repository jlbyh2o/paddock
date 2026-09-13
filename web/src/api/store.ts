/**
 * The client-side state: one snapshot store plus three incremental buffers.
 *
 * The snapshot is absolute — every frame replaces the last — so it is held in a
 * tiny external store that `useSyncExternalStore` can read without React ever
 * re-rendering on a heartbeat.
 *
 * The three append-only collections of §3 (engine log, request ring, job output)
 * are *not* in the snapshot. Each has a buffer here, fetched by sequence number,
 * and each polls only while its tab is on screen and only when the snapshot's
 * counters say something was appended. The reducers below are pure so the gap and
 * reset rules can be tested directly.
 */

import { useCallback, useEffect, useRef, useSyncExternalStore } from "react";
import type {
  JobOutputLine,
  JobOutputPage,
  LogLine,
  LogsSnapshot,
  RequestRecord,
  RequestsSnapshot,
  SeqPage,
  Snapshot,
  ToastKind,
} from "./types.ts";
import { ApiError, api } from "./client.ts";

// ---------------------------------------------------------------- store

type Listener = () => void;

/** The smallest external store `useSyncExternalStore` will accept. */
export class Store<T> {
  private value: T;
  private readonly listeners = new Set<Listener>();

  constructor(initial: T) {
    this.value = initial;
  }

  readonly get = (): T => this.value;

  readonly set = (next: T): void => {
    if (Object.is(next, this.value)) return;
    this.value = next;
    for (const listener of this.listeners) listener();
  };

  readonly update = (fn: (prev: T) => T): void => {
    this.set(fn(this.value));
  };

  readonly subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
}

export function useStore<T>(store: Store<T>): T {
  return useSyncExternalStore(store.subscribe, store.get, store.get);
}

// ---------------------------------------------------------------- snapshot

export const snapshotStore = new Store<Snapshot | null>(null);

export interface ConnectionState {
  /** The SSE stream is delivering. */
  connected: boolean;
  /** True once a first snapshot has arrived, so the app can tell "loading" from "lost". */
  everConnected: boolean;
}

export const connectionStore = new Store<ConnectionState>({
  connected: false,
  everConnected: false,
});

export function useSnapshot(): Snapshot | null {
  return useStore(snapshotStore);
}

export function useConnection(): ConnectionState {
  return useStore(connectionStore);
}

// ---------------------------------------------------------------- local toasts

/**
 * Problems the daemon never heard about: a network failure, a 400, a 500. A refusal
 * the daemon has already toasted (`ApiError.toasted`) is *not* raised here — §1.2 says
 * to render one problem, not two.
 */
export interface LocalToast {
  id: number;
  text: string;
  kind: ToastKind;
  at: number;
  ttl_ms: number;
}

export const localToastStore = new Store<LocalToast[]>([]);

let localToastId = 0;

const TTL_BY_KIND: Record<ToastKind, number> = {
  info: 4000,
  success: 4000,
  warn: 8000,
  error: 12_000,
};

export function pushLocalToast(text: string, kind: ToastKind = "error"): void {
  localToastId -= 1;
  const toast: LocalToast = {
    id: localToastId,
    text,
    kind,
    at: Date.now(),
    ttl_ms: TTL_BY_KIND[kind],
  };
  localToastStore.update((prev) => [...prev, toast].slice(-4));
  setTimeout(() => {
    localToastStore.update((prev) => prev.filter((t) => t.id !== toast.id));
  }, toast.ttl_ms);
}

/**
 * The single place a failed action is turned into something the reader sees.
 * Returns true when the error was swallowed because the daemon is reporting it.
 *
 * The daemon says which of the two it is: `toasted` means the same sentence is on its
 * way in the next snapshot. A refusal with `toasted: false` belongs to the field that
 * was submitted, so a caller with an inline slot for it (the Serve tab) handles it
 * before this is reached; anything that gets here has no such slot and is toasted
 * locally, because a silent refusal is worse than a duplicated one.
 */
export function reportError(error: unknown): boolean {
  if (error instanceof ApiError) {
    if (error.isUnauthorized) return true; // the login page is already coming
    if (error.toasted) return true; // the daemon's own toast explains it
    pushLocalToast(error.message, error.status === 0 ? "warn" : "error");
    return false;
  }
  pushLocalToast(error instanceof Error ? error.message : String(error), "error");
  return false;
}

/** Run an action, swallow nothing silently, and never let a rejection escape. */
export function run<T>(work: Promise<T>, onOk?: (value: T) => void): void {
  void work.then(
    (value) => {
      if (onOk) onOk(value);
    },
    (error: unknown) => {
      reportError(error);
    },
  );
}

// ---------------------------------------------------------------- sequence buffers

/** A window onto one append-only collection, plus what is known to be missing. */
export interface SeqBuffer<T> {
  items: T[];
  /** The sequence of the newest item rendered. 0 before anything has been seen. */
  heldSeq: number;
  /** The server's own eviction count, as last observed. */
  dropped: number;
  /** How many items were lost in gaps this buffer has lived through. */
  gapDropped: number;
}

export function emptyBuffer<T>(): SeqBuffer<T> {
  return { items: [], heldSeq: 0, dropped: 0, gapDropped: 0 };
}

interface SeqCounters {
  count: number;
  first_seq: number;
  last_seq: number;
  dropped: number;
}

/**
 * Fold one page into the buffer.
 *
 * §3: `first_seq > heldSeq + 1` means items were evicted between two polls, so the
 * buffer is discarded and what came back is rendered with an elision marker of
 * `first_seq - heldSeq - 1`. A `last_seq` *below* what is held can only mean the
 * daemon restarted and renumbered, which is the same recovery.
 */
export function applyPage<T extends { seq: number }>(
  prev: SeqBuffer<T>,
  page: SeqPage<T>,
  cap: number,
): SeqBuffer<T> {
  const restarted = prev.heldSeq > 0 && page.last_seq > 0 && page.last_seq < prev.heldSeq;
  if (restarted) {
    return {
      items: page.items.slice(-cap),
      heldSeq: Math.max(page.next_after, lastSeqOf(page.items)),
      dropped: page.dropped,
      gapDropped: 0,
    };
  }

  const lost =
    prev.heldSeq > 0 && page.items.length > 0 && page.first_seq > prev.heldSeq + 1
      ? page.first_seq - prev.heldSeq - 1
      : 0;

  const base = lost > 0 ? [] : prev.items;
  const fresh = page.items.filter((item) => item.seq > prev.heldSeq);
  const items = (lost > 0 ? [...page.items] : [...base, ...fresh]).slice(-cap);

  return {
    items,
    heldSeq: Math.max(prev.heldSeq, page.next_after, lastSeqOf(page.items)),
    dropped: page.dropped,
    gapDropped: prev.gapDropped + lost,
  };
}

function lastSeqOf<T extends { seq: number }>(items: T[]): number {
  const last = items[items.length - 1];
  return last ? last.seq : 0;
}

/**
 * Reconcile the buffer against the counters in the snapshot, which arrive far more
 * often than pages do.
 *
 * A clear (`POST /api/logs/clear`, or `c` pressed in the TUI) leaves `count` at 0
 * while `dropped` absorbs what went, so sequence comparison keeps working but the
 * lines we hold are gone from the server and must go from here too.
 */
export function syncBuffer<T>(prev: SeqBuffer<T>, counters: SeqCounters): SeqBuffer<T> {
  if (prev.heldSeq > 0 && counters.last_seq < prev.heldSeq) {
    // The daemon's counter is below what we hold, so it restarted and renumbered
    // underneath us. `last_seq` is 0 for an empty collection, which is exactly what a
    // fresh process publishes, so a zero is the *most* likely restart and not a case
    // to exclude.
    return emptyBuffer<T>();
  }
  if (counters.count === 0 && prev.items.length > 0) {
    return { items: [], heldSeq: counters.last_seq, dropped: counters.dropped, gapDropped: 0 };
  }
  if (counters.dropped === prev.dropped) return prev;
  return { ...prev, dropped: counters.dropped };
}

/** True when the collection has items the buffer has not fetched yet. */
export function isBehind<T>(buffer: SeqBuffer<T>, counters: SeqCounters): boolean {
  return counters.last_seq > buffer.heldSeq;
}

// ---------------------------------------------------------------- feeds

const LOG_CAP = 4000;
const REQUEST_CAP = 512;

class Feed<T extends { seq: number }> {
  readonly store: Store<SeqBuffer<T>>;
  private inFlight = false;

  constructor(
    private readonly fetchPage: (after: number) => Promise<SeqPage<T>>,
    private readonly cap: number,
  ) {
    this.store = new Store<SeqBuffer<T>>(emptyBuffer<T>());
  }

  readonly reset = (): void => {
    this.store.set(emptyBuffer<T>());
  };

  readonly sync = (counters: SeqCounters): void => {
    this.store.update((prev) => syncBuffer(prev, counters));
  };

  readonly poll = async (): Promise<void> => {
    if (this.inFlight) return;
    this.inFlight = true;
    try {
      const page = await this.fetchPage(this.store.get().heldSeq);
      this.store.update((prev) => applyPage(prev, page, this.cap));
    } catch (error) {
      reportError(error);
    } finally {
      this.inFlight = false;
    }
  };
}

export const logFeed = new Feed<LogLine>((after) => api.logs(after, 500), LOG_CAP);
export const requestFeed = new Feed<RequestRecord>(
  (after) => api.requests(after, 200),
  REQUEST_CAP,
);

/**
 * Keep a feed current while its tab is on screen.
 *
 * Polling is gated twice: on `active`, so a hidden tab costs nothing, and on the
 * snapshot's own counter having moved past what is held, so a still engine
 * produces no requests at all.
 */
function useFeed<T extends { seq: number }>(
  feed: Feed<T>,
  active: boolean,
  counters: SeqCounters | null,
): SeqBuffer<T> {
  const buffer = useStore(feed.store);
  const lastSeq = counters?.last_seq ?? 0;
  const count = counters?.count ?? 0;
  const dropped = counters?.dropped ?? 0;
  const firstSeq = counters?.first_seq ?? 0;

  useEffect(() => {
    feed.sync({ count, first_seq: firstSeq, last_seq: lastSeq, dropped });
  }, [feed, count, firstSeq, lastSeq, dropped]);

  useEffect(() => {
    if (!active) return;
    if (lastSeq <= buffer.heldSeq) return;
    void feed.poll();
  }, [feed, active, lastSeq, buffer.heldSeq]);

  return buffer;
}

export function useLogFeed(active: boolean, counters: LogsSnapshot | null): SeqBuffer<LogLine> {
  return useFeed(logFeed, active, counters);
}

export function useRequestFeed(
  active: boolean,
  counters: RequestsSnapshot | null,
): SeqBuffer<RequestRecord> {
  return useFeed(requestFeed, active, counters);
}

// ---------------------------------------------------------------- job output

/** The tail of one job's log file. A byte offset is the cursor, not a sequence. */
export interface OutputBuffer {
  id: number | null;
  offset: number;
  lines: JobOutputLine[];
  eof: boolean;
  /** The file was rotated or removed under us and the read restarted at 0. */
  restarted: boolean;
}

const OUTPUT_CAP = 2000;

export function emptyOutput(): OutputBuffer {
  return { id: null, offset: 0, lines: [], eof: true, restarted: false };
}

export function applyOutput(
  prev: OutputBuffer,
  page: JobOutputPage,
  cap = OUTPUT_CAP,
): OutputBuffer {
  const sameJob = prev.id === page.id;
  const keep = sameJob && !page.truncated ? prev.lines : [];
  return {
    id: page.id,
    offset: page.next_offset,
    lines: [...keep, ...page.lines].slice(-cap),
    eof: page.eof,
    restarted: page.truncated,
  };
}

export const jobOutputStore = new Store<OutputBuffer>(emptyOutput());

/**
 * Poll one job's output.
 *
 * `JobEntry.output_seq` (§2.14) is the change counter: it is the job's output line
 * counter, so a new value means new lines and an unchanged one means there is nothing
 * to ask for. Selecting a job reads once; after that a fetch happens only when the
 * counter moves — and because the final status line moves it too, there is no special
 * case for a job that stopped.
 *
 * Two races are handled here rather than left to luck. A counter that moves while a
 * read is in flight is remembered and re-read when that read lands, so the pane never
 * stops one page short of the end. And a page that arrives after the reader picked a
 * different job is dropped, so job 3's tail is never shown under job 4's heading.
 */
export function useJobOutput(jobId: number | null, active: boolean, outputSeq: number): OutputBuffer {
  const buffer = useStore(jobOutputStore);
  const busy = useRef(false);
  /** A job whose output was asked for while a read was already in flight. */
  const queued = useRef<number | null>(null);
  /** The job the reader is looking at right now. */
  const wanted = useRef<number | null>(jobId);

  const poll = useCallback(async (first: number) => {
    if (busy.current) {
      queued.current = first;
      return;
    }
    busy.current = true;
    try {
      let id: number | null = first;
      while (id !== null) {
        const current = jobOutputStore.get();
        const offset = current.id === id ? current.offset : 0;
        const page = await api.jobOutput(id, offset);
        // The selection may have moved on while this was in flight.
        if (wanted.current === page.id) {
          jobOutputStore.update((prev) => applyOutput(prev, page));
        }
        id = queued.current;
        queued.current = null;
      }
    } catch (error) {
      reportError(error);
    } finally {
      queued.current = null;
      busy.current = false;
    }
  }, []);

  useEffect(() => {
    wanted.current = jobId;
    if (jobId === null) {
      jobOutputStore.set(emptyOutput());
      return;
    }
    // Never leave one job's lines on screen under another job's heading while the
    // first page for the new one is in flight.
    if (jobOutputStore.get().id !== jobId) jobOutputStore.set(emptyOutput());
    if (!active) return;
    // `outputSeq` is in the dependency list rather than the body: the effect is the
    // poll, and it re-runs exactly when the counter moved.
    void poll(jobId);
  }, [jobId, active, outputSeq, poll]);

  return buffer;
}

// ---------------------------------------------------------------- wiring

/** The `seq` of the newest document applied, which is what makes a rewind detectable. */
let appliedSeq = 0;

/** Drop every incremental buffer: the daemon restarted and renumbered. */
export function resetFeeds(): void {
  logFeed.reset();
  requestFeed.reset();
  jobOutputStore.set(emptyOutput());
  appliedSeq = 0;
}

/**
 * Replace the held snapshot, unless it would rewind the view.
 *
 * §2.2: `seq` is monotonic for the life of the daemon, so a document that is not newer
 * than the one already applied carries nothing new and may carry something stale. The
 * first paint's `GET /api/snapshot` routinely loses that race against the stream's
 * first frame, and applying its older document would undo a state the reader already
 * saw. A genuinely *lower* `seq` is the daemon having restarted, which `events.ts`
 * detects and answers with `resetFeeds`, clearing this counter so the new process's
 * first frame is accepted.
 *
 * Returns true when the document was applied.
 */
export function applySnapshot(snapshot: Snapshot): boolean {
  if (appliedSeq > 0 && snapshot.seq <= appliedSeq) return false;
  appliedSeq = snapshot.seq;
  snapshotStore.set(snapshot);
  return true;
}

export function setConnected(connected: boolean): void {
  connectionStore.update((prev) => ({
    connected,
    everConnected: prev.everConnected || connected,
  }));
}
