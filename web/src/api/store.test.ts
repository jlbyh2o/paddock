import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import type { JobOutputPage, LogLine, SeqPage } from "./types.ts";
import { api } from "./client.ts";
import {
  applyOutput,
  applyPage,
  emptyBuffer,
  emptyOutput,
  isBehind,
  syncBuffer,
  useJobOutput,
} from "./store.ts";

function line(seq: number, text = `line ${seq}`): LogLine {
  return { seq, text, err: false };
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
    lines: ["a", "b"],
    ...over,
  });

  it("appends and advances the byte cursor", () => {
    const first = applyOutput(emptyOutput(), page({}));
    const next = applyOutput(first, page({ offset: 100, next_offset: 180, lines: ["c"] }));
    expect(next.lines).toEqual(["a", "b", "c"]);
    expect(next.offset).toBe(180);
  });

  it("starts fresh for a different job", () => {
    const first = applyOutput(emptyOutput(), page({}));
    const next = applyOutput(first, page({ id: 4, lines: ["x"] }));
    expect(next.id).toBe(4);
    expect(next.lines).toEqual(["x"]);
  });

  it("discards what it held when the file was rotated", () => {
    const first = applyOutput(emptyOutput(), page({}));
    const next = applyOutput(first, page({ truncated: true, lines: ["fresh"] }));
    expect(next.lines).toEqual(["fresh"]);
    expect(next.restarted).toBe(true);
  });

  it("caps the tail it keeps", () => {
    const next = applyOutput(emptyOutput(), page({ lines: ["a", "b", "c"] }), 2);
    expect(next.lines).toEqual(["b", "c"]);
  });
});

/**
 * §2.14 and §3.2: the job output pane used to poll on a one-second timer because the
 * snapshot carried no counter for a job's file. `JobEntry.output_bytes` is that counter,
 * so a fetch happens when it moves and not otherwise — plus one last read when the job
 * stops running, because the final lines can land in the tick that sets `finished_at`.
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
    vi.restoreAllMocks();
  });

  it("fetches on selection, on every change of output_bytes, and once when the job stops", async () => {
    const fetched = vi.spyOn(api, "jobOutput").mockImplementation((id: number) =>
      Promise.resolve(emptyPage(id)),
    );

    const view = renderHook(
      ({ running, bytes }: { running: boolean; bytes: number }) =>
        useJobOutput(3, true, running, bytes),
      { initialProps: { running: true, bytes: 0 } },
    );
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(1);

    // A snapshot that changed nothing about the file asks for nothing.
    view.rerender({ running: true, bytes: 0 });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(1);

    // The file grew.
    view.rerender({ running: true, bytes: 4096 });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(2);

    // It stopped running: one last read, even though the size did not move again.
    view.rerender({ running: false, bytes: 4096 });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(3);

    // And then nothing, however many snapshots arrive.
    view.rerender({ running: false, bytes: 4096 });
    view.rerender({ running: false, bytes: 4096 });
    await act(async () => {});
    expect(fetched).toHaveBeenCalledTimes(3);

    view.unmount();
  });
});
