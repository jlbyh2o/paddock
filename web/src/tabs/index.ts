/**
 * The nine tabs, in the order the tab bar and the `1`–`9` keys use.
 *
 * `hints` is the footer line: the TUI's context-sensitive hints, kept as the same
 * words, with a visible control for each one somewhere on the tab.
 */

import { lazy } from "react";
import type { ComponentType } from "react";
import type { Snapshot } from "../api/types.ts";

/**
 * Each tab is split out, so first paint parses the chrome and the one screen being
 * looked at rather than all nine. The chunks are served from the same binary, so the
 * extra request is local and instant, and a tab already visited is cached.
 */
type TabComponent = ComponentType<{ snapshot: Snapshot }>;

const Dashboard = lazy(async () => ({
  default: (await import("./Dashboard.tsx")).Dashboard as TabComponent,
}));
const Models = lazy(async () => ({ default: (await import("./Models.tsx")).Models as TabComponent }));
const Hub = lazy(async () => ({ default: (await import("./Hub.tsx")).Hub as TabComponent }));
const Templates = lazy(async () => ({
  default: (await import("./Templates.tsx")).Templates as TabComponent,
}));
const Serve = lazy(async () => ({ default: (await import("./Serve.tsx")).Serve as TabComponent }));
const Cache = lazy(async () => ({ default: (await import("./Cache.tsx")).Cache as TabComponent }));
const Jobs = lazy(async () => ({ default: (await import("./Jobs.tsx")).Jobs as TabComponent }));
const Requests = lazy(async () => ({
  default: (await import("./Requests.tsx")).Requests as TabComponent,
}));
const Logs = lazy(async () => ({ default: (await import("./Logs.tsx")).Logs as TabComponent }));

export type TabId =
  | "dashboard"
  | "models"
  | "hub"
  | "templates"
  | "serve"
  | "cache"
  | "jobs"
  | "requests"
  | "logs";

export interface TabHint {
  k: string;
  what: string;
}

export interface TabDef {
  id: TabId;
  title: string;
  Component: TabComponent;
  hints: TabHint[];
}

export const TABS: TabDef[] = [
  {
    id: "dashboard",
    title: "Dashboard",
    Component: Dashboard,
    hints: [
      { k: "e", what: "start" },
      { k: "s", what: "stop" },
      { k: "S", what: "force-stop" },
      { k: "t", what: "smoke test" },
      { k: "r", what: "rescan" },
    ],
  },
  {
    id: "models",
    title: "Models",
    Component: Models,
    hints: [
      { k: "/", what: "filter" },
      { k: "Enter", what: "use" },
      { k: "s", what: "serve now" },
      { k: "c", what: "convert" },
      { k: "D", what: "delete" },
      { k: "r", what: "rescan" },
    ],
  },
  {
    id: "hub",
    title: "Hub",
    Component: Hub,
    hints: [
      { k: "/", what: "search" },
      { k: "Enter", what: "list files" },
      { k: "Space", what: "toggle file" },
      { k: "a / n", what: "all / none" },
      { k: "d", what: "download" },
      { k: "i", what: "install hf" },
    ],
  },
  {
    id: "templates",
    title: "Templates",
    Component: Templates,
    hints: [
      { k: "r", what: "repo" },
      { k: "f", what: "fetch" },
      { k: "a", what: "apply" },
      { k: "u", what: "restore built-in" },
      { k: "v", what: "verify" },
      { k: "D", what: "delete" },
    ],
  },
  {
    id: "serve",
    title: "Serve",
    Component: Serve,
    hints: [
      { k: "← →", what: "group" },
      { k: "Enter", what: "edit" },
      { k: "Space", what: "cycle" },
      { k: "x", what: "unset" },
      { k: "a", what: "plan" },
      { k: "S / P", what: "save / load profile" },
      { k: "g", what: "start" },
    ],
  },
  {
    id: "cache",
    title: "Cache",
    Component: Cache,
    hints: [
      { k: "← →", what: "±1%" },
      { k: "⇧← →", what: "±10%" },
      { k: "r", what: "reset pool" },
      { k: "R", what: "reset all" },
      { k: "a", what: "apply" },
    ],
  },
  {
    id: "jobs",
    title: "Jobs",
    Component: Jobs,
    hints: [
      { k: "b", what: "run bench" },
      { k: "x", what: "cancel" },
      { k: "X", what: "clear finished" },
    ],
  },
  {
    id: "requests",
    title: "Requests",
    Component: Requests,
    hints: [
      { k: "Enter", what: "detail" },
      { k: "f", what: "follow" },
      { k: "p", what: "pause" },
      { k: "c", what: "clear" },
    ],
  },
  {
    id: "logs",
    title: "Logs",
    Component: Logs,
    hints: [
      { k: "/", what: "filter" },
      { k: "e", what: "errors only" },
      { k: "w", what: "wrap" },
      { k: "f", what: "follow" },
      { k: "c", what: "clear" },
    ],
  },
];

export function tabFromHash(hash: string): TabId {
  const id = hash.replace(/^#\/?/, "");
  const found = TABS.find((tab) => tab.id === id);
  return found ? found.id : "dashboard";
}
