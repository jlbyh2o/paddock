/**
 * The one text field four tabs share: Models' filter, Logs' filter, the Hub's search
 * box and Templates' repo box.
 *
 * All four want the same three behaviors, which is why they are here once rather than
 * copied four times: `/` focuses the field and selects what is in it, `Escape` gives
 * the keyboard back to the tab (clearing the field first where the field is a filter),
 * and the input itself is a `type="search"` so a browser offers its own clear button.
 *
 * The value lives in the hook, so a tab reads `field.value` and never holds a second
 * copy of it.
 */

import { useCallback, useRef, useState } from "react";
import type { CSSProperties, ReactNode, RefObject } from "react";

export interface FilterField {
  /** What is in the box right now, verbatim. */
  value: string;
  /** What matching should use: trimmed once, here, rather than at each use. */
  needle: string;
  set: (next: string) => void;
  clear: () => void;
  /** Focus and select, as `/` does. */
  focus: () => void;
  /** `/` and `Escape`. Returns true when the key was consumed. */
  handleKey: (event: KeyboardEvent) => boolean;
  /** For `SearchField`; a tab never touches the element itself. */
  ref: RefObject<HTMLInputElement | null>;
}

export function useFilterField(
  initial = "",
  options: {
    /** Escape empties the field before blurring it. True for a filter, false for a form. */
    clearOnEscape?: boolean;
  } = {},
): FilterField {
  const clearOnEscape = options.clearOnEscape ?? true;
  const [value, setValue] = useState(initial);
  const ref = useRef<HTMLInputElement | null>(null);

  const focus = useCallback(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);

  const clear = useCallback(() => {
    setValue("");
  }, []);

  const handleKey = useCallback(
    (event: KeyboardEvent): boolean => {
      if (event.key === "Escape") {
        // Escape in a filter empties it; Escape in a form field only gives the
        // keyboard back, because the text there is the thing being composed.
        if (clearOnEscape && value !== "") {
          setValue("");
          ref.current?.blur();
          return true;
        }
        ref.current?.blur();
        return false;
      }
      if (event.key === "/") {
        focus();
        return true;
      }
      return false;
    },
    [clearOnEscape, focus, value],
  );

  return { value, needle: value.trim(), set: setValue, clear, focus, handleKey, ref };
}

export function SearchField(props: {
  field: FilterField;
  /** Include the key that focuses it, e.g. `"filter  (/)"`. */
  placeholder: string;
  /** The accessible name; the placeholder is not one. */
  label: string;
  /** A `<datalist>` id, for a field with suggestions. */
  list?: string;
  style?: CSSProperties;
}): ReactNode {
  const { field } = props;
  return (
    <input
      ref={field.ref}
      type="search"
      value={field.value}
      list={props.list}
      placeholder={props.placeholder}
      aria-label={props.label}
      style={props.style}
      onChange={(event) => {
        field.set(event.target.value);
      }}
    />
  );
}
