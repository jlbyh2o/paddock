/**
 * Chart — the uPlot wrapper (§6). Renders without throwing against the fixture,
 * short-circuits when there is no series, wires its series into the readout, and
 * destroys cleanly on unmount.
 *
 * jsdom has no canvas 2D context, so the test stubs one (uPlot draws at
 * construction). uPlot's own import-time `window.matchMedia` is stubbed by
 * test/setup.ts, which runs before imports.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { fixture } from "../mock/fixture.ts";
import { Chart, type ChartSeries } from "./Chart.tsx";

/** Index x-axis aligned with the fixture's 120-sample series. */
const xIdx = fixture.series.decode_tps.map((_v, i) => i);

/** Decode on the left axis, prefill on the right — two scales, two series. */
function throughput(): ChartSeries[] {
  return [
    { label: "decode", values: fixture.series.decode_tps, stroke: "var(--good)", axis: "left" },
    { label: "prefill", values: fixture.series.prefill_tps, stroke: "var(--accent)", axis: "right" },
  ];
}

let origGetContext: typeof HTMLCanvasElement.prototype.getContext;

beforeEach(() => {
  origGetContext = HTMLCanvasElement.prototype.getContext;

  const ctx = new Proxy(
    {},
    { get: () => () => {} },
  ) as unknown as CanvasRenderingContext2D;

  Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
    configurable: true,
    value: () => ctx,
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  HTMLCanvasElement.prototype.getContext = origGetContext;
  cleanup();
});

describe("Chart", () => {
  it("mounts a uPlot instance for the given series", () => {
    render(<Chart label="throughput" x={xIdx} series={throughput()} />);
    const plot = document.querySelector(".uplot");
    expect(plot).toBeTruthy();
    expect(plot?.querySelector("canvas")).toBeTruthy();
  });

  it("renders a chart-empty shell, and no plot, when there is no series", () => {
    render(<Chart label="empty" x={xIdx} series={[]} />);
    expect(document.querySelector(".chart-empty")).toBeTruthy();
    expect(document.querySelector(".uplot")).toBeNull();
  });

  it("wires each series into the readout", () => {
    render(<Chart label="throughput" x={xIdx} series={throughput()} />);
    // uPlot renders the readout as a .u-legend table whose rows carry each
    // series' label, so a reader can tell the two series apart at a glance.
    const legend = document.querySelector(".u-legend");
    expect(legend).toBeTruthy();
    expect(legend?.textContent).toContain("decode");
    expect(legend?.textContent).toContain("prefill");
  });

  it("hides the sample-index row and never shows a Unix-epoch date", () => {
    render(<Chart label="throughput" x={xIdx} series={throughput()} />);
    // The x axis is a plain sample index, so uPlot would otherwise render it as a
    // legend row (the x-series). Disable that row's plot + legend with `show: false`,
    // which marks it `.u-off` (display:none). The pinned legend still reads the newest
    // sample for the y-series, and a time scale would instead format the index as a
    // Unix date ("1969-12-31 …"), which is what this pins against.
    const legend = document.querySelector(".u-legend");
    // The y-series rows are the only visible legend rows.
    const visible = legend!.querySelectorAll(".u-series:not(.u-off)");
    expect(visible).toHaveLength(2);
    expect(visible[0]!.textContent).toContain("decode");
    expect(visible[1]!.textContent).toContain("prefill");
    // The hidden x-series row carries the sample index, never a date.
    const xRow = legend!.querySelector(".u-series.u-off");
    expect(xRow?.textContent).toContain("119");
    expect(legend!.textContent).not.toMatch(/\d{4}-\d{2}-\d{2}/);
  });

  it("destroys the instance on unmount", () => {
    const { unmount } = render(<Chart label="throughput" x={xIdx} series={throughput()} />);
    expect(document.querySelector(".uplot")).toBeTruthy();
    expect(() => unmount()).not.toThrow();
    expect(document.querySelector(".uplot")).toBeNull();
  });
});
