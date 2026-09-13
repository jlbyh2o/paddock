/**
 * Typed fetch helpers for every route in the web API.
 *
 * Rules this module encodes, from `docs/web-api.md` §1.2:
 *  - every non-2xx body is `{ "error": "..." }`, so the envelope is parsed once here;
 *  - a 409 or 503 is a *refusal*: the daemon has already pushed the same warn toast
 *    onto `app.toasts`, so the caller must not raise a second problem of its own.
 *    `ApiError.isRefusal` says so;
 *  - a 401 means auth expired; `onUnauthorized` takes the app back to the login page;
 *  - a 200 from a route marked "⚠ confirms" means "watch `snapshot.confirm`", never
 *    "done" — see `isConfirmPending`.
 */

import type {
  ActionReply,
  AuthReply,
  AuthStatus,
  CachePendingReply,
  HubDownloadRequest,
  HubFileSelectReply,
  HubFileToggleReply,
  JobOutputPage,
  JobsClearReply,
  KnobSchema,
  LogPage,
  OkReply,
  PoolId,
  ProfileSaveReply,
  RequestPage,
  RequestsPauseReply,
  ServeCycleReply,
  ServeFlagReply,
  ServeKnobReply,
  ServePlanApplyReply,
  ServePlanReply,
  Snapshot,
  StartedReply,
  TemplatePreview,
} from "./types.ts";

/** True when the bundle was built to run against the in-browser fixture. */
export const MOCK = import.meta.env.VITE_MOCK === "1";

/** A non-2xx reply, with the envelope's sentence already unwrapped. */
export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }

  /**
   * 409 and 503 are refusals the daemon has already explained with a toast.
   * Render one problem, not two.
   */
  get isRefusal(): boolean {
    return this.status === 409 || this.status === 503;
  }

  get isUnauthorized(): boolean {
    return this.status === 401;
  }
}

type UnauthorizedHandler = () => void;

let unauthorizedHandler: UnauthorizedHandler | null = null;

/** Register the callback that returns the app to the login page on a 401. */
export function onUnauthorized(handler: UnauthorizedHandler | null): void {
  unauthorizedHandler = handler;
}

function noteUnauthorized(): void {
  if (unauthorizedHandler) unauthorizedHandler();
}

async function mockCall(method: string, path: string, body: unknown): Promise<unknown> {
  const mod = await import("../mock/server.ts");
  return mod.handle(method, path, body);
}

async function parseError(res: Response): Promise<ApiError> {
  let message = `${res.status} ${res.statusText}`.trim();
  try {
    const parsed: unknown = await res.json();
    if (parsed && typeof parsed === "object" && "error" in parsed) {
      const envelope = (parsed as { error: unknown }).error;
      if (typeof envelope === "string" && envelope.length > 0) message = envelope;
    }
  } catch {
    // A proxy or a panic can produce a non-JSON body; the status line stands in.
  }
  return new ApiError(res.status, message);
}

async function call<T>(method: "GET" | "POST", path: string, body?: unknown): Promise<T> {
  if (MOCK) return (await mockCall(method, path, body ?? null)) as T;

  const init: RequestInit = {
    method,
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  };
  if (method === "POST") {
    init.headers = { ...init.headers, "Content-Type": "application/json" };
    init.body = JSON.stringify(body ?? {});
  }

  let res: Response;
  try {
    res = await fetch(path, init);
  } catch (cause) {
    throw new ApiError(0, cause instanceof Error ? cause.message : "the daemon is not reachable");
  }

  if (!res.ok) {
    const err = await parseError(res);
    if (err.isUnauthorized) noteUnauthorized();
    throw err;
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

const get = <T>(path: string): Promise<T> => call<T>("GET", path);
const post = <T>(path: string, body?: unknown): Promise<T> => call<T>("POST", path, body);

/** A `200` whose body says the action is waiting on `snapshot.confirm`. */
export function isConfirmPending(reply: ActionReply): boolean {
  return reply.status === "confirm_pending";
}

const q = encodeURIComponent;

/** Every route in §4.1, in the order the spec lists them. */
export const api = {
  // ---- auth (1-3) --------------------------------------------------------
  auth: (): Promise<AuthStatus> => get("/api/auth"),
  login: (token: string): Promise<AuthReply> => post("/api/login", { token }),
  logout: (): Promise<AuthReply> => post("/api/logout", {}),

  // ---- state (4-6) -------------------------------------------------------
  snapshot: (): Promise<Snapshot> => get("/api/snapshot"),
  knobs: (): Promise<KnobSchema> => get("/api/knobs"),

  // ---- incremental collections (7-12) ------------------------------------
  logs: (after: number, limit = 500): Promise<LogPage> =>
    get(`/api/logs?after=${after}&limit=${limit}`),
  clearLogs: (): Promise<OkReply> => post("/api/logs/clear", {}),
  requests: (after: number, limit = 200): Promise<RequestPage> =>
    get(`/api/requests?after=${after}&limit=${limit}`),
  pauseRequests: (paused: boolean): Promise<RequestsPauseReply> =>
    post("/api/requests/pause", { paused }),
  clearRequests: (): Promise<OkReply> => post("/api/requests/clear", {}),
  jobOutput: (id: number, offset: number, limit = 65536): Promise<JobOutputPage> =>
    get(`/api/jobs/${id}/output?offset=${offset}&limit=${limit}`),

  // ---- templates preview (13) --------------------------------------------
  templatePreview: (name: string): Promise<TemplatePreview> =>
    get(`/api/templates/preview?name=${q(name)}`),

  // ---- confirmation (14) -------------------------------------------------
  confirm: (accept: boolean): Promise<ActionReply> => post("/api/confirm", { accept }),

  // ---- engine (15-17) ----------------------------------------------------
  engineStart: (): Promise<ActionReply> => post("/api/engine/start", {}),
  /** ⚠ confirms. */
  engineStop: (force: boolean): Promise<ActionReply> => post("/api/engine/stop", { force }),
  smokeTest: (): Promise<ActionReply> => post("/api/engine/smoke-test", {}),

  // ---- models (18-21) ----------------------------------------------------
  rescanModels: (): Promise<StartedReply> => post("/api/models/rescan", {}),
  useModel: (path: string, andServe = false): Promise<ActionReply> =>
    post("/api/models/use", { path, and_serve: andServe }),
  /** ⚠ confirms. */
  convertModel: (path: string): Promise<ActionReply> => post("/api/models/convert", { path }),
  /** ⚠ confirms, destructively. */
  deleteModel: (path: string): Promise<ActionReply> => post("/api/models/delete", { path }),

  // ---- hub (22-29) -------------------------------------------------------
  hubSearch: (query: string): Promise<StartedReply> => post("/api/hub/search", { query }),
  hubOpen: (repoId: string, revision?: string): Promise<StartedReply> =>
    post("/api/hub/open", revision ? { repo_id: repoId, revision } : { repo_id: repoId }),
  hubVariant: (label: string): Promise<OkReply> => post("/api/hub/variant", { label }),
  hubToggleFile: (path: string, wanted?: boolean): Promise<HubFileToggleReply> =>
    post("/api/hub/files/toggle", wanted === undefined ? { path } : { path, wanted }),
  hubSelectFiles: (mode: "all" | "none"): Promise<HubFileSelectReply> =>
    post("/api/hub/files/select", { mode }),
  hubDownload: (req: HubDownloadRequest): Promise<StartedReply> => post("/api/hub/download", req),
  /** ⚠ confirms. */
  hubInstallCli: (): Promise<ActionReply> => post("/api/hub/install-cli", {}),
  /** ⚠ confirms. */
  cancelDownload: (id: number): Promise<ActionReply> => post("/api/downloads/cancel", { id }),

  // ---- templates (30-35) -------------------------------------------------
  templatesListRepo: (repo: string): Promise<StartedReply> =>
    post("/api/templates/list-repo", { repo }),
  templatesFetch: (repo: string, path: string, revision?: string): Promise<StartedReply> =>
    post("/api/templates/fetch", revision ? { repo, path, revision } : { repo, path }),
  /** ⚠ confirms. */
  templatesApply: (template: string, modelPath: string): Promise<ActionReply> =>
    post("/api/templates/apply", { template, model_path: modelPath }),
  /** ⚠ confirms. */
  templatesRevert: (modelPath: string): Promise<ActionReply> =>
    post("/api/templates/revert", { model_path: modelPath }),
  templatesVerify: (template: string, modelPath: string): Promise<StartedReply> =>
    post("/api/templates/verify", { template, model_path: modelPath }),
  /** ⚠ confirms, destructively. */
  templatesDelete: (name: string): Promise<ActionReply> =>
    post("/api/templates/delete", { name }),

  // ---- serve configuration (36-41) ---------------------------------------
  serveKnob: (key: string, value: string | null): Promise<ServeKnobReply> =>
    post("/api/serve/knob", { key, value }),
  serveFlag: (key: string, on?: boolean): Promise<ServeFlagReply> =>
    post("/api/serve/flag", on === undefined ? { key } : { key, on }),
  serveCycle: (key: string, delta: number): Promise<ServeCycleReply> =>
    post("/api/serve/cycle", { key, delta }),
  servePlan: (): Promise<ServePlanReply> => post("/api/serve/plan", {}),
  servePlanApply: (): Promise<ServePlanApplyReply> => post("/api/serve/plan/apply", {}),
  servePlanDismiss: (): Promise<OkReply> => post("/api/serve/plan/dismiss", {}),

  // ---- profiles (42-44) --------------------------------------------------
  profileSave: (name: string): Promise<ProfileSaveReply> => post("/api/profiles/save", { name }),
  profileLoad: (name: string): Promise<OkReply> => post("/api/profiles/load", { name }),
  /** ⚠ confirms, destructively. */
  profileDelete: (name: string): Promise<ActionReply> => post("/api/profiles/delete", { name }),

  // ---- cache (45-48) -----------------------------------------------------
  cachePending: (pool: PoolId, value: number | null): Promise<CachePendingReply> =>
    post("/api/cache/pending", { pool, value }),
  cacheAdjust: (pool: PoolId, percent: number): Promise<CachePendingReply> =>
    post("/api/cache/adjust", { pool, percent }),
  cacheResetAll: (): Promise<OkReply> => post("/api/cache/reset-all", {}),
  /** ⚠ confirms. */
  cacheApply: (): Promise<ActionReply> => post("/api/cache/apply", {}),

  // ---- jobs (49-51) ------------------------------------------------------
  runBench: (): Promise<StartedReply> => post("/api/jobs/bench", {}),
  /** ⚠ confirms, destructively. */
  cancelJob: (id: number): Promise<ActionReply> => post("/api/jobs/cancel", { id }),
  clearFinishedJobs: (): Promise<JobsClearReply> => post("/api/jobs/clear-finished", {}),
};

export type Api = typeof api;
