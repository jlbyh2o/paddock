/**
 * Typed fetch helpers for every route in the web API.
 *
 * Rules this module encodes, from `docs/web-api.md` §1.2:
 *  - every non-2xx body is `{ "error": "...", "toasted": ... }`, so the envelope is
 *    parsed once here;
 *  - `toasted` says whether the daemon already pushed the same sentence as a toast.
 *    It is the daemon's word, not a guess from the status code: a refusal that belongs
 *    to one field carries `toasted: false` and is rendered against that field;
 *  - a 401 means auth expired; `onUnauthorized` takes the app back to the login page;
 *  - a 200 from a route marked "⚠ confirms" means "watch `snapshot.confirm`", never
 *    "done" — see `isConfirmPending`.
 *
 * Every POST body is typed as the `*Request` interface `types.ts` declares for that
 * route, so the compiler — not a review — checks the field names against the contract.
 */

import type {
  ActionReply,
  AuthReply,
  AuthStatus,
  CacheAdjustRequest,
  CachePendingReply,
  CachePendingRequest,
  ConfirmRequest,
  DownloadCancelRequest,
  EngineStopRequest,
  HubDownloadRequest,
  HubFileSelectReply,
  HubFileSelectRequest,
  HubFileToggleReply,
  HubFileToggleRequest,
  HubOpenRequest,
  HubSearchRequest,
  HubVariantRequest,
  JobCancelRequest,
  JobOutputPage,
  JobsClearReply,
  KnobSchema,
  LoginRequest,
  LogPage,
  ModelConvertRequest,
  ModelDeleteRequest,
  ModelSamplingRequest,
  ModelSamplingRevertRequest,
  ModelUseRequest,
  OkReply,
  ProfileDeleteRequest,
  ProfileLoadRequest,
  ProfileSaveReply,
  ProfileSaveRequest,
  RequestPage,
  RequestsPauseReply,
  RequestsPauseRequest,
  ServeCycleReply,
  ServeCycleRequest,
  ServeFlagReply,
  ServeFlagRequest,
  ServeKnobReply,
  ServeKnobRequest,
  ServePlanApplyReply,
  ServePlanReply,
  Snapshot,
  StartedReply,
  TemplateApplyRequest,
  TemplateDeleteRequest,
  TemplateFetchRequest,
  TemplateListRepoRequest,
  TemplatePreview,
  TemplateRevertRequest,
  TemplateVerifyRequest,
} from "./types.ts";

/**
 * True when the bundle was built to run against the in-browser fixture.
 *
 * `__PADDOCK_MOCK__` is replaced at build time (see `vite.config.ts`), so a production
 * build folds every mock branch away and the fixture never reaches the bundle.
 */
export const MOCK: boolean = __PADDOCK_MOCK__;

/** A non-2xx reply, with the envelope's sentence already unwrapped. */
export class ApiError extends Error {
  readonly status: number;
  /**
   * The daemon pushed this same sentence onto `app.toasts`, so it arrives in the next
   * snapshot and the browser must not raise a second problem of its own (§1.2).
   */
  readonly toasted: boolean;

  constructor(status: number, message: string, toasted = false) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.toasted = toasted;
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

/** The mock daemon answers a refusal with the same envelope the real one sends. */
function asErrorEnvelope(
  value: unknown,
): { error: string; status?: number; toasted?: boolean } | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  return typeof record["error"] === "string"
    ? {
        error: record["error"],
        status: typeof record["status"] === "number" ? record["status"] : undefined,
        toasted: record["toasted"] === true,
      }
    : null;
}

async function mockCall(method: string, path: string, body: unknown): Promise<unknown> {
  // Folded away entirely in a production build, which is what keeps the fixture and
  // the mock router out of `dist/`.
  if (!__PADDOCK_MOCK__) throw new ApiError(0, "this build carries no mock daemon");
  const mod = await import("../mock/server.ts");
  const reply = mod.handle(method, path, body);
  const envelope = asErrorEnvelope(reply);
  if (envelope) throw new ApiError(envelope.status ?? 409, envelope.error, envelope.toasted);
  return reply;
}

async function parseError(res: Response): Promise<ApiError> {
  let message = `${res.status} ${res.statusText}`.trim();
  let toasted = false;
  try {
    const parsed: unknown = await res.json();
    if (parsed && typeof parsed === "object") {
      const record = parsed as Record<string, unknown>;
      const envelope = record["error"];
      if (typeof envelope === "string" && envelope.length > 0) message = envelope;
      toasted = record["toasted"] === true;
    }
  } catch {
    // A proxy or a panic can produce a non-JSON body; the status line stands in, and
    // an unreadable body is by definition one no toast explained.
  }
  return new ApiError(res.status, message, toasted);
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
  login: (req: LoginRequest): Promise<AuthReply> => post("/api/login", req),
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
  pauseRequests: (req: RequestsPauseRequest): Promise<RequestsPauseReply> =>
    post("/api/requests/pause", req),
  clearRequests: (): Promise<OkReply> => post("/api/requests/clear", {}),
  jobOutput: (id: number, offset: number, limit = 65536): Promise<JobOutputPage> =>
    get(`/api/jobs/${id}/output?offset=${offset}&limit=${limit}`),

  // ---- templates preview (13) --------------------------------------------
  templatePreview: (name: string): Promise<TemplatePreview> =>
    get(`/api/templates/preview?name=${q(name)}`),

  // ---- confirmation (14) -------------------------------------------------
  confirm: (req: ConfirmRequest): Promise<ActionReply> => post("/api/confirm", req),

  // ---- engine (15-17) ----------------------------------------------------
  engineStart: (): Promise<ActionReply> => post("/api/engine/start", {}),
  /** ⚠ confirms. */
  engineStop: (req: EngineStopRequest): Promise<ActionReply> => post("/api/engine/stop", req),
  smokeTest: (): Promise<ActionReply> => post("/api/engine/smoke-test", {}),
  /** Ask the loaded model what the upstream commits change; the answer lands in the snapshot. */
  summarizeUpstream: (): Promise<StartedReply> =>
    post("/api/engine/summarize-upstream", {}),
  /** Pull the FreeToken checkout and reinstall it. Answers with a confirmation to accept. */
  updateFreetoken: (): Promise<ActionReply> => post("/api/freetoken/update", {}),

  // ---- models (18-21) ----------------------------------------------------
  rescanModels: (): Promise<StartedReply> => post("/api/models/rescan", {}),
  useModel: (req: ModelUseRequest): Promise<ActionReply> => post("/api/models/use", req),
  /** ⚠ confirms. */
  convertModel: (req: ModelConvertRequest): Promise<ActionReply> =>
    post("/api/models/convert", req),
  /** ⚠ confirms, destructively. */
  deleteModel: (req: ModelDeleteRequest): Promise<ActionReply> => post("/api/models/delete", req),
  /** ⚠ confirms. Writes the checkpoint's generation_config.json. */
  applySampling: (req: ModelSamplingRequest): Promise<ActionReply> =>
    post("/api/models/sampling/apply", req),
  /** ⚠ confirms. */
  revertSampling: (req: ModelSamplingRevertRequest): Promise<ActionReply> =>
    post("/api/models/sampling/revert", req),

  // ---- hub (22-29) -------------------------------------------------------
  hubSearch: (req: HubSearchRequest): Promise<StartedReply> => post("/api/hub/search", req),
  hubOpen: (req: HubOpenRequest): Promise<StartedReply> => post("/api/hub/open", req),
  hubVariant: (req: HubVariantRequest): Promise<OkReply> => post("/api/hub/variant", req),
  hubToggleFile: (req: HubFileToggleRequest): Promise<HubFileToggleReply> =>
    post("/api/hub/files/toggle", req),
  hubSelectFiles: (req: HubFileSelectRequest): Promise<HubFileSelectReply> =>
    post("/api/hub/files/select", req),
  hubDownload: (req: HubDownloadRequest): Promise<StartedReply> => post("/api/hub/download", req),
  /** ⚠ confirms. */
  hubInstallCli: (): Promise<ActionReply> => post("/api/hub/install-cli", {}),
  /** ⚠ confirms. */
  cancelDownload: (req: DownloadCancelRequest): Promise<ActionReply> =>
    post("/api/downloads/cancel", req),

  // ---- templates (30-35) -------------------------------------------------
  templatesListRepo: (req: TemplateListRepoRequest): Promise<StartedReply> =>
    post("/api/templates/list-repo", req),
  templatesFetch: (req: TemplateFetchRequest): Promise<StartedReply> =>
    post("/api/templates/fetch", req),
  /** ⚠ confirms. */
  templatesApply: (req: TemplateApplyRequest): Promise<ActionReply> =>
    post("/api/templates/apply", req),
  /** ⚠ confirms. */
  templatesRevert: (req: TemplateRevertRequest): Promise<ActionReply> =>
    post("/api/templates/revert", req),
  templatesVerify: (req: TemplateVerifyRequest): Promise<StartedReply> =>
    post("/api/templates/verify", req),
  /** ⚠ confirms, destructively. */
  templatesDelete: (req: TemplateDeleteRequest): Promise<ActionReply> =>
    post("/api/templates/delete", req),

  // ---- serve configuration (36-41) ---------------------------------------
  serveKnob: (req: ServeKnobRequest): Promise<ServeKnobReply> => post("/api/serve/knob", req),
  serveFlag: (req: ServeFlagRequest): Promise<ServeFlagReply> => post("/api/serve/flag", req),
  serveCycle: (req: ServeCycleRequest): Promise<ServeCycleReply> => post("/api/serve/cycle", req),
  servePlan: (): Promise<ServePlanReply> => post("/api/serve/plan", {}),
  servePlanApply: (): Promise<ServePlanApplyReply> => post("/api/serve/plan/apply", {}),
  servePlanDismiss: (): Promise<OkReply> => post("/api/serve/plan/dismiss", {}),

  // ---- profiles (42-44) --------------------------------------------------
  profileSave: (req: ProfileSaveRequest): Promise<ProfileSaveReply> =>
    post("/api/profiles/save", req),
  profileLoad: (req: ProfileLoadRequest): Promise<OkReply> => post("/api/profiles/load", req),
  /** ⚠ confirms, destructively. */
  profileDelete: (req: ProfileDeleteRequest): Promise<ActionReply> =>
    post("/api/profiles/delete", req),

  // ---- cache (45-48) -----------------------------------------------------
  cachePending: (req: CachePendingRequest): Promise<CachePendingReply> =>
    post("/api/cache/pending", req),
  cacheAdjust: (req: CacheAdjustRequest): Promise<CachePendingReply> =>
    post("/api/cache/adjust", req),
  cacheResetAll: (): Promise<OkReply> => post("/api/cache/reset-all", {}),
  /** ⚠ confirms. */
  cacheApply: (): Promise<ActionReply> => post("/api/cache/apply", {}),

  // ---- jobs (49-51) ------------------------------------------------------
  runBench: (): Promise<StartedReply> => post("/api/jobs/bench", {}),
  /** ⚠ confirms, destructively. */
  cancelJob: (req: JobCancelRequest): Promise<ActionReply> => post("/api/jobs/cancel", req),
  clearFinishedJobs: (): Promise<JobsClearReply> => post("/api/jobs/clear-finished", {}),
};

export type Api = typeof api;
