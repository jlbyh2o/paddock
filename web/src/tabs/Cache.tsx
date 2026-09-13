/**
 * Cache — §5.7.
 *
 * Every number in the pool table was computed by `views::cache` on the daemon,
 * including the token-to-page conversions the engine's published limits need. The
 * browser does no pool arithmetic: a slider posts a value, an arrow posts a
 * percentage, and the next snapshot says what that meant.
 */

import { useCallback, useState } from "react";
import type { ReactNode } from "react";
import type { CachePoolRow, PoolId, Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { useSelection } from "../ui/useSelection.ts";
import { Bar, Empty, Field, Meter, Pane } from "../ui/primitives.tsx";
import { bytes, count, percent, signedBytes, signedCount } from "../format.ts";

export function Cache(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const cache = s.cache;
  const pools = cache.pools ?? [];
  const [dragging, setDragging] = useState<Partial<Record<PoolId, number>>>({});

  const selection = useSelection(pools, useCallback((row: CachePoolRow) => row.pool, []));
  const row = selection.item;

  const adjust = useCallback((pool: PoolId, percentStep: number) => {
    run(api.cacheAdjust(pool, percentStep));
  }, []);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (selection.handleKey(event)) return true;
        if (!row) return false;
        switch (event.key) {
          case "ArrowLeft":
            adjust(row.pool, event.shiftKey ? -0.1 : -0.01);
            return true;
          case "ArrowRight":
            adjust(row.pool, event.shiftKey ? 0.1 : 0.01);
            return true;
          case "r":
            run(api.cachePending(row.pool, null));
            return true;
          case "R":
            run(api.cacheResetAll());
            return true;
          case "a":
          case "Enter":
            run(api.cacheApply());
            return true;
          default:
            return false;
        }
      },
      [selection, row, adjust],
    ),
  );

  if (cache.pools === null) {
    return (
      <div className="panes">
        <Pane title="Pools">
          <Empty>
            <p>Cache geometry is only available while the engine is serving.</p>
            <p>
              Current status: <strong>{s.engine.status_text}</strong>.
            </p>
          </Empty>
        </Pane>
      </div>
    );
  }

  if (pools.length === 0) {
    return (
      <div className="panes">
        <Pane title="Pools">
          <Empty>
            <p>This model exposes no resizable pools.</p>
          </Empty>
        </Pane>
      </div>
    );
  }

  const title = cache.state === "rebuilding" ? "Pools (rebuilding…)" : cache.applying ? "Pools (applying…)" : "Pools";
  const disabled = cache.applying || cache.state === "rebuilding";

  return (
    <div className="panes wide">
      <Pane
        title={title}
        note={cache.has_pending ? "pending changes" : undefined}
        bodyClassName="flush"
        actions={
          <>
            <button
              type="button"
              className="btn"
              onClick={() => {
                if (row) run(api.cachePending(row.pool, null));
              }}
              disabled={!row || row.pending === null}
            >
              Reset pool <span className="dim">(r)</span>
            </button>
            <button
              type="button"
              className="btn"
              onClick={() => run(api.cacheResetAll())}
              disabled={!cache.has_pending}
            >
              Reset all <span className="dim">(R)</span>
            </button>
            <button
              type="button"
              className="btn primary"
              onClick={() => run(api.cacheApply())}
              disabled={!cache.has_pending || disabled}
            >
              Apply <span className="dim">(a)</span>
            </button>
          </>
        }
      >
        <ul className="rows">
          {pools.map((pool) => {
            const live = dragging[pool.pool] ?? pool.shown;
            const min = Math.max(pool.min ?? 1, 1);
            return (
              <li
                key={pool.pool}
                className={`row ${pool.pool === selection.id ? "selected" : ""}`}
                onClick={() => selection.select(pool.pool)}
              >
                <div className="row-main">
                  <span className="grow">{pool.label}</span>
                  <span className="nowrap">
                    {count(live)} {pool.unit}
                  </span>
                </div>
                <Bar ratio={pool.ratio} tone={pool.pending === null ? undefined : "warn"} />
                <div className="row-sub">
                  {pool.pending === null
                    ? null
                    : `was ${count(pool.current)} (${signedCount(pool.delta)})  ·  `}
                  max {count(pool.max)}
                  {pool.note ? `  ·  ${pool.note}` : ""}
                </div>
                <div className="inline-form" onClick={(e) => e.stopPropagation()}>
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => adjust(pool.pool, -0.1)}
                    disabled={disabled}
                    title="down 10%"
                  >
                    −10%
                  </button>
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => adjust(pool.pool, -0.01)}
                    disabled={disabled}
                    title="down 1%"
                  >
                    −1%
                  </button>
                  <input
                    type="range"
                    min={min}
                    max={pool.max}
                    value={live}
                    disabled={disabled}
                    aria-label={`${pool.label} in ${pool.unit}`}
                    style={{ flex: "1 1 120px" }}
                    onChange={(e) =>
                      setDragging((prev) => ({ ...prev, [pool.pool]: Number(e.target.value) }))
                    }
                    onPointerUp={(e) => {
                      run(api.cachePending(pool.pool, Number(e.currentTarget.value)));
                      setDragging((prev) => {
                        const next = { ...prev };
                        delete next[pool.pool];
                        return next;
                      });
                    }}
                    onKeyUp={(e) => {
                      run(api.cachePending(pool.pool, Number(e.currentTarget.value)));
                    }}
                  />
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => adjust(pool.pool, 0.01)}
                    disabled={disabled}
                    title="up 1%"
                  >
                    +1%
                  </button>
                  <button
                    type="button"
                    className="btn small"
                    onClick={() => adjust(pool.pool, 0.1)}
                    disabled={disabled}
                    title="up 10%"
                  >
                    +10%
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      </Pane>

      <Pane title="VRAM budget">
        <Field label="Current">{bytes(cache.current_bytes?.total)}</Field>
        <div className="facts">
          <span>KV {bytes(cache.current_bytes?.kv)}</span>
          <span>MoE {bytes(cache.current_bytes?.moe)}</span>
          <span>GDN {bytes(cache.current_bytes?.mamba)}</span>
          <span>SWA {bytes(cache.current_bytes?.swa)}</span>
        </div>
        {cache.proposed_bytes !== null ? (
          <>
            <Field label="Proposed" tone={cache.over_budget ? "bad" : undefined}>
              {bytes(cache.proposed_bytes)} ({signedBytes(cache.delta_bytes)})
            </Field>
            {cache.over_budget ? (
              <p className="bad">This exceeds the engine's cache budget and would be rejected.</p>
            ) : null}
          </>
        ) : null}
        {cache.budget_bytes ? (
          <Meter
            label="Budget"
            ratio={cache.budget_ratio}
            figure={`${percent(cache.budget_ratio)} of ${bytes(cache.budget_bytes)}`}
            tone={cache.over_budget ? "bad" : undefined}
          />
        ) : null}
        <div className="facts">
          {cache.facts.map((fact) => (
            <span key={fact}>{fact}</span>
          ))}
        </div>
        {cache.last_rebuild_summary ? (
          <Field label="Last rebuild">{cache.last_rebuild_summary}</Field>
        ) : null}
        <Field label="In flight">
          {count(cache.active_requests)} request{cache.active_requests === 1 ? "" : "s"}
        </Field>
        <p className="dim">
          {cache.active_requests > 0
            ? "A rebuild is rejected while requests are in flight; apply once they finish."
            : "Applying rebuilds the pools in place. Weights stay loaded."}
        </p>
      </Pane>
    </div>
  );
}
