/**
 * The confirmation modal.
 *
 * `confirm.body` is rendered verbatim, blank lines included: it carries the numbers
 * the decision needs and the daemon composed it for exactly this purpose. The
 * affirmative option is styled dangerous when `destructive`, and both buttons post
 * to `/api/confirm` — which is the only thing that can advance a route that
 * returned `confirm_pending`.
 *
 * `data-confirm` marks which button is which so the global Enter binding can activate
 * whichever one has focus instead of assuming the affirmative.
 */

import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import type { Confirm } from "../api/types.ts";

export function ConfirmModal(props: {
  confirm: Confirm;
  onAnswer: (accept: boolean) => void;
}): ReactNode {
  const { confirm, onAnswer } = props;
  const cancelRef = useRef<HTMLButtonElement>(null);
  const acceptRef = useRef<HTMLButtonElement>(null);

  // A snapshot arrives up to ten times a second and each one carries a *new* `confirm`
  // object, so keying the focus effect on the object identity yanked the focus back to
  // the default option several times a second — a reader could not tab to the other
  // button. The identity that matters is what the modal says.
  const identity = `${confirm.title}\u0000${confirm.body.join("\u0000")}`;
  const defaultIndex = confirm.default_index;

  useEffect(() => {
    const target = defaultIndex === 0 ? cancelRef.current : acceptRef.current;
    // preventScroll for the same reason the help overlay does it: a long confirmation
    // body makes the overlay scrollable, and scrolling the foot into view would hide the
    // title and the first lines of what is about to happen.
    target?.focus({ preventScroll: true });
  }, [identity, defaultIndex]);

  const cancelLabel = confirm.options[0] ?? "Cancel";
  const acceptLabel = confirm.options[1] ?? "Confirm";

  return (
    <div className="overlay" role="presentation">
      <div className="dialog narrow" role="alertdialog" aria-modal="true" aria-label={confirm.title}>
        <div className={`dialog-head ${confirm.destructive ? "danger" : ""}`}>{confirm.title}</div>
        <div className="dialog-body">
          {confirm.body.map((line, i) =>
            line === "" ? (
              <p key={i} className="blank" />
            ) : (
              <p key={i}>{line}</p>
            ),
          )}
        </div>
        <div className="dialog-foot">
          <button
            ref={cancelRef}
            type="button"
            className="btn"
            data-confirm="cancel"
            onClick={() => onAnswer(false)}
          >
            {cancelLabel} <span className="dim">(Esc / n)</span>
          </button>
          <button
            ref={acceptRef}
            type="button"
            className={`btn primary ${confirm.destructive ? "danger" : ""}`}
            data-confirm="accept"
            onClick={() => onAnswer(true)}
          >
            {acceptLabel} <span className="dim">(y)</span>
          </button>
        </div>
      </div>
    </div>
  );
}
