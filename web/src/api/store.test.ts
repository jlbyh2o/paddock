import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import type { JobOutputLine, JobOutputPage, LogLine, SeqPage, Snapshot } from "./types.ts";
import { api } from "./client.ts";
import { fixture } from "../mock/fixture.ts";
import {
  applyOutput,
  applyPage,
  applySnapshot,
  emptyBuffer,
  emptyOutput,
  isBehind,
  jobOutputStore,
  resetFeeds,
  snapshotStore,
  syncBuffer,
  useJobOutput,
} from "./store.ts";

function line(seq: number, text = `line ${seq}`): LogLine {
  return { seq, text, err: false, severity: "normal" };
}

function out(...texts: string[]): JobOutputLine[] {
  return texts.map((text) => ({ text, severity: "normal" }));
}

function pageOf(items: LogLine[], dropped = 0): SeqPage<LogLine> {
  const first = items[0];
  const last = items[items.length - 1];
  return {
    items,
    first_seq: first ? first.seq : 0,
    last_seq: last ? last.seq : 0,
    dropped,
    next_after: last ? last.seq : 0,
  };
}

describe("applyPage", () => {
  it("fills an empty buffer and remembers the sequence", () => {
    const next = applyPage(emptyBuffer<LogLine>(), pageOf([line(1), line(2), line(3)]), 100);
    expect(next.items.map((l) => l.seq)).toEqual([1, 2, 3]);
    expect(next.heldSeq).toBe(3);
    expect(next.gapDropped).toBe(0);
  });

  it("appends a continuation without duplicating", () => {
    const first = applyPage(emptyBuffer<LogLine>(), pageOf([line(1), line(2)]), 100);
    const next = applyPage(first, pageOf([line(2), line(3)]), 100);
    expect(next.items.map((l) => l.seq)).toEqual([1, 2, 3]);
    expect(next.heldSeq).toBe(3);
  });

  it("detects a gap, discards the buffer and counts what was lost", () => {
    const held = applyPage(emptyBuffer<LogLine>(), pageOf([line(9), line(10)]), 100);
    // The ring evicted 11..19 between polls: first_seq jumps past heldSeq + 1.
    const next = applyPage(held, pageOf([line(20), line(21)], 19), 100);
    expect(next.items.map((l) => l.seq)).toEqual([20, 21]);
    expect(next.gapDropped).toBe(9); // 20 - 10 - 1
    expect(next.heldSeq).toBe(21);
  });

  it("accumulates across more than one gap", () => {
    let buf = applyPage(emptyBuffer<LogLine>(), pageOf([line(1)]), 100);
    buf = applyPage(buf, pageOf([line(5)], 3), 100); // lost 2,3,4
    buf = applyPage(buf, pageOf([line(9)], 7), 100); // lost 6,7,8
    expect(buf.gapDropped).toBe(6);
  });

  it("recovers when the daemon restarted and the sequence went backwards", () => {
    const held = applyPage(emptyBuffer<LogLine>(), pageOf([line(500), line(501)]), 100);
    const next = applyPage(held, pageOf([line(1), line(2)]), 100);
    expect(next.items.map((l) => l.seq)).toEqual([1, 2]);
    expect(next.heldSeq).toBe(2);
    expect(next.gapDropped).toBe(0);
  });

  it("keeps the newest entries when the cap is reached", () => {
    const next = applyPage(
      emptyBuffer<LogLine>(),
      pageOf([line(1), line(2), line(3), line(4)]),
      2,
    );
    expect(next.items.map((l) => l.seq)).toEqual([3, 4]);
  });

  it("carries the server's eviction count through", () => {
    const next = applyPage(emptyBuffer<LogLine>(), pageOf([line(4096)], 4095), 100);
    expect(next.dropped).toBe(4095);
  });
});

describe("syncBuffer", () => {
  it("empties the buffer when the collection was cleared", () => {
    const held = applyPage(emptyBuffer<LogLine>(), pageOf([line(10), line(11)]), 100);
    const next = syncBuffer(held, { count: 0, first_seq: 11, last_seq: 11, dropped: 11 });
    expect(next.items).toEqual([]);
    expect(next.heldSeq).toBe(11); // comparison still works after a clear
    expect(next.dropped).toBe(11);
  });

  it("starts over when the sequence went backwards", () => {
    const held = applyPage(emptyBuffer<LogLine>(), pageOf([line(900)]), 100);
    const next = syncBuffer(held, { count: 3, first_seq: 1, last_seq: 3, dropped: 0 });
    expect(next).toEqual(emptyBuffer<LogLine>());
  });

  /**
   * §2.2: the counter being *below* what is held is the restart signal, whatever the
   * value. A daemon that has just started and logged nothing publishes `last_seq: 0`,
   * which is the most likely restart of all and used to be excluded as "no data yet" —
   * so the browser kept rendering the dead process's lines forever.
   */
  it("starts over when the daemon came back with an empty ring", () => {
    const held = applyPage(emptyBuffer<LogLine>(), pageOf([line(5000), line(5001)]), 100);
    const next = syncBuffer(held, { count: 0, first_seq: 0, last_seq: 0, dropped: 0 });
    expect(next).toEqual(emptyBuffer<LogLine>());
  });

  it("starts over when the daemon came back and has logged a line or two", () => {
    const held = applyPage(emptyBuffer<LogLine>(), pageOf([line(5000)]), 100);
    const next = syncBuffer(held, { count: 2, first_seq: 1, last_seq: 2, dropped: 0 });
    expect(next).toEqual(emptyBuffer<LogLine>());
  });

  it("is a no-op when nothing moved", () => {
    const held = applyPage(emptyBuffer<LogLine>(), pageOf([line(1)]), 100);
    const next = syncBuffer(held, { count: 1, first_seq: 1, last_seq: 1, dropped: 0 });
    expect(next).toBe(held);
  });

  it("knows when there is something to fetch", () => {
    const held = applyPage(emptyBuffer<LogLine>(), pageOf([line(7)]), 100);
    expect(isBehind(held, { count: 1, first_seq: 7, last_seq: 7, dropped: 0 })).toBe(false);
    expect(isBehind(held, { count: 2, first_seq: 7, last_seq: 8, dropped: 0 })).toBe(true);
  });
});

describe("applyOutput", () => {
  const page = (over: Partial<JobOutputPage>): JobOutputPage => ({
    id: 3,
    offset: 0,
    next_offset: 100,
    eof: true,
    truncated: false,
    lines: out("a", "b"),
    ...over,
  });

  const texts = (lines: JobOutputLine[]): string[] => lines.map((l) => l.text);

  it("appends and advances the byte cursor", () => {
    const first = applyOutput(emptyOutput(), page({}));
    const next = applyOutput(first, page({ offset: 100, next_offset: 180, lines: out("c") }));
    expect(texts(next.lines)).toEqual(["a", "b", "c"]);
    expect(next.offset).toBe(180);
  });

  it("starts fresh for a different job", () => {
    const first = applyOutput(emptyOutput(), page({}));
    const next = applyOutput(first, page({ id: 4, lines: out("x") }));
    expect(next.id).toBe(4);
    expect(texts(next.lines)).toEqual(["x"]);
  });

  it("discards what it held when the file was rotated", () => {
    const first = applyOutput(emptyOutput(), page({}));
    const next = applyOutput(first, page({ truncated: true, lines: out("fresh") }));
    expect(texts(next.lines)).toEqual(["fresh"]);
    expect(next.restarted).toBe(true);
  });

  it("caps the tail it keeps", () => {
    const next = applyOutput(emptyOutput(), page({ lines: out("a", "b", "c") }), 2);
    expect(texts(next.lines)).toEqual(["b", "c"]);
  });
});

/**
 * §2.2: `seq` is monotonic, and the first paint's `GET /api/snapshot` races the
 * stream's first frame. Applying the loser of that race rewinds the page to a state
 * the reader has already seen past — which is why the store, not the caller, decides.
 */
describe("applySnapshot", () => {
  afterEach(() => {
    resetFeeds();
    snapshotStore.set(null);
  });

  const at = (seq: number): Snapshot => ({ ...fixture, seq });

  it("applies a newer document and drops an older or repeated one", () => {
    expect(applySnapshot(at(10))).toBe(true);
    expect(applySnapshot(at(9))).toBe(false);
    expect(applySnapshot(at(10))).toBe(false);
    expect(snapshotStore.get()?.seq).toBe(10);
    expect(applySnapshot(at(11))).toBe(true);
    expect(snapshotStore.get()?.seq).toBe(11);
  });

  it("accepts the new process's first frame once the restart was noticed", () => {
    applySnapshot(at(4821));
    // `events.ts` sees the rewind and calls `resetFeeds`, which clears the counter.
    resetFeeds();
    expect(applySnapshot(at(1))).toBe(true);
    expect(snapshotStore.get()?.seq).toBe(1);
  });
});

/**
 * §2.14 and §3.2: the job output pane polls on `JobEntry.output_seq`, the job's own
 * output line counter. A fetch happens when it moves and not otherwise — and because
 * the final status line moves it too, there is no extra read after a job stops.
 */
describe("useJobOutput", () => {
  const emptyPage = (id: number): JobOutputPage => ({
    id,
    offset: 0,
    next_offset: 0,
    eof: true,
    truncated: false,
    lines: [],
  });

  afterEach(() => {
    cleanup();
    jobOutputStore.set(emptyOutput());
    vi.restoreAllMocks();
  });

  it("fetches on selection and on every change of output_seq, and not otherwise", async () => {
    const fetched = vi.spyOn(api, "jobOutput").mockImplementation((id: number) =>
      Promise.resolve(emptyPage(id)),
    );

    const view = renderHook(({ seq }: { seq: number }) => useJobOutput(3, true, seq), {
      initialProps: { seq: 0 },
    });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(1);

    // A snapshot that changed nothing about the job's output asks for nothing.
    view.rerender({ seq: 0 });
    view.rerender({ seq: 0 });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(1);

    // The job printed something.
    view.rerender({ seq: 12 });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(2);

    view.unmount();
  });

  /**
   * The counter moves while a read is in flight — a conversion prints steadily — and
   * the effect that noticed used to be dropped on the floor, leaving the pane one page
   * short of the end until something else happened to move the counter again.
   */
  it("re-reads once for a counter that moved while a read was in flight", async () => {
    const gates: (() => void)[] = [];
    const fetched = vi.spyOn(api, "jobOutput").mockImplementation(
      (id: number) =>
        new Promise((resolve) => {
          gates.push(() => {
            resolve(emptyPage(id));
          });
        }),
    );

    const view = renderHook(({ seq }: { seq: number }) => useJobOutput(3, true, seq), {
      initialProps: { seq: 1 },
    });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(1);

    // Two more frames land before the first read resolves; they coalesce into one
    // re-read, because a single read returns everything after the cursor.
    view.rerender({ seq: 2 });
    view.rerender({ seq: 3 });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(1);

    await act(async () => {
      gates[0]?.();
    });
    expect(fetched).toHaveBeenCalledTimes(2);

    await act(async () => {
      gates[1]?.();
    });
    expect(fetched).toHaveBeenCalledTimes(2);

    view.unmount();
  });

  it("never leaves one job's lines under another job's heading", async () => {
    const gates = new Map<number, () => void>();
    vi.spyOn(api, "jobOutput").mockImplementation(
      (id: number) =>
        new Promise((resolve) => {
          gates.set(id, () => {
            resolve({ ...emptyPage(id), lines: out(`output of job ${id}`) });
          });
        }),
    );

    const view = renderHook(({ id }: { id: number }) => useJobOutput(id, true, 1), {
      initialProps: { id: 3 },
    });
    await act(async () => {});

    // The reader moves to another job while job 3's read is still in flight.
    view.rerender({ id: 4 });
    await act(async () => {});
    expect(view.result.current.lines).toEqual([]);
    expect(view.result.current.id).toBe(null);

    await act(async () => {
      gates.get(3)?.();
    });
    expect(view.result.current.lines).toEqual([]);

    await act(async () => {
      gates.get(4)?.();
    });
    expect(view.result.current.id).toBe(4);
    expect(view.result.current.lines.map((l) => l.text)).toEqual(["output of job 4"]);

    view.unmount();
  });
});
