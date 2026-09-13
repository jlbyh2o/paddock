/**
 * The small drawing vocabulary every tab is built from: a titled pane, a
 * label/value line, a meter, a sparkline, a progress bar.
 *
 * Meters and sparklines are inline SVG — no canvas, no chart library — and they
 * size themselves to their column, so a pane can be any width without the numbers
 * beside them moving.
 */

import type { ReactNode } from "react";
import type { Severity } from "../api/types.ts";
import { DASH } from "../format.ts";

// ---------------------------------------------------------------- pane

export function Pane(props: {
  title: ReactNode;
  note?: ReactNode;
  children: ReactNode;
  actions?: ReactNode;
  className?: string;
  bodyClassName?: string;
}): ReactNode {
  return (
    <section className={`pane ${props.className ?? ""}`}>
      <header className="pane-head">
        <span className="pane-title">{props.title}</span>
        {props.note !== undefined && props.note !== null ? (
          <span className="pane-note">{props.note}</span>
        ) : null}
      </header>
      <div className={`pane-body ${props.bodyClassName ?? ""}`}>{props.children}</div>
      {props.actions ? <div className="pane-actions">{props.actions}</div> : null}
    </section>
  );
}

// ---------------------------------------------------------------- text lines

export function Field(props: {
  label: ReactNode;
  children: ReactNode;
  tone?: Severity;
  mono?: boolean;
}): ReactNode {
  const cls = [props.tone ?? "", props.mono ? "mono" : ""].filter(Boolean).join(" ");
  return (
    <div className="field">
      <span className="label">{props.label}</span>
      <span className={`value ${cls}`}>{props.children}</span>
    </div>
  );
}

export function Dot(props: { tone: Severity; title?: string }): ReactNode {
  return <span className={`dot ${props.tone}`} title={props.title} aria-hidden="true" />;
}

export function Bullets(props: { items: { level: Severity; text: string }[] }): ReactNode {
  if (props.items.length === 0) return null;
  return (
    <ul className="bullets">
      {props.items.map((item, i) => (
        <li key={`${i}-${item.text}`} className={item.level}>
          <span>{item.text}</span>
        </li>
      ))}
    </ul>
  );
}

export function Empty(props: { children: ReactNode }): ReactNode {
  return <div className="empty">{props.children}</div>;
}

// ---------------------------------------------------------------- meters

function clampRatio(ratio: number | null | undefined): number {
  if (typeof ratio !== "number" || !Number.isFinite(ratio)) return 0;
  return Math.max(0, Math.min(1, ratio));
}

function toneColor(tone: Severity | undefined): string {
  switch (tone) {
    case "good":
      return "var(--good)";
    case "warn":
      return "var(--warn)";
    case "bad":
      return "var(--bad)";
    case "dim":
      return "var(--dim)";
    default:
      return "var(--gauge)";
  }
}

export function Bar(props: { ratio: number | null; tone?: Severity; height?: number }): ReactNode {
  const ratio = clampRatio(props.ratio);
  const height = props.height ?? 8;
  const known = props.ratio !== null && props.ratio !== undefined;
  return (
    <svg
      viewBox={`0 0 100 ${height}`}
      preserveAspectRatio="none"
      style={{ height }}
      role="img"
      aria-label={known ? `${Math.round(ratio * 100)} percent` : "progress unknown"}
    >
      <rect x="0" y="0" width="100" height={height} fill="var(--gauge-bg)" />
      {known ? (
        <rect x="0" y="0" width={ratio * 100} height={height} fill={toneColor(props.tone)} />
      ) : null}
    </svg>
  );
}

export function Meter(props: {
  label: ReactNode;
  ratio: number | null;
  figure: ReactNode;
  tone?: Severity;
}): ReactNode {
  return (
    <div className="meter">
      <span className="label">{props.label}</span>
      <Bar ratio={props.ratio} tone={props.tone} />
      <span className="figure">{props.figure}</span>
    </div>
  );
}

// ---------------------------------------------------------------- sparkline

export function Sparkline(props: {
  values: number[];
  height?: number;
  tone?: Severity;
  label: string;
}): ReactNode {
  const height = props.height ?? 28;
  const width = 240;
  const values = props.values;

  if (values.length === 0) {
    return (
      <div className="sparkline">
        <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" style={{ height }} role="img" aria-label={`${props.label}: no samples yet`}>
          <line x1="0" y1={height - 0.5} x2={width} y2={height - 0.5} stroke="var(--gauge-bg)" strokeWidth="1" />
        </svg>
      </div>
    );
  }

  let max = 0;
  for (const v of values) if (v > max) max = v;
  if (max <= 0) max = 1;

  const step = values.length > 1 ? width / (values.length - 1) : width;
  const points = values.map((v, i) => {
    const x = values.length > 1 ? i * step : width / 2;
    const y = height - (v / max) * (height - 1) - 0.5;
    return `${x.toFixed(2)},${y.toFixed(2)}`;
  });
  const line = points.join(" ");
  const area = `0,${height} ${line} ${width},${height}`;
  const color = toneColor(props.tone);

  return (
    <div className="sparkline">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        style={{ height }}
        role="img"
        aria-label={`${props.label}: ${values.length} samples, peak ${max}`}
      >
        <polygon points={area} fill={color} fillOpacity="0.16" />
        <polyline
          points={line}
          fill="none"
          stroke={color}
          strokeWidth="1.5"
          vectorEffect="non-scaling-stroke"
        />
      </svg>
    </div>
  );
}

// ---------------------------------------------------------------- misc

export function Maybe(props: { children: ReactNode | null | undefined }): ReactNode {
  const value = props.children;
  if (value === null || value === undefined || value === "") return <span className="dim">{DASH}</span>;
  return <>{value}</>;
}

export function Hint(props: { k: string; children: ReactNode }): ReactNode {
  return (
    <span className="hint">
      <kbd>{props.k}</kbd>
      {props.children}
    </span>
  );
}
