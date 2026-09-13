/**
 * Keyboard plumbing.
 *
 * The global keys (`1`–`9`, `?`, `Esc`) are handled in `App`; everything else is
 * the active tab's business, so a tab registers one handler and `App` calls it.
 * Only one tab is mounted at a time, so a module-level slot is enough and avoids
 * a context re-render on every keystroke.
 *
 * A letter key never fires while something is being typed into — that is the rule
 * that lets the TUI's single-letter bindings survive in a browser that has real
 * text fields.
 */

import { useEffect } from "react";

export type KeyHandler = (event: KeyboardEvent) => boolean;

let activeHandler: KeyHandler | null = null;

/** Register this tab's key handler for as long as it is mounted. */
export function useTabKeys(handler: KeyHandler): void {
  useEffect(() => {
    activeHandler = handler;
    return () => {
      if (activeHandler === handler) activeHandler = null;
    };
  }, [handler]);
}

/** Offer a key to the active tab. Returns true when the tab consumed it. */
export function dispatchTabKey(event: KeyboardEvent): boolean {
  return activeHandler ? activeHandler(event) : false;
}

/** True when the event landed in a text field, where letters mean letters. */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

/** True for a plain keypress: no modifier that would mean something else. */
export function isPlain(event: KeyboardEvent): boolean {
  return !event.ctrlKey && !event.metaKey && !event.altKey;
}
