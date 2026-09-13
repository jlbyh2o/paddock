import { describe, expect, it } from "vitest";
import type { JobOutputPage, LogLine, SeqPage } from "./types.ts";
import {
  applyOutput,
  applyPage,
  emptyBuffer,
  emptyOutput,
  isBehind,
  syncBuffer,
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
