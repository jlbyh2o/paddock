/**
 * Requests — §5.9.
 *
 * The ring is fetched by sequence from `GET /api/requests`, gated on the snapshot's
 * `requests.last_seq` having moved, so a quiet engine costs no polling at all.
 *
 * `paused` is the daemon's display flag (§3.3 and §7.1): collection never stops, so
 * pausing only stops this table following the newest row, and unpausing catches up
 * from the held sequence with nothing lost.
 *
 * Following and selecting are one state, as they are in the TUI: while the table is
 * following, the row of interest is the newest one and the detail pane shows it;
 * touching a row (or an arrow key) stops the follow and pins the choice, and `f`
 * resumes it.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { RequestRecord, Severity, Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run, useRequestFeed } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { useSelection } from "../ui/useSelection.ts";
import { Empty, Field, Pane } from "../ui/primitives.tsx";
import { clock, count, decimal, ms, text, timestamp } from "../format.ts";

function statusTone(status: number): Severity {
  if (status >= 500) return "bad";
  if (status >= 400) return "warn";
  if (status >= 300) return "dim";
  return "good";
}

/** A status mapped to its palette color, so the trend line can carry status itself. */
function toneColor(tone: Severity): string {
  switch (tone) {
    case "good":
      return "var(--good)";
    case "warn":
      return "var(--warn)";
    case "bad":
      return "var(--bad)";
    default:
      return "var(--dim)";
  }
}

/**
 * A latency trend over the request ring: one point per request, joined by a segment
 * colored by the status of the request it leads to — green through 2xx, amber through
 * 4xx, red through 5xx — so an error streak or a slow-down reads at a glance above the
 * table. Points are laid out evenly by ring position, scaled to the slowest request.
 */
function LatencyTrend(props: { items: RequestRecord[] }): ReactNode {
  const width = 600;
  const height = 64;
  const pad = 6;
  const items = props.items;

  if (items.length === 0) {
    return <div className="trend"><div className="trend-empty">… no requests yet</div></div>;
  }

  const values = items.map((r) => r.duration_ms);
  const max = values.reduce((m, v) => (v > m ? v : m), 0);
  if (max <= 0) {
    return <div className="trend"><div className="trend-empty">… no latency yet</div></div>;
  }

  const n = values.length;
  const span = n > 1 ? width - 2 * pad : 0;
  const y = (v: number) => height - pad - (v / max) * (height - 2 * pad);

  const segments: ReactNode[] = [];
  let prev: { x: number; y: number } | null = null;
  items.forEach((r, i) => {
    const v = r.duration_ms;
    const px = n === 1 ? width / 2 : pad + (i / (n - 1)) * span;
    const py = y(v);
    if (prev) {
      segments.push(
        <line
          key={i}
          x1={prev.x}
          y1={prev.y}
          x2={px}
          y2={py}
          stroke={toneColor(statusTone(r.status))}
          strokeWidth={1.5}
          vectorEffect="non-scaling-stroke"
        />,
      );
    }
    prev = { x: px, y: py };
  });

  return (
    <div className="trend">
      <svg
        className="trend-chart"
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        style={{ height }}
        role="img"
        aria-label={`Latency trend · peak ${ms(max)}`}
      >
        <line
          x1={pad}
          y1={height - pad}
          x2={width - pad}
          y2={height - pad}
          stroke="var(--gauge-bg)"
          strokeWidth={1}
          vectorEffect="non-scaling-stroke"
        />
        {segments}
      </svg>
      <div className="trend-legend">
        <span className="trend-legend-item">
          <span className="dot good" aria-hidden /> ok
        </span>
        <span className="trend-legend-item">
          <span className="dot warn" aria-hidden /> 4xx
        </span>
        <span className="trend-legend-item">
          <span className="dot bad" aria-hidden /> 5xx
        </span>
      </div>
    </div>
  );
}
export function Requests(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const feed = useRequestFeed(true, s.requests);
  const [follow, setFollow] = useState(true);
  const [showDetail, setShowDetail] = useState(true);
  const bodyRef = useRef<HTMLDivElement>(null);

  const selection = useSelection(feed.items, useCallback((r: RequestRecord) => String(r.seq), []));
  const newest = feed.items[feed.items.length - 1] ?? null;
  // Following means "no explicit selection": the newest record is the one being read,
  // and `useSelection`'s fallback (the first row) would be the oldest one instead.
  const record = follow ? newest : selection.item;
  const selectedSeq = record === null ? null : String(record.seq);

  const paused = s.requests.paused;

  // The ring is capped at 512, so `items.length` stops moving once it is full; the
  // newest sequence is what still changes when a request lands.
  const newestSeq = newest?.seq ?? 0;

  // Keep the cursor under the row being read, so that the first arrow key steps away
  // from the newest entry rather than from the oldest one. Deliberately keyed on the
  // sequence alone: `selection` is a fresh object every render.
  const select = selection.select;
  useEffect(() => {
    if (follow && newestSeq > 0) select(String(newestSeq));
  }, [follow, newestSeq, select]);

  useEffect(() => {
    if (!follow || paused) return;
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [follow, paused, newestSeq]);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (selection.handleKey(event)) {
          setFollow(false);
          return true;
        }
        switch (event.key) {
          case "Enter":
            setShowDetail((prev) => !prev);
            return true;
          case "f":
            setFollow((prev) => !prev);
            return true;
          case "p":
            run(api.pauseRequests({ paused: !paused }));
            return true;
          case "c":
            run(api.clearRequests());
            return true;
          default:
            return false;
        }
      },
      [selection, paused],
    ),
  );

  return (
    <div className={showDetail ? "panes wide" : "panes"}>
      <Pane
        title="Requests"
        note={`${count(s.requests.count)}${paused ? " — paused" : ""}`}
        bodyClassName="flush"
        actions={
          <>
            <label className="toggle">
              <input
                type="checkbox"
                checked={follow}
                onChange={(e) => setFollow(e.target.checked)}
              />
              follow <span className="dim">(f)</span>
            </label>
            <button type="button" className="btn" onClick={() => run(api.pauseRequests({ paused: !paused }))}>
              {paused ? "Resume" : "Pause"} <span className="dim">(p)</span>
            </button>
            <button type="button" className="btn" onClick={() => run(api.clearRequests())}>
              Clear <span className="dim">(c)</span>
            </button>
            <label className="toggle">
              <input
                type="checkbox"
                checked={showDetail}
                onChange={(e) => setShowDetail(e.target.checked)}
              />
              detail <span className="dim">(Enter)</span>
            </label>
          </>
        }
      >
        {feed.gapDropped > 0 ? (
          <div className="gap-notice">… {count(feed.gapDropped)} entries dropped</div>
        ) : null}
        {feed.items.length === 0 ? (
          <Empty>
            {s.engine.server_reachable ? (
              <p>No requests yet. The ring fills as the engine serves.</p>
            ) : (
              <p>The engine is not answering, so there is nothing to read.</p>
            )}
          </Empty>
        ) : (
          <>
            <LatencyTrend items={feed.items} />
            <div className="table-wrap scroll h-560" ref={bodyRef}>
              <table className="grid">
                <thead>
                  <tr>
                    <th>time</th>
                    <th>method</th>
                    <th>path</th>
                    <th className="r">status</th>
                    <th className="r">latency</th>
                    <th className="r">TTFT</th>
                    <th className="r">in</th>
                    <th className="r">out</th>
                  </tr>
                </thead>
                <tbody>
                  {feed.items.map((item) => (
                    <tr
                      key={item.seq}
                      className={`row ${String(item.seq) === selectedSeq ? "selected" : ""}`}
                      onClick={() => {
                        setFollow(false);
                        selection.select(String(item.seq));
                      }}
                    >
                      <td className="mono">{clock(item.ts)}</td>
                      <td className="mono">{item.method}</td>
                      <td className="mono truncate">{item.path}</td>
                      <td className={`r ${statusTone(item.status)}`}>{item.status}</td>
                      <td className="r">{ms(item.duration_ms)}</td>
                      <td className="r">{ms(item.ttft_ms)}</td>
                      <td className="r">{count(item.prompt_tokens)}</td>
                      <td className="r">{count(item.completion_tokens)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </>
        )}
      </Pane>

      {showDetail ? (
        <Pane title="Detail">
          {!record ? (
            <Empty>
              <p>Select a request.</p>
            </Empty>
          ) : (
            <>
              <Field label="When">{timestamp(record.ts)}</Field>
              <Field label="Call" mono>
                {record.method} {record.path}
              </Field>
              <Field label="Status" tone={statusTone(record.status)}>
                {record.status}
              </Field>
              <Field label="Model">{text(record.model)}</Field>
              <Field label="Duration">{ms(record.duration_ms)}</Field>
              <Field label="TTFT">{ms(record.ttft_ms)}</Field>
              <Field label="Tokens in">{count(record.prompt_tokens)}</Field>
              <Field label="Tokens out">{count(record.completion_tokens)}</Field>
              <Field label="Decode rate">
                {record.decode_tps === null ? "—" : `${decimal(record.decode_tps)} tok/s`}
              </Field>
              <Field label="Streamed">{record.stream === null ? "—" : record.stream ? "yes" : "no"}</Field>
              {record.error ? (
                <Field label="Error" tone="bad">
                  {record.error}
                </Field>
              ) : null}
            </>
          )}
        </Pane>
      ) : null}
    </div>
  );
}
