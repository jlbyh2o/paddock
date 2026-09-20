/**
 * The nine tabs, in the order the `1`–`9` keys and the sidebar use.
 *
 * Each tab belongs to a `group`; the sidebar renders them under the group's header so
 * nine reads as four. The per-tab `hints` that once fed the terminal's footer line live
 * now in the help overlay (`ui/HelpOverlay.tsx`), so they are gone from here.
 */

import { lazy } from "react";
import type { ComponentType, ReactNode } from "react";
import type { Snapshot } from "../api/types.ts";
import {
  IconCache,
  IconDashboard,
  IconHub,
  IconJobs,
  IconLogs,
  IconModels,
  IconRequests,
  IconServe,
  IconTemplates,
} from "../ui/icons.tsx";

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

/** The sidebar's four sections. Order here is the order the groups appear. */
export type TabGroup = "overview" | "library" | "serving" | "monitoring";

export interface GroupDef {
  id: TabGroup;
  title: string;
}

export const GROUPS: GroupDef[] = [
  { id: "overview", title: "Overview" },
  { id: "library", title: "Library" },
  { id: "serving", title: "Serving" },
  { id: "monitoring", title: "Monitoring" },
];

export interface TabDef {
  id: TabId;
  title: string;
  Component: TabComponent;
  group: TabGroup;
  icon: () => ReactNode;
}

export const TABS: TabDef[] = [
  {
    id: "dashboard",
    title: "Dashboard",
    Component: Dashboard,
    group: "overview",
    icon: IconDashboard,
  },
  {
    id: "models",
    title: "Models",
    Component: Models,
    group: "library",
    icon: IconModels,
  },
  {
    id: "hub",
    title: "Hub",
    Component: Hub,
    group: "library",
    icon: IconHub,
  },
  {
    id: "templates",
    title: "Templates",
    Component: Templates,
    group: "library",
    icon: IconTemplates,
  },
  {
    id: "serve",
    title: "Serve",
    Component: Serve,
    group: "serving",
    icon: IconServe,
  },
  {
    id: "cache",
    title: "Cache",
    Component: Cache,
    group: "serving",
    icon: IconCache,
  },
  {
    id: "jobs",
    title: "Jobs",
    Component: Jobs,
    group: "monitoring",
    icon: IconJobs,
  },
  {
    id: "requests",
    title: "Requests",
    Component: Requests,
    group: "monitoring",
    icon: IconRequests,
  },
  {
    id: "logs",
    title: "Logs",
    Component: Logs,
    group: "monitoring",
    icon: IconLogs,
  },
];

export function tabFromHash(hash: string): TabId {
  const id = hash.replace(/^#\/?/, "");
  const found = TABS.find((tab) => tab.id === id);
  return found ? found.id : "dashboard";
}
