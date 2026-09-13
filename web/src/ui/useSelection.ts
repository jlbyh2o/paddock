/**
 * List selection, held by identity rather than index.
 *
 * §1.5: the server never accepts a list index, and a list can change between a
 * render and a click. Selecting by id means a row that moved is still the row that
 * was chosen, and a row that vanished falls back to the same position rather than
 * silently retargeting an action at its neighbor.
 */

import { useCallback, useMemo, useState } from "react";

export interface Selection<T> {
  id: string | null;
  item: T | null;
  index: number;
  select: (id: string | null) => void;
  move: (delta: number) => void;
  toStart: () => void;
  toEnd: () => void;
  /** `j k ↑ ↓ PgUp PgDn Home End G`. Returns true when the key was consumed. */
  handleKey: (event: KeyboardEvent) => boolean;
}

const PAGE = 10;

export function useSelection<T>(items: T[], idOf: (item: T) => string): Selection<T> {
  const [wanted, setWanted] = useState<string | null>(null);
  const [fallbackIndex, setFallbackIndex] = useState(0);

  const ids = useMemo(() => items.map(idOf), [items, idOf]);

  let index = wanted === null ? -1 : ids.indexOf(wanted);
  if (index < 0) index = items.length === 0 ? -1 : Math.min(fallbackIndex, items.length - 1);

  const id = index >= 0 ? (ids[index] ?? null) : null;
  const item = index >= 0 ? (items[index] ?? null) : null;

  const select = useCallback(
    (next: string | null) => {
      setWanted(next);
      const at = next === null ? 0 : ids.indexOf(next);
      if (at >= 0) setFallbackIndex(at);
    },
    [ids],
  );

  const moveTo = useCallback(
    (next: number) => {
      if (items.length === 0) return;
      const clamped = Math.max(0, Math.min(items.length - 1, next));
      setFallbackIndex(clamped);
      setWanted(ids[clamped] ?? null);
    },
    [items.length, ids],
  );

  const move = useCallback((delta: number) => moveTo(index + delta), [moveTo, index]);
  const toStart = useCallback(() => moveTo(0), [moveTo]);
  const toEnd = useCallback(() => moveTo(items.length - 1), [moveTo, items.length]);

  const handleKey = useCallback(
    (event: KeyboardEvent): boolean => {
      switch (event.key) {
        case "ArrowDown":
        case "j":
          move(1);
          return true;
        case "ArrowUp":
        case "k":
          move(-1);
          return true;
        case "PageDown":
          move(PAGE);
          return true;
        case "PageUp":
          move(-PAGE);
          return true;
        case "Home":
          toStart();
          return true;
        case "End":
        case "G":
          toEnd();
          return true;
        default:
          return false;
      }
    },
    [move, toStart, toEnd],
  );

  return { id, item, index, select, move, toStart, toEnd, handleKey };
}
