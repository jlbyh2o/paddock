/**
 * The plan overlay (§5.6).
 *
 * Every number here was computed by `views::plan::build` on the daemon: the fit, the
 * reasons, which knobs would change. The browser lays them out and offers the two
 * things the TUI offers — apply, or dismiss.
 */

import type { ReactNode } from "react";
import type { Plan } from "../api/types.ts";
import { percent, tokens } from "../format.ts";

function headline(plan: Plan): string {
  const fit = plan.fit;
  if (!fit) return "Context after this plan — not enough measured information to say";
  if (!fit.is_truncated) {
    return `Context after this plan — the full ${tokens(fit.ceiling)} this model offers`;
  }
  return `Context after this plan — ${tokens(fit.usable)} of the ${tokens(fit.ceiling)} this model offers (${percent(fit.ratio)})`;
}

export function PlanOverlay(props: {
  plan: Plan;
  onApply: () => void;
  onDismiss: () => void;
}): ReactNode {
  const { plan } = props;
  return (
    <div className="overlay" role="presentation">
      <div className="dialog" role="dialog" aria-modal="true" aria-label="Serve plan">
        <div className="dialog-head">Plan this serve</div>
        <div className="dialog-body">
          <p>{headline(plan)}</p>
          {plan.unpriced ? <p className="warn">{plan.unpriced}</p> : null}
          {plan.is_empty ? (
            <p className="dim">nothing to change — this configuration is already optimal here</p>
          ) : null}
          {plan.steps.map((step, i) => (
            <div className={`plan-step ${step.level}`} key={`${i}-${step.label}`}>
              <div className="label">{step.label}</div>
              <div className="reason">{step.reason}</div>
            </div>
          ))}
        </div>
        <div className="dialog-foot">
          <span className="dim" style={{ marginRight: "auto" }}>
            {plan.edit_count === 0
              ? "no knob would change"
              : `A applies ${plan.edit_count} change${plan.edit_count === 1 ? "" : "s"}`}
          </span>
          <button type="button" className="btn" onClick={props.onDismiss}>
            Dismiss <span className="dim">(Esc)</span>
          </button>
          <button
            type="button"
            className="btn primary"
            onClick={props.onApply}
            disabled={plan.edit_count === 0}
            autoFocus
          >
            Apply <span className="dim">(A)</span>
          </button>
        </div>
      </div>
    </div>
  );
}
