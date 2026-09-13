/**
 * Logs — §5.9.
 *
 * Lines come from `GET /api/logs` by sequence. Filtering, errors-only, wrapping and
 * following are all browser-side over what was fetched, exactly as the TUI computes
 * them over its own ring — including the errors rule (`err` or the text matching
 * ERROR / CRITICAL / Traceback / Exception) and the coloring rule (`[ft-man]` lines
 * are accents, `WARNING`/`WARN` is warn, `ready to serve` is good).
 *
 * When the ring dropped lines between two polls the store says how many, and that
 * elision is shown rather than silently closing the gap.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { LogLine, Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run, useLogFeed } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { Empty, Pane } from "../ui/primitives.tsx";
import { count } from "../format.ts";

/** The TUI's `is_error_text`. */
function isErrorText(text: string): boolean {
  return (
    text.includes("ERROR") ||
    text.includes("CRITICAL") ||
    text.includes("Traceback") ||
    text.includes("Exception")
  );
}

/** The TUI's `views::logs::line_style`. */
function lineClass(line: LogLine): string {
  if (line.text.startsWith("[ft-man]")) return "accent";
  if (line.err || isErrorText(line.text)) return "bad";
  if (line.text.includes("WARNING") || line.text.includes("WARN")) return "warn";
  if (line.text.includes("ready to serve")) return "ready";
  return "";
}

/** Enough of a window that a long ring stays cheap to render. */
const WINDOW = 800;

export function Logs(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const feed = useLogFeed(true, s.logs);
  const [filter, setFilter] = useState("");
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [wrap, setWrap] = useState(false);
  const [follow, setFollow] = useState(true);
  const filterRef = useRef<HTMLInputElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    let lines = feed.items;
    if (errorsOnly) lines = lines.filter((l) => l.err || isErrorText(l.text));
    if (needle !== "") lines = lines.filter((l) => l.text.toLowerCase().includes(needle));
    return lines;
  }, [feed.items, filter, errorsOnly]);

  const window = visible.length > WINDOW ? visible.slice(-WINDOW) : visible;
  const hidden = visible.length - window.length;

  useEffect(() => {
    if (!follow) return;
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [follow, window.length]);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (event.key === "Escape") {
          if (filter !== "") {
            setFilter("");
            filterRef.current?.blur();
            return true;
          }
          return false;
        }
        switch (event.key) {
          case "/":
            filterRef.current?.focus();
            filterRef.current?.select();
            return true;
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
          <input
            ref={filterRef}
            type="search"
            value={filter}
            placeholder="filter  (/)"
            onChange={(e) => setFilter(e.target.value)}
            aria-label="Filter log lines"
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
            <div key={line.seq} className={`logline ${lineClass(line)}`}>
              {line.text}
            </div>
          ))}
          {visible.length === 0 ? <div className="dim">nothing matches this filter</div> : null}
        </div>
      )}
    </Pane>
  );
}
