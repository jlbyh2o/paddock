/**
 * The plan overlay (§5.6).
 *
 * Every number here was computed by `views::plan::build` on the daemon: the fit, its
 * verdict sentence, the reasons, which knobs would change. The browser lays them out
 * and offers the two things the TUI offers — apply, or dismiss.
 */

import type { ReactNode } from "react";
import type { Plan } from "../api/types.ts";

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
          {/*
            `ContextFit::verdict()` is the sentence itself — the same words the TUI
            prints — so the browser supplies only the label in front of it.
          */}
          <p>
            Context after this plan —{" "}
            {plan.fit ? plan.fit.verdict : "not enough measured information to say"}
          </p>
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
