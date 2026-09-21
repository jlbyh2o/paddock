/**
 * Chart — a thin uPlot wrapper: one shared x axis, one or two y axes, a crosshair
 * and tooltip, and line or area series drawn in the shared palette.
 *
 * uPlot is the one justified dependency in the redesign (§6): a small, dependency-
 * free canvas chart built for exactly what the telemetry series are — 1–10 Hz time
 * series with multiple overlays, a crosshair and a live readout, smooth at width. The
 * compact sparklines under a field stay inline SVG (§4: meters and sparklines are
 * simple enough to hand-draw); this is for the charts that should be big.
 *
 * The instance is created once and its data is replaced in place on every frame —
 * recreating a canvas chart each snapshot would flicker and waste work.
 */

import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";

/** Which y-axis a series is measured against. */
export type ChartAxis = "left" | "right";

/** One y-series plotted against the shared x axis. */
export interface ChartSeries {
  /** Tooltip/legend label. */
  label: string;
  /** Y values, aligned with the chart's `x`. */
  values: number[];
  /** CSS stroke color for the line (and area fill). */
  stroke: string;
  /** Area fill under the line; omit for a bare line. */
  fill?: string;
  /** Which y-axis this series is measured against. */
  axis: ChartAxis;
}

export interface ChartProps {
  /** Accessible description; becomes the chart's aria-label. */
  label: string;
  /** Plot area height in CSS pixels. */
  height?: number;
  /** Shared x values — time in ms, or a plain index. */
  x: number[];
  /** One entry per y column, in order. */
  series: ChartSeries[];
  /** Formats an x tick. Defaults to the rounded value. */
  xFormat?: (value: number) => string;
  /** Formats a y tick for a given axis. Defaults to the value as-is. */
  yFormat?: (value: number, axis: ChartAxis) => string;
}

/** Default y-tick formatter: integers plain, otherwise one decimal. */
function fmtTick(v: number): string {
  return Number.isInteger(v) ? String(v) : v.toFixed(1);
}

/**
 * Build the uPlot option object. Pure: same inputs, same object, so the wrapper can
 * recreate the instance only when the things it captures (height, formatters) change
 * and swap data in place for everything else.
 *
 * uPlot's scale keys are `"y"` (left) and `"y2"` (right); the chart's own
 * `ChartAxis` names are `"left"`/`"right"`, so they are mapped here.
 */
function buildOpts(
  series: ChartSeries[],
  width: number,
  height: number,
  xFormat: ChartProps["xFormat"],
  yFormat: ChartProps["yFormat"],
): uPlot.Options {
  const hasRight = series.some((s) => s.axis === "right");

  const yOpts: uPlot.Series[] = series.map((s) => ({
    label: s.label,
    scale: s.axis === "left" ? "y" : "y2",
    stroke: s.stroke,
    width: 1.5,
    ...(s.fill ? { fill: s.fill, fillTo: 0 } : {}),
  }));

  const axes: uPlot.Axis[] = [
    {
      side: 2, // bottom (x)
      stroke: "var(--dim)",
      values: (_, splits) => splits.map((v) => (xFormat ?? fmtTick)(v)),
      grid: { stroke: "var(--border)", width: 1 },
    },
    {
      side: 3, // left (y)
      scale: "y",
      stroke: "var(--dim)",
      values: (_, splits) => splits.map(fmtTick),
      grid: { stroke: "var(--border)", width: 1 },
    },
  ];

  if (hasRight) {
    axes.push({
      side: 1, // right (y2)
      scale: "y2",
      stroke: "var(--dim)",
      values: (_, splits) => splits.map((v) => (yFormat ? yFormat(v, "right") : fmtTick(v))),
      grid: { show: false },
    });
  }

  return {
    width,
    height,
    series: [{}, ...yOpts],
    scales: hasRight ? { y2: {} } : undefined,
    padding: [height * 0.08, 0, 0, 0],
    axes,
    cursor: { x: true, y: false, points: { show: false } },
  };
}

export function Chart(props: ChartProps): ReactNode {
  const { label, height = 180, x, series, xFormat, yFormat } = props;
  const ref = useRef<HTMLDivElement>(null);
  const plotRef = useRef<uPlot | null>(null);

  // Nothing to draw without at least one series.
  if (series.length === 0) {
    return <div className="chart chart-empty" aria-label={label} />;
  }

  const yData = series.map((s) => s.values);

  // Create once; recreate only if the height or formatters change.
  useEffect(() => {
    const targ = ref.current!;
    const self = new uPlot(
      buildOpts(series, targ.clientWidth, height, xFormat, yFormat),
      [x, ...yData],
      targ,
    );
    plotRef.current = self;
    return () => {
      self.destroy();
      plotRef.current = null;
    };
    // opts captures series by closure, but data is swapped via setData, so the
    // create only needs the stable style inputs.
  }, [height, xFormat, yFormat]);

  // Replace data in place (new frame) without recreating the instance.
  useEffect(() => {
    const plot = plotRef.current;
    if (plot) plot.setData([x, ...yData]);
  }, [x, series]);

  return <div className="chart" ref={ref} aria-label={label} />;
}
