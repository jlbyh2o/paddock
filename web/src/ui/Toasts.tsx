/**
 * Toasts, bottom right, newest last (§5.1) — the corner the terminal uses too.
 *
 * Server toasts carry `age_ms` and `ttl_ms` (§2.15) so the browser can fade in step
 * with `App::expire_toasts` rather than running a timer of its own. Client-side
 * problems — a network failure, a 400 — are merged in from `localToastStore`; a
 * 409/503 refusal never appears here, because the daemon already sent its own.
 */

import type { ReactNode } from "react";
import type { Toast } from "../api/types.ts";
import { localToastStore, useStore } from "../api/store.ts";

const FADE_AT_MS = 800;

export function Toasts(props: { toasts: Toast[] }): ReactNode {
  const local = useStore(localToastStore);
  const now = Date.now();

  const rows = [
    ...props.toasts.map((t) => ({
      id: t.id,
      text: t.text,
      kind: t.kind,
      remaining: Math.max(0, t.ttl_ms - t.age_ms),
    })),
    ...local.map((t) => ({
      id: t.id,
      text: t.text,
      kind: t.kind,
      remaining: Math.max(0, t.ttl_ms - (now - t.at)),
    })),
  ].slice(-4);

  if (rows.length === 0) return null;

  return (
    <div className="toasts" role="status" aria-live="polite">
      {rows.map((row) => (
        <div
          key={row.id}
          className={`toast ${row.kind} ${row.remaining < FADE_AT_MS ? "fading" : ""}`}
        >
          {row.text}
        </div>
      ))}
    </div>
  );
}
