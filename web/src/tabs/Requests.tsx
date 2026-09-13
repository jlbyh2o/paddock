/**
 * Requests — §5.9.
 *
 * The ring is fetched by sequence from `GET /api/requests`, gated on the snapshot's
 * `requests.last_seq` having moved, so a quiet engine costs no polling at all.
 *
 * `paused` is the daemon's display flag (§3.3 and §7.1): collection never stops, so
 * pausing only stops this table following the newest row, and unpausing catches up
 * from the held sequence with nothing lost.
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

export function Requests(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const feed = useRequestFeed(true, s.requests);
  const [follow, setFollow] = useState(true);
  const [showDetail, setShowDetail] = useState(true);
  const bodyRef = useRef<HTMLDivElement>(null);

  const selection = useSelection(feed.items, useCallback((r: RequestRecord) => String(r.seq), []));
  const record = selection.item;

  const paused = s.requests.paused;

  useEffect(() => {
    if (!follow || paused) return;
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [follow, paused, feed.items.length]);

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
            run(api.pauseRequests(!paused));
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
            <button type="button" className="btn" onClick={() => run(api.pauseRequests(!paused))}>
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
                    className={`row ${String(item.seq) === selection.id ? "selected" : ""}`}
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
