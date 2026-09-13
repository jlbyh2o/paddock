/**
 * Logs — §5.9.
 *
 * Lines come from `GET /api/logs` by sequence, already classified: each carries the
 * `severity` `views::logs::classify` gave it, so the browser colors by that field and
 * never re-reads the text to guess. Filtering, errors-only, wrapping and following are
 * browser-side over what was fetched, exactly as the TUI computes them over its ring.
 *
 * When the ring dropped lines between two polls the store says how many, and that
 * elision is shown rather than silently closing the gap.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run, useLogFeed } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { Empty, Pane } from "../ui/primitives.tsx";
import { SearchField, useFilterField } from "../ui/SearchField.tsx";
import { count, severityClass } from "../format.ts";

/** Enough of a window that a long ring stays cheap to render. */
const WINDOW = 800;

export function Logs(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const feed = useLogFeed(true, s.logs);
  const filter = useFilterField();
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [wrap, setWrap] = useState(false);
  const [follow, setFollow] = useState(true);
  const bodyRef = useRef<HTMLDivElement>(null);

  const needle = filter.needle.toLowerCase();
  const visible = useMemo(() => {
    let lines = feed.items;
    // The TUI's errors-only rule, expressed through the two fields that carry it:
    // anything the daemon classified as an error, plus anything that came on stderr.
    if (errorsOnly) lines = lines.filter((l) => l.severity === "error" || l.err);
    if (needle !== "") lines = lines.filter((l) => l.text.toLowerCase().includes(needle));
    return lines;
  }, [feed.items, needle, errorsOnly]);

  const window = visible.length > WINDOW ? visible.slice(-WINDOW) : visible;
  const hidden = visible.length - window.length;
  // The window is capped, so its *length* stops changing once the ring is full; the
  // newest sequence is what actually moves when a line arrives.
  const newestSeq = window[window.length - 1]?.seq ?? 0;

  useEffect(() => {
    if (!follow) return;
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [follow, newestSeq]);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (filter.handleKey(event)) return true;
        if (event.key === "Escape") return false;
        switch (event.key) {
          case "f":
            setFollow((prev) => !prev);
            return true;
          case "e":
            setErrorsOnly((prev) => !prev);
            return true;
          case "w":
            setWrap((prev) => !prev);
            return true;
          case "c":
            run(api.clearLogs());
            return true;
          case "G":
          case "End":
            setFollow(true);
            return true;
          case "ArrowDown":
          case "j":
          case "ArrowUp":
          case "k":
          case "PageDown":
          case "PageUp": {
            const el = bodyRef.current;
            if (!el) return false;
            setFollow(false);
            const step =
              event.key === "PageDown" || event.key === "PageUp"
                ? el.clientHeight * 0.9
                : 20;
            const sign = event.key === "ArrowUp" || event.key === "k" || event.key === "PageUp" ? -1 : 1;
            el.scrollTop += sign * step;
            return true;
          }
          default:
            return false;
        }
      },
      [filter],
    ),
  );

  const note = [
    `${count(s.logs.count)} lines`,
    errorsOnly ? "— errors only" : null,
    follow ? null : "— paused",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <Pane
      title="Engine log"
      note={note}
      bodyClassName="flush"
      actions={
        <>
          <SearchField
            field={filter}
            placeholder="filter  (/)"
            label="Filter log lines"
            style={{ flex: "1 1 180px" }}
          />
          <label className="toggle">
            <input
              type="checkbox"
              checked={errorsOnly}
              onChange={(e) => setErrorsOnly(e.target.checked)}
            />
            errors only <span className="dim">(e)</span>
          </label>
          <label className="toggle">
            <input type="checkbox" checked={wrap} onChange={(e) => setWrap(e.target.checked)} />
            wrap <span className="dim">(w)</span>
          </label>
          <label className="toggle">
            <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
            follow <span className="dim">(f)</span>
          </label>
          <button type="button" className="btn" onClick={() => run(api.clearLogs())}>
            Clear <span className="dim">(c)</span>
          </button>
        </>
      }
    >
      {feed.items.length === 0 ? (
        <Empty>
          {s.logs.log_path ? (
            <p>
              No lines held. The engine's output is teed to{" "}
              <span className="mono">{s.logs.log_path}</span>.
            </p>
          ) : (
            <p>No engine has been started from this daemon, so there is no log to show.</p>
          )}
        </Empty>
      ) : (
        <div className={`loglist ${wrap ? "wrap" : ""}`} ref={bodyRef}>
          {feed.gapDropped > 0 ? (
            <div className="gap-notice">… {count(feed.gapDropped)} lines dropped</div>
          ) : null}
          {hidden > 0 ? (
            <div className="gap-notice">… {count(hidden)} earlier lines not rendered</div>
          ) : null}
          {window.map((line) => (
            <div key={line.seq} className={`logline ${severityClass(line.severity)}`}>
              {line.text}
            </div>
          ))}
          {visible.length === 0 ? <div className="dim">nothing matches this filter</div> : null}
        </div>
      )}
    </Pane>
  );
}
