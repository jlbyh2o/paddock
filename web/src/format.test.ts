import { describe, expect, it } from "vitest";
import {
  DASH,
  DASHES,
  basename,
  bytes,
  bytesShort,
  clock,
  count,
  dateOnly,
  decimal,
  duration,
  eta,
  fixed,
  ms,
  percent,
  rate,
  shortSha,
  signedBytes,
  signedCount,
  text,
  tokens,
  tps,
} from "./format.ts";

describe("bytes", () => {
  it("matches util::bytes at each width", () => {
    expect(bytes(0)).toBe("0 B");
    expect(bytes(512)).toBe("512 B");
    expect(bytes(1023)).toBe("1023 B");
    expect(bytes(1024)).toBe("1.00 KiB");
    expect(bytes(1024 * 10)).toBe("10.0 KiB");
    expect(bytes(1024 * 100)).toBe("100 KiB");
    expect(bytes(21_045_678_080)).toBe("19.6 GiB");
  });

  it("renders absence as an em dash, never NaN", () => {
    expect(bytes(null)).toBe(DASH);
    expect(bytes(undefined)).toBe(DASH);
    expect(bytes(Number.NaN)).toBe(DASH);
  });

  it("has a short form for fixed-width columns", () => {
    expect(bytesShort(1024)).toBe("1.0K");
    expect(bytesShort(21_045_678_080)).toBe("20G");
  });

  it("signs a delta", () => {
    expect(signedBytes(2048)).toBe("+2.00 KiB");
    expect(signedBytes(-2048)).toBe("-2.00 KiB");
    expect(signedBytes(null)).toBe(DASH);
  });
});

describe("count", () => {
  it("groups thousands the way util::count does", () => {
    expect(count(0)).toBe("0");
    expect(count(999)).toBe("999");
    expect(count(1000)).toBe("1,000");
    expect(count(1_234_567)).toBe("1,234,567");
    expect(count(-1234)).toBe("-1,234");
  });

  it("signs a delta", () => {
    expect(signedCount(581)).toBe("+581");
    expect(signedCount(-581)).toBe("-581");
  });

  it("renders absence", () => {
    expect(count(null)).toBe(DASH);
  });
});

describe("duration", () => {
  it("matches util::duration_secs", () => {
    expect(duration(0)).toBe("0s");
    expect(duration(45)).toBe("45s");
    expect(duration(750)).toBe("12m 30s");
    expect(duration(4200)).toBe("1h 10m");
    expect(duration(300_000)).toBe("3d 11h");
  });

  it("renders absence", () => {
    expect(duration(null)).toBe(DASH);
  });
});

describe("rate and eta", () => {
  it("prints -- when a rate is not meaningful, as the TUI does", () => {
    expect(rate(0)).toBe(DASHES);
    expect(rate(null)).toBe(DASHES);
    expect(rate(Number.POSITIVE_INFINITY)).toBe(DASHES);
  });

  it("formats a real rate", () => {
    expect(rate(325_058_560)).toBe("310 MiB/s");
  });

  it("formats an ETA the server already computed", () => {
    expect(eta(116)).toBe("1m 56s");
    expect(eta(null)).toBe(DASHES);
  });
});

describe("tokens", () => {
  it("matches plan::tokens", () => {
    expect(tokens(0)).toBe("0");
    expect(tokens(512)).toBe("512");
    expect(tokens(1024)).toBe("1k");
    expect(tokens(262_144)).toBe("256k");
    expect(tokens(238_694)).toBe("233.1k");
    expect(tokens(1 << 20)).toBe("1M");
    expect(tokens(1_600_000)).toBe("1.5M");
  });

  it("renders absence", () => {
    expect(tokens(null)).toBe(DASH);
  });
});

describe("numbers for the eye", () => {
  it("formats latencies", () => {
    expect(ms(8340)).toBe("8,340 ms");
    expect(ms(null)).toBe(DASH);
  });

  it("formats ratios as whole percent, clamped", () => {
    expect(percent(0.87)).toBe("87%");
    expect(percent(1.4)).toBe("100%");
    expect(percent(null)).toBe(DASH);
  });

  it("formats decimals with separators", () => {
    expect(decimal(3120.5)).toBe("3,120.5");
    expect(decimal(48.74)).toBe("48.7");
    expect(decimal(0.31, 2)).toBe("0.31");
    expect(fixed(null)).toBe(DASH);
  });

  it("formats throughput", () => {
    expect(tps(48.7)).toBe("48.7 tok/s");
    expect(tps(3120.5)).toBe("3,120.5 tok/s");
    expect(tps(null)).toBe(DASH);
  });
});

describe("strings", () => {
  it("clocks an ISO timestamp to HH:MM:SS", () => {
    expect(clock("2026-09-13T14:23:07.123456Z")).toMatch(/^\d{2}:\d{2}:\d{2}$/);
    expect(clock(null)).toBe(DASH);
  });

  it("takes the date part without inventing a timezone", () => {
    expect(dateOnly("2026-09-08T14:02:51.000Z")).toBe("2026-09-08");
    expect(dateOnly(null)).toBe(DASH);
  });

  it("shortens a sha", () => {
    expect(shortSha("c41b9d7e55a2f0d1b8e6a3c9f2470d5e8c1a6b33")).toBe("c41b9d7e55a2");
    expect(shortSha(null)).toBe(DASH);
  });

  it("names a path's basename", () => {
    expect(basename("/workspace/ftw/Qwen3.6-35B-A3B-NVFP4")).toBe("Qwen3.6-35B-A3B-NVFP4");
    expect(basename("/workspace/ftw/")).toBe("ftw");
    expect(basename(null)).toBe(DASH);
  });

  it("dashes an absent or blank string", () => {
    expect(text(null)).toBe(DASH);
    expect(text("   ")).toBe(DASH);
    expect(text("ok")).toBe("ok");
  });
});
