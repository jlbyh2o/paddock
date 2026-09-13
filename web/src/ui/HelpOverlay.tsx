/**
 * The help overlay (`?` or F1), reproducing §5.11 — which is `src/ui/views/help.rs`
 * with the two corrections §7 records: nine tabs rather than eight, and `g` alone
 * for starting the engine from the Serve tab. The Hub's `i` is listed, which the
 * TUI's own page omits.
 *
 * The last section is the browser's own: `Tab` belongs to the browser's focus ring
 * here, so the pane-cycling the TUI spends it on is done by clicking or by the keys
 * named per tab, and quitting is closing the tab.
 */

import type { ReactNode } from "react";

interface Binding {
  keys: string;
  what: string;
}

const SECTIONS: { title: string; bindings: Binding[] }[] = [
  {
    title: "Global",
    bindings: [
      { keys: "1-9", what: "switch view" },
      { keys: "? or F1", what: "this help" },
      { keys: "/", what: "focus this tab's search or filter" },
      { keys: "Esc", what: "close an overlay, or cancel an edit" },
    ],
  },
  {
    title: "Dashboard",
    bindings: [
      { keys: "e", what: "start the engine with the current Serve configuration" },
      { keys: "s", what: "stop the engine" },
      { keys: "S", what: "force-stop the engine (SIGKILL)" },
      { keys: "t", what: "run a /generate smoke test" },
      { keys: "r", what: "rescan the model library" },
    ],
  },
  {
    title: "Models",
    bindings: [
      { keys: "↑ ↓ / j k", what: "move" },
      { keys: "/", what: "filter; Esc clears" },
      { keys: "Enter", what: "load into the Serve configuration" },
      { keys: "c", what: "convert to FTW" },
      { keys: "s", what: "serve this model now" },
      { keys: "D", what: "delete the checkpoint from disk" },
      { keys: "r", what: "rescan" },
    ],
  },
  {
    title: "Hub",
    bindings: [
      { keys: "/", what: "search" },
      { keys: "Enter", what: "list a repo's files" },
      { keys: "Space", what: "toggle a file" },
      { keys: "a / n", what: "select all / none" },
      { keys: "d", what: "download the selected files" },
      { keys: "i", what: "install the hf CLI when it is missing" },
    ],
  },
  {
    title: "Templates",
    bindings: [
      { keys: "r", what: "set the repo to browse" },
      { keys: "Enter", what: "list that repo's templates" },
      { keys: "f", what: "fetch the highlighted template" },
      { keys: "a", what: "apply to the selected model" },
      { keys: "u", what: "restore the model's built-in template" },
      { keys: "v", what: "check that it renders" },
      { keys: "D", what: "delete a stored template" },
    ],
  },
  {
    title: "Serve",
    bindings: [
      { keys: "↑ ↓", what: "move between knobs" },
      { keys: "← →", what: "switch knob group" },
      { keys: "Enter", what: "edit a value, or toggle a flag" },
      { keys: "Space", what: "cycle a choice knob" },
      { keys: "x / Del", what: "unset a knob, back to its default" },
      { keys: "a", what: "plan this serve for the hardware; A applies it" },
      { keys: "p", what: "show the resolved command line" },
      { keys: "S", what: "save as a profile" },
      { keys: "P", what: "load the selected profile" },
      { keys: "D", what: "delete the selected profile" },
      { keys: "g", what: "start the engine" },
    ],
  },
  {
    title: "Cache",
    bindings: [
      { keys: "↑ ↓", what: "select a pool" },
      { keys: "← →", what: "adjust by 1%" },
      { keys: "Shift + ← →", what: "adjust by 10%" },
      { keys: "r", what: "reset the selected pool" },
      { keys: "R", what: "reset every pending change" },
      { keys: "a", what: "apply the rebuild" },
    ],
  },
  {
    title: "Jobs",
    bindings: [
      { keys: "↑ ↓", what: "select" },
      { keys: "b", what: "run ft bench bw" },
      { keys: "x", what: "cancel the selected job" },
      { keys: "X", what: "clear finished entries" },
    ],
  },
  {
    title: "Requests",
    bindings: [
      { keys: "↑ ↓", what: "move" },
      { keys: "Enter", what: "toggle the detail pane" },
      { keys: "f", what: "follow the newest entry" },
      { keys: "p", what: "pause following" },
      { keys: "c", what: "clear" },
    ],
  },
  {
    title: "Logs",
    bindings: [
      { keys: "↑ ↓ / PgUp PgDn", what: "scroll" },
      { keys: "G / End", what: "jump to the tail and follow" },
      { keys: "f", what: "toggle follow" },
      { keys: "/", what: "filter" },
      { keys: "e", what: "errors only" },
      { keys: "w", what: "wrap long lines" },
      { keys: "c", what: "clear the buffer" },
    ],
  },
  {
    title: "In the browser",
    bindings: [
      { keys: "Tab", what: "moves the browser's focus ring, not the view" },
      { keys: "click", what: "every key here has a control beside it" },
      { keys: "close", what: "closing the tab leaves the engine running" },
    ],
  },
];

export function HelpOverlay(props: { onClose: () => void }): ReactNode {
  return (
    <div className="overlay" role="presentation" onClick={props.onClose}>
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Keyboard shortcuts"
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div className="dialog-head">Keys</div>
        <div className="dialog-body">
          <div className="help-grid">
            {SECTIONS.map((section) => (
              <div className="help-section" key={section.title}>
                <h3>{section.title}</h3>
                <dl>
                  {section.bindings.map((binding) => (
                    <div key={binding.keys} style={{ display: "contents" }}>
                      <dt>{binding.keys}</dt>
                      <dd>{binding.what}</dd>
                    </div>
                  ))}
                </dl>
              </div>
            ))}
          </div>
        </div>
        <div className="dialog-foot">
          <button type="button" className="btn" onClick={props.onClose} autoFocus>
            Close <span className="dim">(Esc)</span>
          </button>
        </div>
      </div>
    </div>
  );
}
