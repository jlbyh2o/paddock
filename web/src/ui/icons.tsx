/**
 * The tab icons and the few UI glyphs the shell needs (collapse, theme, help).
 *
 * Hand-drawn inline SVG, one consistent voice: 16×16, stroke only, no fill, rounded
 * caps and joins. The visual language matches the rest of the web front end — no
 * component library, no icon package — so the collapsible icon rail earns its place
 * and the set stays a few hundred bytes. Colour and size come from the parent: `.icon`
 * sets the shared stroke, and each caller sizes the box.
 */

import type { ReactNode } from "react";

type Icon = () => ReactNode;

const svg = (props: { children: ReactNode; title?: string }): ReactNode => (
  <svg className="icon" viewBox="0 0 16 16" aria-hidden={props.title ? undefined : "true"} focusable="false">
    {props.title ? <title>{props.title}</title> : null}
    {props.children}
  </svg>
);

// --------------------------------------------------------------------- tabs

export const IconDashboard: Icon = () =>
  svg({
    children: (
      <>
        <rect x="2" y="2" width="5" height="5" rx="1" />
        <rect x="9" y="2" width="5" height="5" rx="1" />
        <rect x="2" y="9" width="5" height="5" rx="1" />
        <rect x="9" y="9" width="5" height="5" rx="1" />
      </>
    ),
    title: "Dashboard",
  });

export const IconModels: Icon = () =>
  svg({
    children: (
      <>
        <path d="M8 2.2 2.7 5.2v5.6L8 13.8l5.3-3V5.2z" />
        <path d="M2.7 5.2 8 8.2l5.3-3M8 8.2v5.6" />
      </>
    ),
    title: "Models",
  });

export const IconHub: Icon = () =>
  svg({
    children: <path d="M6.1 12.9a3.15 3.15 0 0 1-.19-6.3A4.4 4.4 0 0 1 14.4 6a3.15 3.15 0 0 1-1.3 6.9z" />,
    title: "Hub",
  });

export const IconTemplates: Icon = () =>
  svg({
    children: (
      <>
        <path d="M3.5 2h5.6l3.6 3.5v7.6H3.5z" />
        <path d="M9.1 2v3.5h3.5" />
        <path d="M6 9h4.5M6 11.5h4.5" />
      </>
    ),
    title: "Templates",
  });

export const IconServe: Icon = () =>
  svg({
    children: (
      <>
        <path d="M8 2.5c.8 1.4 1 3 .8 4.4L8 8.7l-.8-1.8c-.2-1.4 0-3 .8-4.4z" />
        <path d="M7 8.5 4.5 11l1.6.4.4-1.4z" />
        <path d="M9 8.5l2.5 2.5-1.6.4-.4-1.4z" />
        <path d="M6.9 12c.6.5 1.6.5 2.2 0" />
      </>
    ),
    title: "Serve",
  });

export const IconCache: Icon = () =>
  svg({
    children: (
      <>
        <path d="M4 4v8M12 4v8" />
        <path d="M4 4a4 1.6 0 0 0 8 0" />
        <path d="M4 4a4 1.6 0 0 1 8 0" />
        <path d="M4 12a4 1.6 0 0 0 8 0" />
      </>
    ),
    title: "Cache",
  });

export const IconJobs: Icon = () =>
  svg({
    children: (
      <>
        <path d="M5 4.5h6a.5.5 0 0 1 .5.5v8a.5.5 0 0 1-.5.5H5a.5.5 0 0 1-.5-.5V5a.5.5 0 0 1 .5-.5z" />
        <path d="M5 3h6" />
        <path d="M6.5 8.5 7.8 9.8 10 7.3" />
      </>
    ),
    title: "Jobs",
  });

export const IconRequests: Icon = () =>
  svg({
    children: (
      <>
        <path d="M3 8h10" />
        <path d="M11 5l4 3-4 3" />
        <path d="M13 8H3" />
        <path d="M5 11l-4-3 4-3" />
      </>
    ),
    title: "Requests",
  });

export const IconLogs: Icon = () =>
  svg({
    children: (
      <>
        <path d="M4 2h6l2 2v10H4z" />
        <path d="M6.5 6h5M6.5 9h5M6.5 12h3" />
      </>
    ),
    title: "Logs",
  });

// --------------------------------------------------------------------- shell

export const IconChevronLeft: Icon = () => svg({ children: <path d="M9.5 4.5 5.5 8l4 3.5" /> });
export const IconChevronRight: Icon = () =>
  svg({ children: <path d="M6.5 4.5l4 3.5-4 3.5" /> });

/** The theme switch: a sun, a moon, or a split glyph when following the system. */
export function ThemeGlyph(props: { choice: "system" | "light" | "dark" }): ReactNode {
  if (props.choice === "light") {
    return svg({
      title: "light theme",
      children: (
        <>
          <circle cx="8" cy="8" r="2.6" />
          <path d="M8 1.5v1.6M8 12.9v1.6M1.5 8h1.6M12.9 8h1.6M3.4 3.4l1.1 1.1M11.5 11.5l1.1 1.1M12.6 3.4l-1.1 1.1M4.5 11.5l-1.1 1.1" />
        </>
      ),
    });
  }
  if (props.choice === "dark") {
    return svg({
      title: "dark theme",
      children: <path d="M12.5 9.5A5.5 5.5 0 0 1 6.5 3.5 5.8 5.8 0 1 0 12.5 9.5z" />,
    });
  }
  return svg({
    title: "system theme",
    children: (
      <>
        <circle cx="8" cy="8" r="5.5" />
        <path d="M8 2.5A5.5 5.5 0 0 0 8 13.5z" />
      </>
    ),
  });
}

export const IconHelp: Icon = () =>
  svg({
    title: "Keys",
    children: (
      <>
        <circle cx="8" cy="8" r="5.8" />
        <path d="M6.1 6.3a1.9 1.9 0 0 1 3.8 0c0 1.1-.9 1.5-1.6 2.1-.3.3-.4.6-.4 1" />
        <path d="M8 11.2h.01" />
      </>
    ),
  });
