/**
 * Theme selection: follow the system by default, with a manual override kept in
 * `localStorage` so a chosen theme survives a reload.
 */

import { useCallback, useEffect, useState } from "react";

export type ThemeChoice = "system" | "light" | "dark";

const STORAGE_KEY = "ft-man.theme";

function read(): ThemeChoice {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === "light" || raw === "dark" || raw === "system") return raw;
  } catch {
    // Private mode, or storage disabled. The system preference still works.
  }
  return "system";
}

function apply(choice: ThemeChoice): void {
  const root = document.documentElement;
  if (choice === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", choice);
}

/** Apply the stored choice before React mounts, so there is no flash. */
export function initTheme(): void {
  apply(read());
}

export function useTheme(): { choice: ThemeChoice; cycle: () => void } {
  const [choice, setChoice] = useState<ThemeChoice>(read);

  useEffect(() => {
    apply(choice);
    try {
      localStorage.setItem(STORAGE_KEY, choice);
    } catch {
      // Nothing to do; the attribute is already set for this session.
    }
  }, [choice]);

  const cycle = useCallback(() => {
    setChoice((prev) => (prev === "system" ? "light" : prev === "light" ? "dark" : "system"));
  }, []);

  return { choice, cycle };
}

export const THEME_LABEL: Record<ThemeChoice, string> = {
  system: "theme: system",
  light: "theme: light",
  dark: "theme: dark",
};
