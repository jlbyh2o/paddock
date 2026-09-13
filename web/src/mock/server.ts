/**
 * A tiny in-browser stand-in for `ft-man web`, used when `VITE_MOCK=1`.
 *
 * It holds the fixture as mutable state, answers the routes `client.ts` calls,
 * and pushes a snapshot roughly ten times a second the way the real SSE stream
 * does. Actions that would touch the machine are no-ops that still produce the
 * toast, the confirmation or the value change the real daemon would, so every
 * screen can be driven end to end with no FreeToken anywhere.
 *
 * This module is loaded dynamically, so it never reaches a production bundle.
 */

import type {
  Confirm,
  JobOutputPage,
  LogLine,
  LogPage,
  PoolId,
  RequestPage,
  RequestRecord,
  SeqPage,
  Snapshot,
  Toast,
  ToastKind,
} from "../api/types.ts";
import { fixture, mockJobOutput, mockKnobs, mockLogLines, mockRequestRecords } from "./fixture.ts";

type Subscriber = (snapshot: Snapshot) => void;

const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;

let state: Snapshot = clone(fixture);
const subscribers = new Set<Subscriber>();
let ticker: ReturnType<typeof setInterval> | null = null;
let toastId = 100;

function emit(): void {
  state = { ...state, seq: state.seq + 1, ts_ms: Date.now() };
  for (const subscriber of subscribers) subscriber(state);
}

function toast(text: string, kind: ToastKind = "info"): void {
  toastId += 1;
  const ttl = kind === "error" ? 12_000 : kind === "warn" ? 8000 : 4000;
  const entry: Toast = { id: toastId, text, kind, age_ms: 0, ttl_ms: ttl };
  state.toasts = [...state.toasts, entry].slice(-4);
  emit();
}

function ask(confirm: Confirm): { status: "confirm_pending" } {
  state.confirm = confirm;
  emit();
  return { status: "confirm_pending" };
}

function tick(): void {
  const s = state;
  // Age and expire toasts the way `App::expire_toasts` does.
  s.toasts = s.toasts
    .map((t) => ({ ...t, age_ms: t.age_ms + 250 }))
    .filter((t) => t.age_ms < t.ttl_ms);

  // Nudge the series so the sparklines move.
  const push = (arr: number[], next: number): number[] => [...arr.slice(1), Math.max(0, next)];
  const jitter = (base: number, swing: number) => Math.round(base + (Math.random() - 0.5) * swing);
  s.series = {
    ...s.series,
    decode_tps: push(s.series.decode_tps, jitter(48, 14)),
    prefill_tps: push(s.series.prefill_tps, jitter(3100, 900)),
    gpu_util: push(s.series.gpu_util, Math.min(100, jitter(92, 12))),
    vram: push(s.series.vram, jitter(14_240, 200)),
    active: push(s.series.active, Math.max(0, jitter(2, 3))),
  };

  // Advance the download and the conversion.
  const download = s.jobs.downloads[0];
  if (download && download.is_running) {
    const done = Math.min(download.total_bytes, download.done_bytes + download.rate_bps / 4);
    download.done_bytes = done;
    download.ratio = done / download.total_bytes;
    download.elapsed_s += 0.25;
    download.eta_s =
      download.rate_bps > 1 ? Math.round((download.total_bytes - done) / download.rate_bps) : null;
    if (done >= download.total_bytes) {
      download.is_running = false;
      download.status = { kind: "done" };
      download.status_label = "done";
      download.eta_s = null;
      s.engine.active_downloads = 0;
    }
  }
  const job = s.jobs.items.find((j) => j.is_running);
  if (job) {
    job.progress.done = Math.min(job.progress.total, job.progress.done + job.rate_bps / 4);
    job.progress_ratio = job.progress.done / job.progress.total;
    job.elapsed_s += 0.25;
  }
  emit();
}

/** Subscribe to the snapshot stream. The first frame arrives immediately. */
export function subscribe(callback: Subscriber): () => void {
  subscribers.add(callback);
  callback(state);
  if (ticker === null) ticker = setInterval(tick, 250);
  return () => {
    subscribers.delete(callback);
    if (subscribers.size === 0 && ticker !== null) {
      clearInterval(ticker);
      ticker = null;
    }
  };
}

/** Reset to the pristine fixture. Used between tests. */
export function reset(): void {
  state = clone(fixture);
}

// ---------------------------------------------------------------- helpers

function page<T extends { seq: number }>(all: T[], after: number, limit: number): SeqPage<T> {
  const first = all[0];
  const last = all[all.length - 1];
  const items = all.filter((item) => item.seq > after).slice(0, limit);
  return {
    items,
    first_seq: first ? first.seq : 0,
    last_seq: last ? last.seq : 0,
    dropped: first ? first.seq - 1 : 0,
    next_after: last ? last.seq : after,
  };
}

function recount(): void {
  const counts: Record<string, number> = {
    model: 0,
    server: 0,
    runtime: 0,
    memory: 0,
    moe: 0,
    api: 0,
  };
  for (const knob of mockKnobs.knobs) {
    if (state.serve.values[knob.key] !== undefined) counts[knob.group] = (counts[knob.group] ?? 0) + 1;
  }
  state.serve.set_counts = counts as Snapshot["serve"]["set_counts"];
  const parts = ["ft serve"];
  for (const knob of mockKnobs.knobs) {
    const value = state.serve.values[knob.key];
    if (value === undefined) continue;
    parts.push(knob.kind.kind === "flag" ? knob.flag : `${knob.flag} ${value}`);
  }
  state.serve.command_preview = parts.join(" ");
}

function poolRow(pool: PoolId) {
  return state.cache.pools?.find((row) => row.pool === pool) ?? null;
}

function repricePools(): void {
  const pools = state.cache.pools;
  if (!pools) return;
  state.cache.has_pending = pools.some((row) => row.pending !== null);
  for (const row of pools) {
    row.shown = row.pending ?? row.current;
    row.delta = row.pending === null ? null : row.pending - row.current;
    row.ratio = Math.min(1, row.shown / Math.max(row.max, row.current, 1));
  }
  const unit: Record<PoolId, number> = {
    moe: 1_179_648,
    kv: 16 * 49_152,
    mamba: 262_144,
    swa: 16 * 12_288,
  };
  if (!state.cache.has_pending) {
    state.cache.proposed_bytes = null;
    state.cache.delta_bytes = null;
  } else {
    const proposed = pools.reduce((sum, row) => sum + row.shown * unit[row.pool], 0);
    state.cache.proposed_bytes = proposed;
    state.cache.delta_bytes = proposed - (state.cache.current_bytes?.total ?? 0);
    state.cache.over_budget =
      (state.cache.budget_bytes ?? 0) > 0 && proposed > (state.cache.budget_bytes ?? 0);
    state.cache.budget_ratio = state.cache.budget_bytes
      ? Math.min(1, proposed / state.cache.budget_bytes)
      : null;
  }
}

const requestRing: RequestRecord[] = clone(mockRequestRecords);
const logRing: LogLine[] = clone(mockLogLines);

// ---------------------------------------------------------------- the router

/** Answer one request. Throws nothing: the mock daemon never refuses. */
export function handle(method: string, rawPath: string, body: unknown): unknown {
  const url = new URL(rawPath, "http://mock.local");
  const path = url.pathname;
  const num = (key: string, fallback: number): number => {
    const raw = url.searchParams.get(key);
    const parsed = raw === null ? NaN : Number(raw);
    return Number.isFinite(parsed) ? parsed : fallback;
  };
  const payload = (body ?? {}) as Record<string, unknown>;
  const str = (key: string): string => String(payload[key] ?? "");

  if (method === "GET") {
    switch (path) {
      case "/api/auth":
        return { auth_required: false, authorized: true };
      case "/api/snapshot":
        return state;
      case "/api/knobs":
        return mockKnobs;
      case "/api/logs":
        return page(logRing, num("after", 0), num("limit", 500)) satisfies LogPage;
      case "/api/requests":
        return page(requestRing, num("after", 0), num("limit", 200)) satisfies RequestPage;
      case "/api/templates/preview": {
        const name = url.searchParams.get("name") ?? "";
        const stored = state.templates.stored.find((t) => t.name === name);
        return {
          name,
          text: stored
            ? `{# template_version: ${stored.meta.version ?? "?"} #}\n{%- for message in messages %}\n{{ message.role }}: {{ message.content }}\n{%- endfor %}`
            : "",
          truncated: false,
        };
      }
      default:
        break;
    }
    const output = /^\/api\/jobs\/(\d+)\/output$/.exec(path);
    if (output) {
      const id = Number(output[1]);
      const lines = mockJobOutput[id] ?? [];
      const offset = num("offset", 0);
      const fresh = offset > 0 ? [] : lines;
      return {
        id,
        offset,
        next_offset: offset + fresh.join("\n").length,
        eof: true,
        truncated: false,
        lines: fresh,
      } satisfies JobOutputPage;
    }
    return {};
  }

  switch (path) {
    case "/api/login":
      return { authorized: true };
    case "/api/logout":
      return { authorized: false };

    case "/api/confirm": {
      const accepted = payload["accept"] === true;
      const pending = state.confirm;
      state.confirm = null;
      if (accepted && pending) toast(`${pending.title} — done`, "success");
      emit();
      return { status: "ok" };
    }

    case "/api/engine/start":
      toast("an engine is already running; stop it first", "warn");
      return { status: "ok" };
    case "/api/engine/stop":
      return ask({
        title: payload["force"] === true ? "Force-stop the engine" : "Stop the engine",
        body: [
          `Qwen3.6-35B-A3B is serving on http://127.0.0.1:1919.`,
          "",
          payload["force"] === true
            ? "SIGKILL is sent immediately; the model is dropped without a clean shutdown."
            : "SIGINT is sent first, then SIGTERM and SIGKILL if it does not exit.",
        ],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: payload["force"] === true,
        action: { kind: "stop_engine", force: payload["force"] === true },
      });
    case "/api/engine/smoke-test":
      toast("smoke test: “Paris.” in 384 ms", "success");
      return { status: "started" };

    case "/api/models/rescan":
      toast("rescanning the library", "info");
      return { status: "started" };
    case "/api/models/use":
      state.serve.values["model"] = str("path");
      recount();
      toast("loaded into the Serve configuration", "success");
      return { status: "ok" };
    case "/api/models/convert":
      return ask({
        title: "Convert anyway?",
        body: ["The checkpoint check reported a caution.", "", "Converting writes to " + state.models.items[0]?.ftw_output_path],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: false,
        action: { kind: "convert_anyway", source: str("path") },
      });
    case "/api/models/delete":
      return ask({
        title: "Delete checkpoint",
        body: [str("path"), "", "21.0 GiB will be freed. This cannot be undone."],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: true,
        action: { kind: "delete_model", path: str("path") },
      });

    case "/api/hub/search":
      state.hub.query = str("query");
      toast(`searching the Hub for “${state.hub.query}”`, "info");
      return { status: "started" };
    case "/api/hub/open":
      state.hub.revision = str("revision") || state.hub.revision;
      toast(`listing ${str("repo_id")}`, "info");
      return { status: "started" };
    case "/api/hub/variant": {
      const label = str("label");
      const variant = state.hub.layout?.variants.find((v) => v.label === label);
      if (variant) {
        const shared = new Set(state.hub.layout?.shared ?? []);
        const wanted = new Set(variant.files);
        state.hub.files = state.hub.files.map((f) => ({
          ...f,
          wanted: wanted.has(f.path) || shared.has(f.path),
        }));
        state.hub.variant = label;
        state.hub.custom_selection = false;
        recountHubSelection();
        toast(`${label}: ${variant.file_count} file(s)`, "info");
      }
      return { status: "ok" };
    }
    case "/api/hub/files/toggle": {
      const target = str("path");
      let wanted = false;
      state.hub.files = state.hub.files.map((f) => {
        if (f.path !== target) return f;
        wanted = typeof payload["wanted"] === "boolean" ? (payload["wanted"] as boolean) : !f.wanted;
        return { ...f, wanted };
      });
      state.hub.custom_selection = true;
      recountHubSelection();
      return { status: "ok", wanted };
    }
    case "/api/hub/files/select": {
      const all = str("mode") === "all";
      state.hub.files = state.hub.files.map((f) => ({ ...f, wanted: all }));
      state.hub.custom_selection = true;
      recountHubSelection();
      return { status: "ok", selected_count: state.hub.selected_count };
    }
    case "/api/hub/download":
      toast("download started", "success");
      return { status: "started" };
    case "/api/hub/install-cli":
      return ask({
        title: "Install the Hugging Face CLI",
        body: [state.hub.hf_install_command, "", "This runs Hugging Face's own installer script."],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: false,
        action: { kind: "install_hf_cli" },
      });
    case "/api/downloads/cancel":
      return ask({
        title: "Cancel download",
        body: ["Completed files are kept and a partial file resumes."],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: false,
        action: { kind: "cancel_download", id: Number(payload["id"] ?? 0) },
      });

    case "/api/templates/list-repo":
      state.templates.repo = str("repo");
      toast(`listing ${state.templates.repo}`, "info");
      return { status: "started" };
    case "/api/templates/fetch":
      toast(`fetched ${str("path")}`, "success");
      return { status: "started" };
    case "/api/templates/apply":
      return ask({
        title: "Apply chat template",
        body: [
          `${str("template")} → ${str("model_path")}`,
          "",
          "Writes chat_template.jinja into the checkpoint directory.",
          "Restart the engine for it to take effect.",
        ],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: false,
        action: { kind: "apply_template", template: str("template"), model: str("model_path") },
      });
    case "/api/templates/revert":
      return ask({
        title: "Restore built-in template",
        body: [str("model_path")],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: false,
        action: { kind: "revert_template", model: str("model_path") },
      });
    case "/api/templates/verify":
      toast("render check queued", "info");
      return { status: "started" };
    case "/api/templates/delete":
      return ask({
        title: "Delete template",
        body: [
          str("name"),
          "",
          "Checkpoints it was already applied to keep using it.",
        ],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: true,
        action: { kind: "delete_template", name: str("name") },
      });

    case "/api/serve/knob": {
      const key = str("key");
      const raw = payload["value"];
      if (raw === null || raw === "") {
        delete state.serve.values[key];
        recount();
        emit();
        return { status: "ok", set: false, cleared: [] };
      }
      const knob = mockKnobs.knobs.find((k) => k.key === key);
      const cleared = (knob?.exclusive_with ?? []).filter((other) => other !== key);
      for (const other of cleared) delete state.serve.values[other];
      state.serve.values[key] = String(raw);
      recount();
      emit();
      return { status: "ok", set: true, cleared };
    }
    case "/api/serve/flag": {
      const key = str("key");
      const on =
        typeof payload["on"] === "boolean"
          ? (payload["on"] as boolean)
          : state.serve.values[key] === undefined;
      if (on) state.serve.values[key] = "true";
      else delete state.serve.values[key];
      recount();
      emit();
      return { status: "ok", on };
    }
    case "/api/serve/cycle": {
      const key = str("key");
      const knob = mockKnobs.knobs.find((k) => k.key === key);
      const delta = Number(payload["delta"] ?? 1);
      if (!knob || knob.kind.kind !== "choice") {
        emit();
        return { status: "ok", value: state.serve.values[key] ?? null };
      }
      const options = knob.kind.options;
      const current = state.serve.values[key];
      const index = current === undefined ? -1 : options.indexOf(current);
      const next = index + delta;
      let value: string | null;
      if (next < 0 || next >= options.length) value = null;
      else value = options[next] ?? null;
      if (value === null) delete state.serve.values[key];
      else state.serve.values[key] = value;
      recount();
      emit();
      return { status: "ok", value };
    }
    case "/api/serve/plan":
      state.serve.plan = {
        steps: [
          {
            level: "advice",
            label: "--kv-reserve-tokens 262144",
            key: "kv_reserve_tokens",
            value: "262144",
            reason:
              "The checkpoint advertises 256k but KV currently holds 64k. Reserving the full context before --moe-cache-auto spends VRAM on experts keeps the advertised window usable.",
          },
          {
            level: "advice",
            label: "--moe-strategy hybrid",
            key: "moe_strategy",
            value: "hybrid",
            reason:
              "The bandwidth profile measured 63.8 GB/s on the CPU NVFP4 kernel against 31.2 GB/s of PCIe gather, a ratio of 2.04 against a 1.25 threshold.",
          },
          {
            level: "info",
            label: "—",
            key: null,
            value: null,
            reason:
              "Expert banks will occupy about 21.0 GiB of host RAM; 80 GiB is free, so the parallel load path stays available.",
          },
        ],
        fit: {
          usable: 244_318,
          ceiling: 262_144,
          is_truncated: true,
          ratio: 0.932,
          summary: "233.1k of 256k",
        },
        unpriced: null,
        is_empty: false,
        edit_count: 2,
      };
      emit();
      return { status: "ok", plan: true };
    case "/api/serve/plan/apply": {
      let changed = 0;
      for (const step of state.serve.plan?.steps ?? []) {
        if (step.key && step.value !== null) {
          state.serve.values[step.key] = step.value;
          changed += 1;
        }
      }
      state.serve.plan = null;
      recount();
      toast(`applied ${changed} change(s)`, "success");
      return { status: "ok", changed };
    }
    case "/api/serve/plan/dismiss":
      state.serve.plan = null;
      emit();
      return { status: "ok" };

    case "/api/profiles/save": {
      const name = str("name");
      const existing = state.serve.profiles.findIndex((p) => p.name === name);
      if (existing < 0) {
        state.serve.profiles = [
          ...state.serve.profiles,
          { name, notes: "", model: state.serve.values["model"] ?? null },
        ];
      }
      state.serve.last_used_profile = name;
      toast(existing < 0 ? `saved profile ${name}` : `updated profile ${name}`, "success");
      return { status: "ok", created: existing < 0 };
    }
    case "/api/profiles/load":
      state.serve.last_used_profile = str("name");
      toast(`loaded profile ${str("name")}`, "success");
      return { status: "ok" };
    case "/api/profiles/delete":
      return ask({
        title: "Delete profile",
        body: [str("name"), "", "This cannot be undone."],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: true,
        action: { kind: "delete_profile", name: str("name") },
      });

    case "/api/cache/pending": {
      const row = poolRow(payload["pool"] as PoolId);
      if (row) {
        const raw = payload["value"];
        if (raw === null) row.pending = null;
        else {
          const clamped = Math.max(Math.max(row.min ?? 1, 1), Math.min(row.max, Number(raw)));
          row.pending = clamped === row.current ? null : clamped;
        }
        repricePools();
        emit();
        return { status: "ok", pending: row.pending };
      }
      return { status: "ok", pending: null };
    }
    case "/api/cache/adjust": {
      const row = poolRow(payload["pool"] as PoolId);
      if (row) {
        const percent = Number(payload["percent"] ?? 0);
        const step = Math.max(1, Math.round(row.max * Math.abs(percent)));
        const base = row.pending ?? row.current;
        const next = Math.max(1, Math.min(row.max, base + (percent < 0 ? -step : step)));
        row.pending = next === row.current ? null : next;
        repricePools();
        emit();
        return { status: "ok", pending: row.pending };
      }
      return { status: "ok", pending: null };
    }
    case "/api/cache/reset-all":
      for (const row of state.cache.pools ?? []) row.pending = null;
      repricePools();
      emit();
      return { status: "ok" };
    case "/api/cache/apply":
      return ask({
        title: "Rebuild cache",
        body: [
          ...(state.cache.pools ?? [])
            .filter((row) => row.pending !== null)
            .map((row) => `${row.label}: ${row.current} → ${row.pending ?? 0} ${row.unit}`),
          "",
          `${state.cache.active_requests} request(s) are in flight; the rebuild is rejected until they finish.`,
        ],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: false,
        action: { kind: "apply_cache_rebuild" },
      });

    case "/api/jobs/bench":
      toast("cannot benchmark: a convert job is using the GPU", "warn");
      return { status: "ok" };
    case "/api/jobs/cancel":
      return ask({
        title: "Cancel job",
        body: [
          "A partly written FTW directory is left behind and must be deleted before a retry.",
        ],
        options: ["Cancel", "Confirm"],
        default_index: 0,
        destructive: true,
        action: { kind: "cancel_job", id: Number(payload["id"] ?? 0) },
      });
    case "/api/jobs/clear-finished": {
      const before = state.jobs.items.length + state.jobs.downloads.length;
      state.jobs.items = state.jobs.items.filter((j) => j.is_running);
      state.jobs.downloads = state.jobs.downloads.filter((d) => d.is_running);
      const removed = before - state.jobs.items.length - state.jobs.downloads.length;
      emit();
      return { status: "ok", removed };
    }

    case "/api/logs/clear":
      logRing.length = 0;
      state.logs = { ...state.logs, count: 0, first_seq: state.logs.last_seq };
      emit();
      return { status: "ok" };
    case "/api/requests/clear":
      requestRing.length = 0;
      state.requests = { ...state.requests, count: 0, first_seq: state.requests.last_seq };
      emit();
      return { status: "ok" };
    case "/api/requests/pause": {
      const paused = payload["paused"] === true;
      state.requests = { ...state.requests, paused };
      emit();
      return { paused };
    }
    default:
      return { status: "ok" };
  }
}

function recountHubSelection(): void {
  const wanted = state.hub.files.filter((f) => f.wanted);
  state.hub.selected_count = wanted.length;
  state.hub.selected_bytes = wanted.reduce((sum, f) => sum + f.size, 0);
  emit();
}
