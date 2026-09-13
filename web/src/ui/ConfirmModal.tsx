/**
 * The confirmation modal.
 *
 * `confirm.body` is rendered verbatim, blank lines included: it carries the numbers
 * the decision needs and the daemon composed it for exactly this purpose. The
 * affirmative option is styled dangerous when `destructive`, and both buttons post
 * to `/api/confirm` — which is the only thing that can advance a route that
 * returned `confirm_pending`.
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

  useEffect(() => {
    const target = confirm.default_index === 0 ? cancelRef.current : acceptRef.current;
    target?.focus();
  }, [confirm]);

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
          <button ref={cancelRef} type="button" className="btn" onClick={() => onAnswer(false)}>
            {cancelLabel} <span className="dim">(Esc)</span>
          </button>
          <button
            ref={acceptRef}
            type="button"
            className={`btn primary ${confirm.destructive ? "danger" : ""}`}
            onClick={() => onAnswer(true)}
          >
            {acceptLabel} <span className="dim">(Enter)</span>
          </button>
        </div>
      </div>
    </div>
  );
}
