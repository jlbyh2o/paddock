# The web API

The HTTP contract between `ft-man web` and the single-page application in `web/`. It is
the complete surface: every screen the TUI draws and every action a key triggers has a
representation here. [web-ui.md](web-ui.md) records the architecture decisions this
document implements; `web/src/api/types.ts` is the TypeScript rendering of the documents
below and the frontend's source of truth.

Two engineers should be able to build against this independently. Where a value is
derived by `App` today, the field says which function produces it, so the backend has
nothing to invent and the browser has nothing to re-derive.

---

## 1. Conventions

### 1.1 Base path and content type

| | |
|---|---|
| API base path | `/api` |
| Request bodies | `application/json`, and a `POST` that declares anything else is refused with `415` (section 1.2); an action with no parameters still accepts `{}`, an empty body, or no `Content-Type` at all |
| Response bodies | `application/json`, except `GET /api/events` (`text/event-stream`) |
| Character set | UTF-8 |
| Field naming | `snake_case` throughout, matching what `#[derive(Serialize)]` produces from the Rust structs |
| Numbers | JSON numbers. Byte counts, token counts and slot counts are unsigned integers; rates, ratios and GB/s figures are floating point |
| Paths | Absolute filesystem paths as strings, exactly as `PathBuf::display()` renders them |
| Timestamps | Either an ISO-8601 string (when the Rust value is already a `chrono::DateTime` or a string from the engine) or a number of Unix milliseconds. Each field below says which |
| Durations | Numbers. Fields ending `_ms` are milliseconds, `_s` are seconds. Never a Rust `Instant` or `Duration` on the wire |

### 1.2 Errors

Every non-2xx `/api` response is:

```json
{ "error": "human-readable sentence, lowercase, no trailing period", "toasted": true }
```

| Status | When |
|---|---|
| `400 Bad Request` | Malformed JSON, a missing required field, an unparseable value, an unknown enum variant, a `{id}` path segment that is not a number |
| `401 Unauthorized` | Auth is required and the request carried no valid token (see 1.3) |
| `403 Forbidden` | A state-changing request carried an `Origin` header naming another site (see below) |
| `404 Not Found` | The target identity does not exist right now: no model at that `path`, no job with that `id`, no profile or template by that `name`, no knob with that `key` |
| `409 Conflict` | The action is refused in the current state: an engine is already running, a GPU-exclusive job cannot start, a repo is already downloading, a knob value failed `knobs::validate_value`, `serve.validate()` returned errors on start |
| `415 Unsupported Media Type` | A `POST` body declared a `Content-Type` other than `application/json` (see below) |
| `500 Internal Server Error` | An I/O failure, a spawn failure, or an unexpected panic caught at the handler boundary |
| `503 Service Unavailable` | The action needs something absent from this installation: the FreeToken CLI (`app.ft` is `None`), the `hf` CLI, or a running engine for a control-plane call |

**`toasted`** is on every error body. It is `true` when the daemon also pushed this refusal
onto `app.toasts`, so it arrives in the next snapshot; the client then renders *one*
problem, not two — either the status body or the toast, never both. It is `false` for a
refusal the client is expected to render where the reader is already looking.

The rule the daemon follows, so a client can predict it:

| Class | `toasted` | Why |
|---|---|---|
| `409`, `503`, `500` from an action | `true` | A refusal about the *state of the machine* — an engine already running, a GPU already busy, no FreeToken CLI. The terminal has always shown these as a toast and still does |
| `400` and `404` from an action | `false` | A refusal about the *request*: an empty query, an empty profile name, a path that is no longer in the library, a knob key the schema does not know. It belongs against the field or the row that produced it |
| `409` from `POST /api/serve/knob` | `false` | The one state-shaped code used for a field-shaped problem: `knobs::validate_value` names the flag, and the browser has an inline slot under the value (section 4.8) |
| `401`, `403`, `404` (no such endpoint), `415` | `false` | Raised by the web layer before any action ran, so there was nothing to toast |

The terminal keeps its old behavior throughout: where the shared action now stays silent,
`ui::input` raises the toast on the TUI's own side, because a terminal has no inline slot.

* An action that succeeds returns `200` with a small typed reply (section 4). Any state it
  changed arrives through the snapshot stream, never in the reply.
* A `200` does **not** always mean "done". When `ui.confirm_destructive` is on, an action
  that needs confirmation returns `200 {"status":"confirm_pending", ...}` and nothing has
  happened yet. Section 4 lists which routes can do this. An action that has to touch the
  filesystem before it can even *word* the confirmation returns `{"status":"started"}`
  instead, and the modal appears in a later snapshot — see 4.5.

**Cross-origin and content type.** The daemon answers on a LAN address with a cookie
session, which is exactly what a cross-site request forgery needs: any page open in the
operator's browser can `fetch` this origin, the browser attaches the cookie, and
`POST /api/models/delete` runs. Two checks close that, on every method other than `GET`,
`HEAD` and `OPTIONS`, and on the open routes as well as the gated ones:

* **`Origin`** — when the header is present its authority (host and port, case-insensitively)
  must equal the request's `Host`. A browser sets it on every cross-origin request and
  cannot be talked out of it; anything else is `403`. A **missing** `Origin` is allowed:
  that is `curl`, and a tool with no origin has no cookie jar to be borrowed either. `null`
  — what a sandboxed iframe or a `file://` page sends — matches nothing and is refused.
* **`Content-Type`** — must be `application/json` (any `; charset=…` is ignored) or absent.
  A cross-site form post is the one cross-origin request that needs no preflight at all, and
  a form can only send `application/x-www-form-urlencoded`, `multipart/form-data` or
  `text/plain`. Requiring JSON puts every state-changing route behind a preflight the
  browser will refuse to make. Anything else is `415`.

Neither check consumes a token or a round trip, and neither affects `GET`: the browser's
own rules already keep another origin from reading a response it is not allowed to see.

### 1.3 Authentication

Optional, off by default, exactly as `[web] token` / `--token` describe in web-ui.md.

* When a token is configured, every `/api` request must carry it as
  `Authorization: Bearer <token>` **or** as the `ft_man_token` cookie.
* Both are checked, and either one matching authorizes the request. A stale or unrelated
  `Authorization` header — a proxy's, a browser extension's — must not shadow a cookie the
  browser holds, and a non-`Bearer` scheme is ignored rather than treated as a wrong token.
* When no token is configured there is no authentication at all and every request is
  authorized.
* The token must be a value a cookie can carry: no whitespace, control characters, `;`,
  `=`, `,`, `"` or `\`. `ft-man web --token` **refuses to start** on one that is not, and
  says why. The header path would work with any of those, so the failure would otherwise be
  invisible to `curl` and to tests and total for every browser — `EventSource` has no other
  way to authenticate.
* Static assets (section 1.4) are never gated, so the login page can load.

**`GET /api/auth`** — always reachable, never gated.

```json
{ "auth_required": true, "authorized": false }
```

**`POST /api/login`** — body `{"token": "..."}`.

* On a match: `200 {"authorized": true}` plus
  `Set-Cookie: ft_man_token=<token>; Path=/; HttpOnly; SameSite=Strict` (and `Secure` when
  the request arrived over TLS). The cookie has no `Max-Age`; it is a session cookie.
* On a mismatch: `401 {"error": "that token is not correct"}`. The comparison is constant
  time.
* When no token is configured: `200 {"authorized": true}` and no cookie is set.

**`POST /api/logout`** — body `{}`. Clears the cookie (`Max-Age=0`) and returns
`200 {"authorized": false}`. Always succeeds, even when auth is off.

`GET /api/events` cannot send an `Authorization` header from `EventSource`, so the cookie
is the path a browser actually uses; the bearer header exists for `curl` and for tests.

**`GET /api/events` when unauthenticated** — `401` with the ordinary JSON envelope
(`content-type: application/json`), decided *before* the response becomes a stream. Not
one SSE byte is written: no `retry:`, no comment, no `event: snapshot`. A `200
text/event-stream` carrying an error frame would be worse than useless, because
`EventSource` surfaces it as an opaque `onerror` with no status to read.

`EventSource` does not expose the status code either, so the browser's recovery is to
re-probe `GET /api/auth` (never gated) whenever the stream errors, and go to the login
page when it answers `auth_required: true, authorized: false`. That is what `web/` does,
and it is why the refusal must be a real `401` rather than a closed connection: a daemon
that simply dropped the stream would be indistinguishable from one that had stopped.

### 1.4 Static assets and SPA fallback

* `GET /` and any path that does not start with `/api` is served from the embedded
  `web/dist` bundle (`rust-embed`).
* A path that matches an embedded file is served with that file's media type and a strong
  `ETag`; hashed asset filenames additionally get `Cache-Control: public, max-age=31536000,
  immutable`.
* A path that matches no embedded file and does not start with `/api` returns
  `index.html` with `200` and `Cache-Control: no-cache`, so client-side routes deep-link.
* A path under `/api` that matches no route returns `404` with the JSON error envelope,
  never `index.html`.

### 1.5 Targets are identities

The server never accepts a list index. Every action names its target by identity:

| Thing | Identity | Notes |
|---|---|---|
| Model | `path` (string) | The checkpoint directory, `Model::path`. Not the name, which is not unique across roots |
| Job | `id` (number) | `Job::id` |
| Download | `id` (number) | `Download::id`. **Job ids and download ids are separate counters and do collide**, which is why cancel is two routes |
| Profile | `name` (string) | `Profile::name` |
| Stored template | `name` (string) | `StoredTemplate::name` |
| Knob | `key` (string) | `Knob::key`, e.g. `moe_cache_size` — never the flag spelling |
| Hub repo | `repo_id` + `revision` | `RepoInfo::id`, and the revision string the listing was taken at |
| Hub variant | `label` (string) | `Variant::label` |
| Hub file | `path` (string) | `RepoFile::path`, repo-relative |
| Cache pool | `pool` (enum) | `"moe" \| "kv" \| "mamba" \| "swa"` |

Because targets are explicit, **`POST /api/select/model` does not exist and must not be
added.** In the TUI, "apply this template" means "apply it to the model selected on the
Models tab" — a cross-tab dependency that exists only because a terminal has one cursor per
list and no way to name a second one in the same keystroke. On the web the request body
carries both identities (`{"template": "...", "model_path": "..."}`), so the server needs
no notion of which model the browser is looking at, two browser tabs cannot fight over one
selection, and a list that changed between render and click cannot retarget an action.
The same reasoning applies to the Hub's "the repo whose files are listed", the Serve tab's
"the highlighted knob", and the Cache tab's "the selected pool": all become request
parameters. Selection, scroll position, filter text, pane focus and the help overlay are
browser state and appear nowhere in this API.

### 1.6 Concurrency and ordering

One `App` behind one mutex. Handlers lock it, act, unlock; nothing is held across an
`.await`. Therefore:

* Actions are serialized and take effect in arrival order.
* Every action that mutates state bumps `Snapshot::seq`; a client that sends an action and
  then sees a snapshot with a higher `seq` has seen its effect (or the toast explaining
  why it did nothing). An action that refused with `"toasted": false` changed nothing, so it
  publishes nothing — there is no new state for any *other* client to render.
* Long operations (conversion, benchmark, download, cache rebuild, template render check,
  Hub search) are spawned as tasks. The route returns as soon as the task is started, and
  the result arrives later as a snapshot change plus a toast. So is anything that has to
  touch the filesystem to decide what to *ask*: sizing a checkpoint before offering to
  delete it, and removing it afterwards (section 4.5).
* **One task builds the frames.** A state change wakes a single broadcaster, which
  serializes the snapshot once and publishes the finished bytes; each connected stream only
  forwards them. Nothing is serialized per client, and every client connected at a given
  moment sees the identical document with the identical `seq`.
* **Nothing publishes a frame that carries no news.** The supervisor tick runs five times a
  second forever and on an idle machine changes nothing; it wakes the broadcaster only when
  the engine's state moved, a toast expired, a download's rate was resampled, or the polled
  endpoint changed. That is what leaves a second in which the heartbeat can fire.
* **The snapshot may not touch the filesystem.** It is built under the one `App` mutex that
  every route and both front ends share, so a `stat` on unmounted network storage would
  stall the whole daemon. Every figure that needs the disk is sampled elsewhere and read
  back here: template status and root existence at scan time, free space and the bench
  profile path on the hardware tick (once a second) and after anything that moves real
  bytes, and a job's output counter from its own in-memory ring.

---

## 2. State: `GET /api/snapshot` and `GET /api/events`

### 2.1 Transport

**`GET /api/events`** — Server-Sent Events.

| | |
|---|---|
| `event: snapshot` | `data:` is one complete `Snapshot` document as a single JSON line |
| `event: heartbeat` | `data: {}` — once a second when nothing changed, so a proxy does not idle out the connection |
| First frame | A `snapshot` is sent immediately on connect, before any heartbeat |
| Coalescing | At most **10 snapshots per second**. Changes arriving faster are merged; the client always receives the newest state, never a backlog |
| Retry | `retry: 2000` is sent once at the start of the stream |
| `Last-Event-ID` | Ignored. State is absolute, not incremental — reconnecting simply yields a fresh snapshot |
| `id:` | The snapshot's `seq`, so browser devtools show progress |

**`GET /api/snapshot`** — the same document on demand, `200 {...}`. Used on first paint, by
a client that cannot use `EventSource`, and by tests.

### 2.2 `seq`, size and what is deliberately absent

* `seq` is a `u64` starting at 1, incremented on every snapshot the server builds. It is
  monotonic for the lifetime of the process and resets when the daemon restarts (the
  client detects that by seeing a lower `seq` than the one it holds, and re-fetches
  everything, including the incremental streams of section 3).
* The snapshot carries **no engine log lines, no job output text and no request-ring
  entries**. Those are append-only and unbounded-ish; they are fetched by sequence number
  from the endpoints in section 3. The snapshot carries only their *counters* so the
  browser knows when to poll.

Size budget — target **under 64 KiB** for a typical populated machine:

| Section | Typical | Notes |
|---|---|---|
| `engine` + `telemetry` (raw `Health`, `Stats`, `CacheStatus`) | ~2.5 KiB | `cache.limits` and `cache.last_rebuild` are passed through as-is |
| `series` (5 series × 240 `u64`) | ~8 KiB | See the cap below |
| `models` (per entry ~400 B) | ~12 KiB at 30 models | Unbounded; a 300-model library costs ~120 KiB and exceeds the budget. Accepted: the alternative is paging a list the Models tab renders whole |
| `hub` (50 results + up to ~80 files + variants + compat) | ~14 KiB | Search is already capped at 50 by `Hub::search` |
| `serve` (values + errors + command preview) | ~2 KiB | The knob *schema* is **not** here — see `GET /api/knobs` |
| `templates` (stored + remote + preview head) | ~6 KiB | `preview` is capped at 8 KiB |
| `jobs` + `downloads` + `bench_profile` | ~4 KiB | |
| `cache` pool table | ~1 KiB | |
| `plan`, `confirm`, `toasts`, `config`, `environment` | ~4 KiB | |

Two caps exist purely to hold the budget, and both are server-side:

* **Series**: each series sends its most recent **120** samples (`History::tail(120)`),
  oldest first, as a plain array of numbers. `History` holds 240; 120 is more than any
  sparkline width a browser will render.
* **Template preview**: `templates.preview.text` is truncated to 8 KiB, with
  `preview.truncated: true` when it was cut.

### 2.3 Enum encoding

Rust enums that carry data are **internally tagged objects** with a `kind` discriminant in
`snake_case`, plus the payload under a named field. Rust enums that carry no data are plain
`snake_case` strings. This is stated once and applies everywhere below.

```jsonc
// EngineState
{"kind": "stopped"} | {"kind": "starting"} | {"kind": "running"} | {"kind": "stopping"}
| {"kind": "adopted"}
| {"kind": "exited", "code": 1, "signal": null}      // either may be null

// JobStatus, DownloadStatus
{"kind": "running"} | {"kind": "done"} | {"kind": "canceled"}
| {"kind": "failed", "reason": "with status 1"}

// Preflight (ft::preflight::Outcome)
{"kind": "ok", "detail": "..."} | {"kind": "warn", "detail": "..."} | {"kind": "fail", "detail": "..."}

// templates::Status
{"kind": "built_in", "label": "built-in"}
{"kind": "foreign",  "label": "custom (not applied by ft-man)"}
{"kind": "overridden", "label": "qwen-sharp (v3)", "applied": AppliedTemplate}

// knobs::Kind
{"kind": "text"} | {"kind": "flag"}
{"kind": "int", "min": 1, "max": null}
{"kind": "float", "min": 0.0, "max": 1.0}
{"kind": "choice", "options": ["auto", "offload", "hybrid", "cpu", "fused"]}

// ConfirmAction — see 2.15
{"kind": "stop_engine", "force": false}
{"kind": "delete_model", "path": "/models/x"}
...
```

Data-free enums as strings:

| Rust | JSON values |
|---|---|
| `models::Format` | `"hf"`, `"ftw"`, `"gguf"`, `"partial_ftw"` (plus a sibling `format_label` field carrying `"HF"`, `"FTW"`, `"GGUF"`, `"PART"`) |
| `compat::Verdict` | `"supported"`, `"caution"`, `"unsupported"`, `"unknown"` (plus `verdict_label`, from `Verdict::label()`) |
| `compat::Level` | `"info"`, `"caution"`, `"blocker"` |
| `plan::Level` | `"info"`, `"advice"`, `"warning"` |
| `knobs::Group` | `"model"`, `"server"`, `"runtime"`, `"memory"`, `"moe"`, `"api"` |
| `ui::app::Pool` | `"moe"`, `"kv"`, `"mamba"`, `"swa"` |
| `widgets::ToastKind` | `"info"`, `"success"`, `"warn"`, `"error"` |
| `variants::Role` | `"weights"`, `"projector"`, `"draft"` |
| `ft::proc::JobKind` | `"convert"`, `"bench"` (plus `kind_label`, `"convert"` / `"bench"`) |

`ui::app::Tab`, `HubFocus`, `TemplatePane` and `widgets::Selection` are browser concerns
and are not serialized.

### 2.4 `Snapshot` — top level

| Field | Type | Source |
|---|---|---|
| `seq` | number | Monotonic counter, section 2.2 |
| `ts_ms` | number | `chrono::Utc::now().timestamp_millis()` when the snapshot was built |
| `version` | string | `env!("CARGO_PKG_VERSION")` — what the TUI prints bottom-right |
| `engine` | `EngineSnapshot` | 2.5 |
| `telemetry` | `TelemetrySnapshot` | 2.6 |
| `series` | `SeriesSnapshot` | 2.7 |
| `hardware` | `HardwareSnapshot` | 2.8 |
| `models` | `ModelsSnapshot` | 2.9 |
| `hub` | `HubSnapshot` | 2.10 |
| `templates` | `TemplatesSnapshot` | 2.11 |
| `serve` | `ServeSnapshot` | 2.12 |
| `cache` | `CacheSnapshot` | 2.13 |
| `jobs` | `JobsSnapshot` | 2.14 |
| `requests` | `RequestsSnapshot` | 2.16 |
| `logs` | `LogsSnapshot` | 2.16 |
| `toasts` | `Toast[]` | 2.15 |
| `confirm` | `Confirm \| null` | 2.15 |
| `config` | `ConfigSnapshot` | 2.17 |
| `environment` | `EnvironmentSnapshot` | 2.18 |

### 2.5 `engine`

| Field | Type | Source |
|---|---|---|
| `state` | `EngineState` (tagged) | `app.engine.state` |
| `is_live` | boolean | `app.engine.is_live()` — true for starting, running, adopted |
| `status_text` | string | **`app.engine_status_text()`**. The single authority for the status line: it prefers `/health` (`loading 62%`, `error: ...`, `rebuilding cache`, `serving`) and falls back to the supervisor state (`starting`, `stopping`, `exited with status 1`, `attached (unreachable)`, `running (unreachable)`, `not running`). The browser must render this string, not re-derive it |
| `status_class` | `"good" \| "warn" \| "bad" \| "dim"` | The theme role `app.engine_status_color()` picks, mapped to a name so CSS can color the dot without knowing the palette |
| `pid` | number \| null | `app.engine.pid` |
| `adopted` | boolean | `app.engine.state == EngineState::Adopted` — the TUI writes `pid 48211 (attached)` |
| `model` | string \| null | **`app.current_model()`**: `stats.model.id`, else `health.model`, else `app.engine.model` |
| `port` | number \| null | `app.engine.port` |
| `command_line` | string \| null | `app.engine.command_line` (shell-quoted, as spawned or as reconstructed on adoption) |
| `log_path` | string \| null | `app.engine.log_path` |
| `endpoint` | string | `app.client.base_url()` — the URL actually polled, after `poll_host` turns a wildcard bind into loopback |
| `server_reachable` | boolean | `app.server_reachable()` |
| `context_fit` | `ContextFit \| null` | **`app.context_fit()`**. `{usable, ceiling, is_truncated, ratio, summary, verdict}` where `summary` is `ContextFit::summary()` (`"32k of 256k"`), `ratio` is `ContextFit::ratio()`, and `verdict` is `ContextFit::verdict()` — the plan overlay's headline sentence, `"the full 256k this model offers"` or `"233.1k of the 256k this model offers  (91%)"`. `views::plan` prints the same string, so the terminal and the browser cannot word one number two ways. `null` while the engine has published neither number |
| `prefix_reuse` | `Reuse \| null` | **`app.prefix_reuse()`**. `{fraction, cold_rate, samples, summary}`; `summary` is `Reuse::summary()` (`"~97%  (est. from 12 reqs)"`). `null` when the evidence is too thin — the browser shows nothing, never a zero |
| `completed_rate` | number | **`app.completed_rate.get()`** — the smoothed completions per second the Dashboard prints as `0.31 completed/s` |
| `active_jobs` | number | `app.active_jobs()` |
| `active_downloads` | number | `app.active_downloads()` |
| `gpu_busy_reason` | string \| null | **`app.gpu_busy_reason()`** — why a conversion or benchmark would be refused right now. `null` means the GPU is free |
| `start_blocked` | string \| null | **`actions::start_blocked(app)`** — why `POST /api/engine/start` would be refused right now, computed by the same predicate the route runs, so a button that offers to start cannot disagree with the daemon that would refuse it. The reasons, in the order they are checked: an engine is already live; one another process started is recorded in the state file; a GPU-exclusive job is running; there is no FreeToken CLI; `serve.validate()` has an error, flag-prefixed. `null` when a start would be attempted. The cross-process check is refreshed once a second by `Engine::poll`, and forced by the route itself before it decides |

### 2.6 `telemetry`

The raw control-plane documents, passed through unchanged so the browser sees exactly what
the engine said.

| Field | Type | Source |
|---|---|---|
| `health` | `Health \| null` | `app.telemetry.health`, verbatim `ft::types::Health` |
| `stats` | `Stats \| null` | `app.telemetry.stats`, verbatim `ft::types::Stats` |
| `cache_status` | `CacheStatus \| null` | `app.telemetry.cache`, verbatim `ft::types::CacheStatus` (including `geometry`, `limits` and `last_rebuild` as raw JSON) |
| `error` | string \| null | `app.telemetry.error` — the last poll failure, e.g. connection refused |
| `age_ms` | number \| null | `app.telemetry.at.elapsed()` in milliseconds. The TUI renders this as `(12s ago)` beside the error |

Derived helpers the views use, computed server-side so the browser does not reimplement
them (all `null` when the source document is absent):

| Field | Type | Source |
|---|---|---|
| `health_load_ratio` | number \| null | `Health::load_ratio()` |
| `pool_bytes` | `{kv, moe, mamba, swa, total}` \| null | `CacheGeometry::pool_bytes()` plus `PoolBytes::total()` |
| `total_experts` | number \| null | `CacheGeometry::total_experts()` |
| `kv_used_tokens` / `kv_total_tokens` | number \| null | `PagePool::used_tokens()` / `total_tokens()` on `stats.kv` |
| `kv_ratio` | number \| null | `PagePool::ratio()` on `stats.kv` |
| `swa_used_tokens` / `swa_total_tokens` / `swa_ratio` | number \| null | Same on `stats.swa` |
| `mamba_ratio` | number \| null | `SlotPool::ratio()` on `stats.mamba` |
| `last_rebuild_summary` | string \| null | The Cache view's `last_rebuild_summary(app)` — `"last rebuild: MoE 2,403  KV 8,192"` |
| `sampling_summary` | string \| null | **`views::dashboard::format_sampling(stats.model.sampling)`** — the checkpoint's recommended sampling as the Dashboard prints it, `"temperature 0.6  top_p 0.95"`. Reasoning models ship these in `generation_config.json` and loop without them, so the browser must show what the engine will apply rather than re-derive which keys matter. `null` when the engine published none |

### 2.7 `series`

Each field is an array of non-negative integers, oldest first, at most 120 entries
(`History::tail(120)`; section 2.2).

| Field | Source |
|---|---|
| `decode_tps` | `app.series.decode_tps` |
| `prefill_tps` | `app.series.prefill_tps` |
| `gpu_util` | `app.series.gpu_util` (percent) |
| `vram` | `app.series.vram` (MiB) |
| `active` | `app.series.active` (concurrent requests) |
| `decode_peak` | number — `app.series.decode_peak`, the running maximum the Dashboard prints as `peak 61.2` |

### 2.8 `hardware`

| Field | Type | Source |
|---|---|---|
| `gpu_source` | `"NVML" \| "nvidia-smi"` | `app.gpu_source` — the pane title is `GPU (NVML)` |
| `gpus` | `Gpu[]` | `app.gpus`, verbatim `probe::Gpu` (`index`, `name`, `uuid`, `memory_total`, `memory_used`, `utilization`, `temperature`, `power_watts`, `power_limit_watts`, `pcie_link`), plus derived `memory_free` (`Gpu::memory_free()`), `memory_ratio` (`Gpu::memory_ratio()`) and `short_uuid` (`Gpu::short_uuid()`) |
| `engine_gpu_uuid` | string \| null | `stats.gpus.first().uuid` — the card the engine actually bound, which the TUI flags with `← engine` |
| `reported_gpus` | `GpuCard[]` | `stats.gpus`, verbatim. Rendered only when `gpus` is empty ("no local GPU readable; reporting what the engine says") |
| `host` | `Host` | `app.host`, verbatim `probe::Host` (`cpu_percent`, `cpu_cores`, `physical_cores`, `memory_total`, `memory_used`, `swap_total`, `swap_used`, `load_avg` as a 3-tuple array, `hostname`, `kernel`, `uptime_s`), plus derived `memory_free` and `memory_ratio` |
| `bench_profile` | `BenchProfile \| null` | `app.bench_profile`, verbatim `ft::types::BenchProfile` including `dtypes` and `dtype_kernels` as objects keyed by format name |
| `bench_summary` | string \| null | The Dashboard's one-liner: `"bench: nvfp4→hybrid  mxfp4→offload"`, or `"bandwidth profile present"` when no verdict is recorded. `null` when there is no profile, which the Dashboard renders as "no bandwidth profile — run one from the Jobs tab (b)" |
| `bench_profile_path` | string \| null | `plan::bench_profile_status(uuid)` — the file the profile was read from |

### 2.9 `models`

| Field | Type | Source |
|---|---|---|
| `scanning` | boolean | `app.models_view.scanning` — the pane title becomes `Library (scanning…)` |
| `items` | `ModelEntry[]` | `app.models` in scan order (sorted by lowercased name). **Unfiltered** — filtering is a browser concern |
| `roots` | `{path, exists}[]` | `config.library.effective_roots()` — the configured roots plus the Hugging Face cache and the FTW directory — each marked `is_dir()` **at scan time**, not when the snapshot is built. A snapshot may not stat a path: it is assembled under the one `App` mutex, and a root on unmounted network storage would stall every connected browser. The empty-library message names every root and says which do not exist |
| `config_path` | string | `config::config_path()` — named in that same message |

`ModelEntry` is `models::Model` verbatim plus derived fields:

| Field | Type | Source |
|---|---|---|
| `name` | string | `Model::name` |
| `repo` | string \| null | `Model::repo` |
| `variant` | string \| null | `Model::variant` |
| `path` | string | `Model::path` — **the identity used by every model action** |
| `format` | `"hf" \| "ftw" \| "gguf" \| "partial_ftw"` | `Model::format` |
| `format_label` | string | `Format::label()` — `HF`, `FTW`, `GGUF`, `PART` |
| `format_description` | string | The Models detail pane's long form: `"FTW — FreeToken fast-load"`, `"Hugging Face safetensors"`, `"GGUF"`, `"incomplete FTW conversion"` |
| `size_bytes` | number | `Model::size_bytes` |
| `arch` | string \| null | `Model::arch` |
| `model_type` | string \| null | `Model::model_type` |
| `is_moe` | boolean | `Model::is_moe` |
| `num_experts` | number \| null | `Model::num_experts` |
| `num_layers` | number \| null | `Model::num_layers` |
| `quant` | string \| null | `Model::quant` (render uppercased, as the detail pane does) |
| `max_position` | number \| null | `Model::max_position` |
| `ftw_fingerprint` | string \| null | `Model::ftw_fingerprint` |
| `converted_to` | string \| null | `Model::converted_to` — the FTW build's path when one exists; the list row's `→` marker and the detail pane's `Converted` line |
| `modified_ms` | number \| null | `Model::modified` as Unix milliseconds |
| `summary` | string | **`Model::summary()`** — the dim second line (`"Qwen3MoeForCausalLM · MoE x128 · NVFP4 · 256k ctx"`) |
| `served_name` | string | **`Model::served_name()`** — `repo:variant`, what `--served-model-name` is set to and the key `costs.json` is keyed by |
| `convertible` | boolean | `Model::convertible()` |
| `is_partial` | boolean | `Model::is_partial()` |
| `template_status` | `TemplateStatus` (tagged) | **`templates::status(&model.path)`** as recorded on the `Model` by the scan, read back through `app.template_status(model)`, with `label` from `Status::label()`. Re-read only when it can change — after a template apply or revert, both of which are already writing those directories |
| `template_targets` | string[] | `templates::targets(model)` — the directories an apply would write into (the checkpoint, plus its FTW build when one exists) |
| `ftw_output_path` | string | `models::ftw_output_path(...)` — where a conversion would write, whether or not it exists yet |
| `guidance` | `{level, text}[]` | The detail pane's bullet list, computed server-side because every rule reads host RAM, GPU VRAM and the filesystem. `level` is `"good" \| "warn" \| "bad" \| "dim"`, matching the color the TUI uses |

### 2.10 `hub`

| Field | Type | Source |
|---|---|---|
| `query` | string | `app.hub_view.query.value` — the last query the server searched with |
| `searching` | boolean | `app.hub_view.searching` |
| `results` | `RepoSummary[]` | `app.hub_view.results`, verbatim (`id`, `downloads`, `likes`, `last_modified`, `tags`, `gated` as raw JSON, `private`), plus derived `is_gated` (`RepoSummary::is_gated()`) and `interesting_tags` (`RepoSummary::interesting_tags()`) |
| `revision` | string | `app.hub_view.revision` — `"main"` unless an action set another |
| `loading_info` | boolean | `app.hub_view.loading_info` |
| `info` | `RepoInfo \| null` | `app.hub_view.info`, verbatim (`id`, `sha`, `gated`, `siblings[]` of `{path, size}`), plus derived `is_gated` |
| `layout` | `Layout \| null` | `app.hub_view.layout`: `{variants: Variant[], shared: string[]}` where `Variant` is `{label, role, subdir, files, bytes, file_count}` (`file_count` from `Variant::file_count()`), plus derived `is_multi` (`Layout::is_multi()`) |
| `variant` | string \| null | `app.hub_view.variant` — the chosen quantization |
| `custom_selection` | boolean | `app.hub_view.custom_selection` — true once files were toggled by hand, so the UI stops claiming the selection is a quantization |
| `files` | `RepoFile[]` | `app.hub_view.files`: `{path, size, wanted}` |
| `selected_bytes` | number | Sum of `size` over `wanted` files — the Files pane title |
| `selected_count` | number | Count of `wanted` files |
| `compat` | `CompatReport \| null` | `app.hub_view.compat`: `compat::Report` verbatim (`arch`, `model_type`, `is_moe`, `num_experts`, `num_layers`, `quant`, `context`, `notes` as `{level, text}[]`) plus derived `verdict`, `verdict_label` (`Verdict::label()`) and `summary` (`Report::summary()`) |
| `compat_error` | string \| null | `app.hub_view.compat_error` |
| `checking_compat` | boolean | `app.hub_view.checking_compat` |
| `target` | string | `app.hub_view.target.value` — the cache directory the download lands in, `hub::cache_repo_dir(config.library.hub_cache(), info.id)`. Informational; downloads always go to the cache |
| `disk_free` | `{measured_path, free_bytes} \| null` | **`hub::disk_free_at(target)`**. `measured_path` is the existing ancestor the figure was taken from — it must be shown with the number, because the walk up to an existing ancestor can silently measure a different filesystem |
| `hf_cli` | string \| null | `app.hf_cli` — the resolved `hf` binary. `null` means downloads are impossible until it is installed |
| `hf_installing` | boolean | `app.hf_installing` |
| `hf_install_command` | string | `hub::INSTALL_COMMAND`, quoted in the install confirmation |

### 2.11 `templates`

| Field | Type | Source |
|---|---|---|
| `stored` | `StoredTemplateEntry[]` | `app.templates_view.stored`: `{name, path, size, meta}` where `meta` is `TemplateMeta` (`source`, `revision`, `repo_path`, `fetched_at`, `version`), plus derived `subtitle` (`StoredTemplate::subtitle()`) |
| `repo` | string | `app.templates_view.repo.value` — the repo in the browse field |
| `loading` | boolean | `app.templates_view.loading` |
| `remote` | `{path, size}[]` | `app.templates_view.remote` — the repo's `.jinja` files (`hub::jinja_files`) |
| `remote_repo` | string \| null | `app.templates_view.remote_repo` |
| `remote_revision` | string \| null | `app.templates_view.remote_revision` — the commit the listing resolved to |
| `remote_stored_names` | string[] | `templates::name_for(remote_repo, path)` for each remote file, so the browser can draw the `✓` on files already in the store without knowing the naming rule |
| `preview` | `{name, text, truncated} \| null` | `app.templates_view.preview` — the head of the last previewed template. `text` is capped at 8 KiB |
| `checking` | boolean | `app.templates_view.checking` |
| `preflight` | `{template, outcome} \| null` | `app.templates_view.preflight` — the last render check and which template it was for. `outcome` is the tagged `Preflight` |
| `sources` | string[] | `config.templates.sources` — the repos offered in the browse field |
| `preflight_enabled` | boolean | `config.templates.preflight` — whether an apply runs a render check first |

### 2.12 `serve`

The knob **schema** is static for the process lifetime and is served once by
`GET /api/knobs` (section 4.2). The snapshot carries only values and errors.

| Field | Type | Source |
|---|---|---|
| `values` | `{[key: string]: string}` | `app.serve` — only the knobs actually set. A `Flag` knob that is on is present with the value `"true"`; an off flag is absent |
| `errors` | `{key, flag, message}[]` | **`serve.validate()`**, sorted and deduplicated. Includes the always-present `("model", "a model path or repo id is required")` when no model is set, per-value domain failures from `knobs::validate_value`, `"unknown knob"`, and `"cannot be combined with --num-tokens"` for a mutual-exclusion clash. `flag` is the knob's flag spelling resolved server-side, and is **`null` when the key names no knob** — which a profile from a newer FreeToken, or a hand-edited `profiles.toml`, really does produce. The browser prints `flag ?? key`, so an unresolved key reads `moe_fanout_beta: unknown knob` rather than `undefined: unknown knob` |
| `command_preview` | string | **`serve.preview(program)`** where `program` is `app.ft.display_program()` or `"ft"`. The exact shell-quoted command line `g` would run |
| `set_counts` | `{[group: string]: number}` | Per `knobs::Group`, how many knobs in it are set — the Groups pane's right-hand column |
| `plan` | `Plan \| null` | `app.serve_view.plan`, present only after a successful plan build. See below |
| `profiles` | `ProfileEntry[]` | `app.profiles.items`: `{name, notes, model}` where `model` is `profile.serve.get("model")` — the dim second line in the profile list. The full `serve` map of a profile is **not** sent; loading a profile is a server-side action |
| `last_used_profile` | string \| null | `app.profiles.last_used` — the profile the list marks `active` |

`Plan` (from `plan::Plan`):

| Field | Type | Source |
|---|---|---|
| `steps` | `PlanStep[]` | `plan.steps` in order |
| `fit` | `ContextFit \| null` | `plan.fit`, same shape as `engine.context_fit`, including `verdict` — which is exactly the overlay's headline, so the browser renders it rather than rebuilding the sentence |
| `unpriced` | string \| null | `plan.unpriced` — why the cache split was not planned |
| `is_empty` | boolean | `Plan::is_empty()` |
| `edit_count` | number | `plan.edits().len()` — the overlay's `A applies 3 changes` line |

`PlanStep`:

| Field | Type | Source |
|---|---|---|
| `level` | `"info" \| "advice" \| "warning"` | `Step::level` |
| `label` | string | **`Step::label()`** — `"--kv-reserve-tokens 262144"`, or `"—"` for a note |
| `key` | string \| null | `Step::set`'s knob key, or `null` for a note |
| `value` | string \| null | `Step::set`'s value, or `null` |
| `reason` | string | `Step::reason`, verbatim and unwrapped. The browser wraps it |

### 2.13 `cache`

The Cache tab's pool table, computed exactly as `src/ui/views/cache.rs` computes it. Only
pools `cache_pools::present(&geo, pool)` accepts are listed, in `Pool::ALL` order.
`null` for the whole section's `pools` when `telemetry.cache_status` is absent — the tab
then shows its "only available while the engine is serving" message.

| Field | Type | Source |
|---|---|---|
| `state` | string \| null | `cache_status.state` — `"serving"`, `"rebuilding"` |
| `applying` | boolean | `app.cache_view.applying` |
| `has_pending` | boolean | `CacheView::has_pending()` |
| `pools` | `PoolRow[]` | One per present pool |
| `current_bytes` | `{kv, moe, mamba, swa, total}` \| null | `geo.pool_bytes()` |
| `proposed_bytes` | number \| null | **`views::cache::proposed_bytes(app, geo)`** — total pool bytes if every pending edit were applied. `null` when nothing is pending |
| `delta_bytes` | number \| null | `proposed_bytes - current_bytes.total`, signed. The `+2.00 KiB` / `-2.00 KiB` line |
| `budget_bytes` | number \| null | `geo.cache_budget_bytes`; `null` or `0` when the engine did not publish one |
| `over_budget` | boolean | `budget > 0 && proposed > budget` — the tab's "exceeds the engine's cache budget" warning |
| `budget_ratio` | number \| null | `ratio(pending ? proposed : current.total, budget)` |
| `facts` | string[] | The VRAM-budget pane's footnotes, already formatted: `"expert eviction: lru"`, `"window/full ratio: 0.25"`, `"thinking gears: low/medium/high (default medium)"` |
| `last_rebuild_summary` | string \| null | Same value as `telemetry.last_rebuild_summary`, repeated here because this pane renders it |
| `active_requests` | number | `stats.requests.active` — the apply confirmation says a rebuild is rejected while requests are in flight |

`PoolRow` — **the server sends computed numbers; the browser does no pool arithmetic**:

| Field | Type | Source |
|---|---|---|
| `pool` | `"moe" \| "kv" \| "mamba" \| "swa"` | `Pool` |
| `label` | string | `Pool::label()` — `"MoE expert slots"`, `"KV pages"`, `"GDN state slots"`, `"SWA window pages"` |
| `unit` | string | `Pool::unit()` — `"slots"` or `"pages"` |
| `current` | number | **`cache_pools::geometry().current`** |
| `max` | number | **`cache_pools::geometry().max`** — the engine's published `limits.<key>.max` converted into the pool's own unit (tokens ÷ `page_size` for KV and SWA), else the local fallback, and never below `current` or 1 |
| `min` | number | `cache_pools::geometry().min` — the engine's published `limits.<key>.min` converted into the pool's own unit, floored at **1** and never above `max`. Never null: a slider needs a floor, and a pool rebuilt to zero is a pool the engine no longer has. The TUI does not display it; `POST /api/cache/pending` clamps against exactly this number |
| `pending` | number \| null | `CacheView::pending_for(pool)` — `null` means "leave this pool alone" |
| `shown` | number | `pending ?? current` — what the bar and the number render |
| `delta` | number \| null | `pending - current`, signed. The `(+581)` in the detail line |
| `ratio` | number | `ratio(shown, max(max, current, 1))` — the bar fill |
| `note` | string | **`cache_pools::note(&geo, pool, shown)`** — `"64% of 6,144 experts resident"`, `"262,144 tokens"`, `"32,768 tokens of window"`, or `""` |

The limit keys are FreeToken's own (`moe_experts`, `kv_tokens`, `mamba_slots`,
`swa_tokens`); the conversion from published tokens to pages is the server's job, and a
mismatch is silent, so the browser must never read `cache_status.geometry.limits` itself.
`src/cache_pools.rs` holds that conversion once — the Cache view draws from it, this table
is built from it, and `POST /api/cache/pending` and `/adjust` clamp against it, so the
number a slider is allowed to reach is the number the daemon will accept.

### 2.14 `jobs`

| Field | Type | Source |
|---|---|---|
| `items` | `JobEntry[]` | `app.jobs`, in creation order |
| `downloads` | `DownloadEntry[]` | `app.downloads`, in creation order |
| `convert_checking` | string \| null | `app.convert_checking` — the checkpoint whose conversion preflight is in flight. The empty Jobs list renders this as "Checking that FreeToken can read …" |

`JobEntry`:

| Field | Type | Source |
|---|---|---|
| `id` | number | `Job::id` — the cancel identity |
| `kind` | `"convert" \| "bench"` | `Job::kind` |
| `kind_label` | string | `JobKind::label()` |
| `title` | string | `Job::title` |
| `command_line` | string | `Job::command_line` — the first line of the output pane |
| `status` | `JobStatus` (tagged) | `Job::status` |
| `status_label` | string | **`JobStatus::label()`** — `"running"`, `"done"`, `"failed"`, `"canceled"`. The same function the Jobs view prints |
| `progress` | `{phase, done, total, bytes}` | `Job::progress` (`JobProgress`) |
| `progress_ratio` | number \| null | **`JobProgress::ratio()`** — `null` when no total is known, which is why the dense conversion phase has no bar |
| `progress_detail` | string | The list's second-line text, formatted server-side exactly as `views::jobs::progress_detail` does: `"12.4 GiB / 21.0 GiB  (experts)  310 MiB/s"`, `"step 3 of 7  nvfp4"`, or the bare phase |
| `rate_bps` | number | `Job::rate.get()` — smoothed bytes per second |
| `elapsed_s` | number | **`Job::elapsed()`** in seconds (frozen once `finished_at` is set) |
| `started_at` | string | `Job::started_at` as an ISO-8601 local timestamp |
| `finished_at` | string \| null | `Job::finished_at` |
| `log_path` | string | `Job::log_path` |
| `output_path` | string \| null | `Job::output_path` — where a bench run wrote its profile |
| `output_seq` | number | The job's output line counter, `LogRing::stats().last_seq`. **This is the change counter for `GET /api/jobs/{id}/output`** — it moves when and only when the job has written another line, so a client polls on the number rather than on a timer, and it moves with the job's final status line too, so no extra read is needed after the job stops. It replaced a `stat` of `log_path` per job per snapshot, which was filesystem I/O inside a document built under the mutex every browser shares. A cleared ring does not renumber, so a held value stays comparable |
| `failure_reason` | string \| null | **`Job::failure_reason()`** — the most informative line the process printed, preferred over the exit code. `null` unless the job failed |
| `is_running` | boolean | `Job::is_running()` |

`DownloadEntry`:

| Field | Type | Source |
|---|---|---|
| `id` | number | `Download::id` — the cancel identity, **numbered independently of job ids** |
| `repo` | string | `Download::repo` |
| `revision` | string | `Download::revision` |
| `target` | string | `Download::target` |
| `total_bytes` | number | `Download::total_bytes` |
| `done_bytes` | number | **`Download::done()`** (reads the shared atomic) |
| `ratio` | number | `Download::ratio()` |
| `file_count` | number | `Download::file_count` |
| `files_done` | number | `Download::files_done` |
| `current` | string | `Download::current` — the file in flight |
| `status` | `DownloadStatus` (tagged) | `Download::status` |
| `status_label` | string | **`DownloadStatus::label()`** — `"downloading"`, `"done"`, `"failed"`, `"canceled"`. Note that `status.kind` for a running download is `"running"`: the discriminant is what a client switches on and the label is the verb a reader wants, so both spellings exist on purpose |
| `rate_bps` | number | `Download::rate.get()`, sampled once per tick by `sample_rate()` |
| `eta_s` | number \| null | `(total_bytes - done_bytes) / rate_bps`, `null` when the rate is not yet meaningful (≤ 1 B/s) — the TUI prints `--` |
| `elapsed_s` | number | `Download::elapsed()` in seconds |
| `started_at` / `finished_at` | string / string \| null | ISO-8601 local |
| `failure_reason` | string \| null | The `DownloadStatus::Failed` payload |
| `is_running` | boolean | `Download::is_running()` |

### 2.15 `toasts` and `confirm`

`Toast`:

| Field | Type | Source |
|---|---|---|
| `id` | number | A server-assigned monotonic id, so the browser can animate without matching on text |
| `text` | string | `Toast::text` |
| `kind` | `"info" \| "success" \| "warn" \| "error"` | `Toast::kind` |
| `age_ms` | number | `Toast::at.elapsed()` in milliseconds — computed server-side, because `Instant` cannot cross the wire |
| `ttl_ms` | number | 4000 for info/success, 8000 for warn, 12000 for error — the same ladder `Toast::is_expired()` uses, sent so the browser can fade in step with the server expiring it |

At most four toasts are ever present (`App::toast` pops the front past four), and
`App::expire_toasts` removes expired ones on every tick, so the list shrinks on its own.

`Confirm` — `app.confirm`, `null` when nothing is pending:

| Field | Type | Source |
|---|---|---|
| `title` | string | `Confirm::title` |
| `body` | string[] | `Confirm::body` — one entry per line, empty strings are deliberate blank lines. Render them as-is; they contain the numbers the decision needs |
| `options` | string[] | `Confirm::options` — always `["Cancel", "Confirm"]` today |
| `default_index` | number | `Confirm::selected` as pushed — always `0`, the safe option |
| `destructive` | boolean | `Confirm::destructive` — color the affirmative option as dangerous |
| `action` | `ConfirmAction` (tagged) | What accepting would do. Informational: `POST /api/confirm` carries only `{accept}`, because the pending action lives on the server |

`ConfirmAction` encodings:

```jsonc
{"kind": "stop_engine", "force": false}
{"kind": "delete_model", "path": "/models/Qwen3.6-35B-A3B"}
{"kind": "cancel_job", "id": 3}
{"kind": "cancel_download", "id": 1}
{"kind": "delete_profile", "name": "qwen-256k"}
{"kind": "apply_cache_rebuild"}
{"kind": "apply_template", "template": "qwen-sharp", "model": "/models/x"}
{"kind": "revert_template", "model": "/models/x"}
{"kind": "delete_template", "name": "qwen-sharp"}
{"kind": "reconvert_model", "source": "/models/x"}
{"kind": "install_hf_cli"}
{"kind": "convert_anyway", "source": "/models/x"}
{"kind": "quit"}
```

`{"kind":"quit"}` is included for completeness only: the daemon has no quit action, so the
web layer never pushes it and the browser never needs to render it.

### 2.16 `requests` and `logs` counters

Neither section carries content; both exist so the browser knows when to fetch from
section 3.

`requests`:

| Field | Type | Source |
|---|---|---|
| `paused` | boolean | `app.requests_view.paused` |
| `count` | number | `app.requests_view.entries.len()` (the ring holds at most 512) |
| `first_seq` | number | The stream sequence of the oldest retained entry |
| `last_seq` | number | The stream sequence of the newest entry; `0` when empty |
| `dropped` | number | How many entries have been evicted from the front since the process started |
| `engine_cursor` | number | `app.requests_view.cursor` — the `/v1/requests?since=` cursor the poller holds. Diagnostic |

`logs`:

| Field | Type | Source |
|---|---|---|
| `count` | number | Lines currently in `app.engine.log` |
| `first_seq` | number | Sequence of the oldest retained line |
| `last_seq` | number | Sequence of the newest line; `0` when empty |
| `dropped` | number | Lines evicted by the ring or discarded by a clear, since process start |
| `capacity` | number | `config.ui.log_capacity` |
| `log_path` | string \| null | `app.engine.log_path`, repeated from `engine` because the empty-log message names it |

### 2.17 `config`

Only what the UI needs. Secrets are never sent: `hub.token` is reported as presence and
provenance (section 2.18), never as a value.

| Field | Type | Source |
|---|---|---|
| `theme` | string | `config.ui.theme` — `auto`, `dark`, `light`, `mono`. **Informational.** It names the *terminal's* palette; the web UI follows `prefers-color-scheme` with its own toggle, because a browser theme is a per-browser choice and the daemon is shared. Nothing in `web/` reads this field, and a client that wanted to could only mislead a second browser with it |
| `confirm_destructive` | boolean | `config.ui.confirm_destructive`. When false, actions run immediately and `confirm` is never populated |
| `tick_ms` | number | `config.ui.tick_ms` |
| `log_capacity` | number | `config.ui.log_capacity` |
| `poll_ms` | number | `config.server.poll_ms` |
| `server_host` | string | `config.server.host` |
| `server_port` | number | `config.server.port` |
| `download_dir` | string | `config.library.download_dir`, tilde-expanded |
| `ftw_dir` | string | `config.library.ftw_dir()` |
| `hub_cache` | string | `config.library.hub_cache()` |
| `hub_endpoint` | string | `config.hub.endpoint` |
| `hub_concurrency` | number | `config.hub.concurrency` |
| `hub_ignore` | string[] | `config.hub.ignore` |
| `convert_preflight` | boolean | `config.convert.preflight` |
| `templates_preflight` | boolean | `config.templates.preflight` |
| `template_sources` | string[] | `config.templates.sources` |
| `config_path` | string | `config::config_path()` |
| `state_dir` | string | `config::state_dir()` |
| `disk_free` | `{measured_path, free_bytes} \| null` | **`hub::disk_free_at(download_dir)`** — the free-disk figure and the path it was actually measured at. The path is mandatory wherever the number is shown |

### 2.18 `environment`

| Field | Type | Source |
|---|---|---|
| `ft_found` | boolean | `app.ft.is_some()` |
| `ft_program` | string \| null | `app.ft.program` |
| `ft_display` | string \| null | `app.ft.display_program()` — how the command reads, e.g. `ft` or `/venv/bin/python -m freetoken.cli` |
| `ft_origin` | string \| null | `app.ft.origin` — where it was found |
| `ft_error` | string \| null | **`app.ft_error`** — why the CLI could not be found. Non-null means every engine, conversion, benchmark and preflight route returns `503` |
| `supported_archs` | string[] \| null | `app.supported_archs` — FreeToken's registry, read once at startup. `null` means it could not be read, and the Hub verdict says support is unverified rather than inventing one |
| `hub_token_present` | boolean | `app.hub_token.is_some()` |
| `hub_token_source` | string \| null | `HubToken::source` — `"the HF_TOKEN environment variable"`, `"hub.token in the config"`, `"the token cached by the hf CLI"`. **The token value is never sent** |
| `endpoint` | string | `app.client.base_url()` — the polled URL, repeated here for the status bar |
| `hostname` | string | `app.host.hostname` |
| `ft_version` | string \| null | `app.ft_version` — the first line of `ft --version`, probed once at startup |

The rest of the block describes the FreeToken **git checkout** this machine builds from,
for an install built from source rather than from a wheel. The tree is found at run time
(`freetoken.checkout`, else the tree enclosing `freetoken.venv` or the `ft` binary, else a
development `vendor-freetoken/`), read on a background timer (`freetoken.checkout_poll_min`,
default 30), and every field below is `null` when no such tree was found.

| Field | Type | Source |
|---|---|---|
| `ft_checkout_path` | string \| null | `FtCheckout::path` — which tree was read. Shown, because it is resolved rather than configured |
| `ft_local_sha` | string \| null | `FtCheckout::local_sha` — the commit the working tree is on |
| `ft_upstream_sha` | string \| null | `upstream/main` after a fetch, or `""` when there is no such remote |
| `ft_origin_sha` | string \| null | `origin/main` after a fetch |
| `ft_upstream_behind` | number \| null | Commits from `local_sha` to `upstream/main`. **Non-zero is the "a newer FreeToken exists" signal** |
| `ft_origin_ahead`, `ft_origin_behind` | number \| null | `git rev-list --left-right HEAD...origin/main` |
| `ft_dirty` | boolean \| null | The working tree has uncommitted changes |
| `ft_kernels_stale` | boolean \| null | The newest built `.so` under `python/freetoken/kernel/` predates the last commit to touch `kernel/csrc/`: pulled, but not rebuilt, so the engine is not running the code on disk. `null` when nothing is built to compare against |
| `ft_checkout_note` | string \| null | The one-line summary the panes gate on, worst-first: behind upstream, then kernels stale, then diverged from origin, then dirty, then `"up to date"` |

---

## 3. Incremental endpoints

Three collections are append-only and unbounded-ish, so they are fetched by sequence
number rather than carried in the snapshot. All three share one convention:

* The server keeps a monotonically increasing `seq` per stream, counted over *every* item
  ever appended, not over what is currently retained. Sequence numbers are never reused
  and never renumbered when the ring drops from the front.
* A request asks for everything after a sequence: `?after=<seq>&limit=<n>`. `after=0`
  means "from the oldest retained item".
* The reply always states what is retained, so a client can detect a gap:

```json
{ "items": [...], "first_seq": 4096, "last_seq": 5120, "dropped": 4095, "next_after": 5120 }
```

**Gap detection**: the client holds the `seq` of the last item it rendered. If
`first_seq > held_seq + 1`, items were lost between the two polls; the client discards its
buffer, renders what came back, and may show an elision marker ("… 128 lines dropped"),
computed as `first_seq - held_seq - 1`. A client that has rendered nothing yet starts at
`after=0` and has no gap by definition.

Polling cadence is the client's choice; once per snapshot arrival is the intended one,
gated on `logs.last_seq` / `requests.last_seq` having moved.

### 3.1 `GET /api/logs`

The engine's merged stdout/stderr, `app.engine.log`.

Query: `after` (number, default 0), `limit` (number, default 500, max 2000).

`LogRing` gains a sequence number: each `LogRing::push` assigns the next `seq` before the
line is appended, and the ring records how many lines it has evicted so `dropped` is exact
across both eviction and `clear`.

```jsonc
{
  "items": [ { "seq": 5118, "text": "INFO: loading weights", "err": false, "severity": "normal" } ],
  "first_seq": 4096, "last_seq": 5120, "dropped": 4095, "next_after": 5120
}
```

| Field | Type | Source |
|---|---|---|
| `seq` | number | Assigned at push |
| `text` | string | `LogLine::text`, unmodified — no truncation, no wrapping, no ANSI stripping |
| `err` | boolean | `LogLine::err`, true for stderr |
| `severity` | `"error" \| "warn" \| "meta" \| "normal"` | **`views::logs::classify(text, err)`** — the same function that picks the terminal's color for the same line. Render it; do not re-derive it |

`classify` in order: a line starting `[ft-man]` is `meta` (ft-man's own, and it wins over
its content — the exit line names a status, not an error); `is_error_text` — `ERROR`,
`CRITICAL`, `Traceback`, `Exception` — is `error`; `WARNING` or `WARN` is `warn`; anything
else is `normal`. **`err` decides nothing**: FreeToken logs its whole life to stderr, so
coloring on the stream would paint every informational line as a problem.

Filtering (`/`), errors-only (`e`), wrapping (`w`), follow (`f`) and scrolling are all
browser-side over the fetched lines, exactly as the TUI computes them over its snapshot.
The errors-only rule is the TUI's and is *not* the same as `severity`: it keeps a line when
`err || is_error_text(text)`, so a stderr line with nothing alarming in it is still shown
there while it renders as `normal`.

**`POST /api/logs/clear`** — body `{}`. Calls `app.engine.log.clear()`. `dropped` absorbs
the cleared lines and `first_seq`/`last_seq` both become `last_seq`, so a client's held
sequence is still comparable. Reply `200 {"status": "ok"}`.

### 3.2 `GET /api/jobs/{id}/output`

The tail of one job's output. `id` is `Job::id`; `404` when no such job exists.

Query: `offset` (number of bytes, default 0), `limit` (number of bytes, default 65536, max
1 MiB).

The source is the job's log **file** (`Job::log_path`), not the in-memory ring, because
the file survives a daemon restart and a byte offset is a stable cursor into it. Two things
must be reconciled with what the TUI's output pane shows:

* The TUI renders `job.log.snapshot()`, which excludes the machine-readable progress
  protocol — `spawn_job_reader` intercepts `FTCONVERT …`, `FTBENCH …` and `FTBENCH_OUT …`
  before they reach the ring, but writes them to the file. **The server therefore drops
  any line beginning with `FTCONVERT `, `FTBENCH ` or `FTBENCH_OUT ` from the response**,
  so the browser sees what the TUI sees.
* `err` cannot be recovered from a file whose streams are merged. Lines read from the file
  report `err: false`, and the browser colors by content (the `is_error_text` rule above),
  which is what `views::logs::line_style` already does for the same reason: Python logging
  writes everything to stderr, so the stream says nothing.

```jsonc
{
  "id": 3,
  "offset": 0,
  "next_offset": 20480,
  "eof": true,
  "truncated": false,
  "lines": [
    {"text": "[ft-man] $ ft checkpoint --model ...", "severity": "meta"},
    {"text": "converting layer 3", "severity": "normal"}
  ]
}
```

| Field | Type | Meaning |
|---|---|---|
| `offset` | number | The offset the read started at, after clamping to the file size |
| `next_offset` | number | Byte offset to pass next time, always on a line boundary |
| `eof` | boolean | True when `next_offset` is the end of the file |
| `truncated` | boolean | True when the requested `offset` was past the end of the file (the file was rotated or removed) and the read restarted at 0 |
| `lines` | `{text, severity}[]` | Complete lines, progress protocol removed, each classified by the same `views::logs::classify` section 3.1 describes — with `err` false, because a file whose streams are merged cannot recover it |

Three rules about where a read stops, because each of them was a way for a client to get
stuck or to render half a sentence:

* **While the job is running, a partial trailing line is withheld.** It is half of
  something, the rest arrives on the next poll, and `next_offset` does not count it.
* **Once the job has stopped, it is returned.** Nothing more is coming, and the last line a
  crash printed is usually the only one worth reading; withholding it would hide it forever.
* **A single line longer than `limit` is returned whole.** The read widens past `limit`
  until it finds that line's ending, then stops at it. The alternative is a reply with no
  lines and `next_offset == offset` — the same request, forever.

**When to poll.** `JobEntry.output_seq` (section 2.14) is the job's own line counter. A
client fetches once when it selects a job, then again whenever `output_seq` differs from
the value it last fetched at. It moves with the job's final status line, so no extra read is
needed after the job stops. There is no timer: an unchanged counter means nothing new was
written.

A `{id}` that is not a number is `400` **in the JSON envelope**, not axum's plain-text
rejection; an id that is a number but names no job is `404`.

The header the output pane shows above the text — command line, log path, bench profile
path — comes from the snapshot's `JobEntry` (`command_line`, `log_path`, `output_path`),
not from this route.

With no job selected the TUI's output pane shows the machine's bandwidth profile instead;
that is `hardware.bench_profile` in the snapshot, and needs no request.

### 3.3 `GET /api/requests`

The engine's request ring as `App` holds it (`app.requests_view.entries`).

Query: `after` (number, default 0), `limit` (number, default 200, max 512).

The server-side `seq` is **not** the engine's `/v1/requests` cursor. It is ft-man's own
count of records appended to `requests_view.entries` since the process started: each entry
pushed in `Message::Requests` gets the next sequence. This makes it resumable across an
engine restart, which resets the engine cursor to 0, and across the front-eviction the ring
does at 512 entries.

```jsonc
{
  "items": [ { "seq": 941, "ts": "2026-09-13T14:23:07.123456Z", "method": "POST",
               "path": "/v1/chat/completions", "status": 200, "model": "Qwen3.6-35B-A3B",
               "duration_ms": 8340, "ttft_ms": 412, "prompt_tokens": 70123,
               "completion_tokens": 512, "stream": true, "error": null,
               "decode_tps": 61.4 } ],
  "first_seq": 430, "last_seq": 941, "dropped": 429, "next_after": 941
}
```

Each item is `ft::types::RequestRecord` verbatim plus:

| Field | Type | Source |
|---|---|---|
| `seq` | number | Assigned on append |
| `decode_tps` | number \| null | `completion_tokens / (duration_ms / 1000)` — the detail pane's "Decode rate", derived server-side because the record does not carry it. `null` unless both `duration_ms > 0` and `completion_tokens > 0` |

`clock(ts)` (the `HH:MM:SS` column) and the `ms()` formatting are presentation and stay in
the browser.

**`POST /api/requests/pause`** — body `{"paused": true}`. Sets
`app.requests_view.paused`, and the new value appears in the next snapshot. Reply
`200 {"paused": true}`.

Semantics, stated exactly because the TUI's are narrow: `paused` is a *display* flag. The
telemetry poller keeps calling `/v1/requests` and keeps appending to the ring — pausing
does not stop collection, and nothing is lost while paused. The TUI uses the flag only to
label the pane `Requests (412) — paused`; the browser should additionally stop
auto-scrolling to the newest row while it is set, which is what a reader who pressed `p`
wants. When the client unpauses it resumes from its held `seq` and catches up.

**`POST /api/requests/clear`** — body `{}`. Clears `app.requests_view.entries`. As with
logs, `dropped` absorbs the cleared entries so sequence comparison keeps working. Reply
`200 {"status": "ok"}`.

---

## 4. Actions

Every route in this section is `POST`, takes a JSON body, and returns `200` with a small
typed reply on success. Three reply shapes exist:

```jsonc
{"status": "ok"}                                    // done
{"status": "started", "job_id": 4}                  // a task is now running; watch the snapshot
{"status": "confirm_pending"}                       // app.confirm is now set; nothing has happened
```

When `config.ui.confirm_destructive` is false, a route that would have raised a
confirmation performs the action immediately and returns `{"status":"ok"}` (or
`"started"`), mirroring `input::ask`. **A route that may return `confirm_pending` is marked
"⚠ confirms" below, and the frontend must treat a `200` from it as "look at the next
snapshot's `confirm`", not as "done".**

### 4.1 Route index

| # | Route | TUI key | `input.rs` / `App` function |
|---|---|---|---|
| 1 | `GET /api/auth` | — | — |
| 2 | `POST /api/login` | — | — |
| 3 | `POST /api/logout` | — | — |
| 4 | `GET /api/snapshot` | — | snapshot builder |
| 5 | `GET /api/events` | — | snapshot builder |
| 6 | `GET /api/knobs` | — | `knobs::KNOBS` |
| 7 | `GET /api/logs` | — | `app.engine.log` |
| 8 | `POST /api/logs/clear` | Logs `c` | `logs_key` |
| 9 | `GET /api/requests` | — | `app.requests_view.entries` |
| 10 | `POST /api/requests/pause` | Requests `p` | `requests_key` |
| 11 | `POST /api/requests/clear` | Requests `c` | `requests_key` |
| 12 | `GET /api/jobs/{id}/output` | Jobs `Tab` pane | `views::jobs::output` |
| 13 | `GET /api/templates/preview` | Templates cursor move | `App::refresh_template_preview` |
| 14 | `POST /api/confirm` | `y`/`n`/`Enter`/`Esc` in a modal | `confirm_key` → `run_action` |
| 15 | `POST /api/engine/start` | Dashboard `e`, Serve `g`, Models `s` | `input::start_engine` |
| 16 | `POST /api/engine/stop` | Dashboard `s` / `S` | `request_stop` |
| 17 | `POST /api/engine/smoke-test` | Dashboard `t` | `smoke_test` |
| 18 | `POST /api/models/rescan` | Dashboard `r`, Models `r` | `App::request_scan` |
| 19 | `POST /api/models/use` | Models `Enter` / `s` | `use_selected_model` |
| 20 | `POST /api/models/convert` | Models `c` | `convert_selected` → `begin_conversion` |
| 21 | `POST /api/models/delete` | Models `D` | `delete_selected_model` |
| 22 | `POST /api/hub/search` | Hub `/` + `Enter` | `start_search` |
| 23 | `POST /api/hub/open` | Hub `Enter` | `load_repo_files` + `check_compatibility` |
| 24 | `POST /api/hub/variant` | Hub `Enter` / `Space` in Variants | `choose_variant` |
| 25 | `POST /api/hub/files/toggle` | Hub `Space` in Files | `hub_files_key` |
| 26 | `POST /api/hub/files/select` | Hub `a` / `n` | `hub_files_key` |
| 27 | `POST /api/hub/download` | Hub `d` | `begin_download` |
| 28 | `POST /api/hub/install-cli` | Hub `i` | `offer_hf_install` |
| 29 | `POST /api/downloads/cancel` | Jobs `x` on a download | `cancel_selected` |
| 30 | `POST /api/templates/list-repo` | Templates `r` + `Enter` | `list_template_repo` |
| 31 | `POST /api/templates/fetch` | Templates `f` / `Enter` | `fetch_template` |
| 32 | `POST /api/templates/apply` | Templates `a` | `apply_template` → `write_template` |
| 33 | `POST /api/templates/revert` | Templates `u` | `request_revert_template` → `revert_template` |
| 34 | `POST /api/templates/verify` | Templates `v` | `verify_template` → `run_preflight` |
| 35 | `POST /api/templates/delete` | Templates `D` | `stored_templates_key` |
| 36 | `POST /api/serve/knob` | Serve `Enter` on a value, `x` / `Del` / `Backspace` | `commit_knob_edit`, unset branch |
| 37 | `POST /api/serve/flag` | Serve `Enter` / `Space` on a flag | `begin_knob_edit`, `cycle_knob` |
| 38 | `POST /api/serve/cycle` | Serve `Space` on a choice | `cycle_knob` |
| 39 | `POST /api/serve/plan` | Serve `a` | `build_plan` → `views::plan::build` |
| 40 | `POST /api/serve/plan/apply` | Plan overlay `A` | `apply_plan` → `Plan::apply` |
| 41 | `POST /api/serve/plan/dismiss` | Plan overlay `Esc` / `q` / `a` | `handle_key` plan branch |
| 42 | `POST /api/profiles/save` | Serve `S` + name + `Enter` | `save_profile` |
| 43 | `POST /api/profiles/load` | Serve `P`, profiles `Enter` | `load_profile` |
| 44 | `POST /api/profiles/delete` | Profiles `D` | `profiles_key` |
| 45 | `POST /api/cache/pending` | Cache `r` (reset one), slider set | `CacheView::set_pending` |
| 46 | `POST /api/cache/adjust` | Cache `←` / `→`, `Shift+←/→` | `views::cache::adjust` |
| 47 | `POST /api/cache/reset-all` | Cache `R` | `CacheView::clear_pending` |
| 48 | `POST /api/cache/apply` | Cache `a` / `Enter` | `cache_key` → `apply_cache_rebuild` |
| 49 | `POST /api/jobs/bench` | Jobs `b` | `run_bench` |
| 50 | `POST /api/jobs/cancel` | Jobs `x` on a job | `cancel_selected` |
| 51 | `POST /api/jobs/clear-finished` | Jobs `X` | `jobs_key` |

**51 routes.** Keys with no route are browser-local and are listed in section 5.10.

### 4.2 `GET /api/knobs`

The static `ft serve` knob schema, served once and cached by the client for the lifetime of
the connection (it cannot change without restarting the daemon). `ETag` is the ft-man
version.

```jsonc
{
  "groups": [ {"group": "model", "title": "Model"}, ... ],          // knobs::Group::ALL, Group::title()
  "knobs": [
    {
      "key": "moe_cache_size",
      "flag": "--moe-cache-size",
      "label": "MoE cache size (slots)",
      "group": "moe",
      "kind": {"kind": "int", "min": 0, "max": null},
      "default": "auto",
      "help": "Absolute number of GPU expert slots.",
      "exclusive_with": ["moe_cache_size", "moe_cache_rate", "moe_cache_auto"]
    }
  ]
}
```

Knobs are in `KNOBS` order, which is the order the Serve view presents them within each
group. `exclusive_with` is verbatim, including the knob's own key where the schema lists it
— the UI filters that out when it renders "excludes", as `views::serve::help` does.

The Serve tab's right-hand "What it does" pane is built entirely from this document plus
the browser's own cursor: `flag`, `help`, `default`, the `choice` options, and the
exclusions.

### 4.3 `POST /api/confirm`

Body: `{"accept": true}`.

* `accept: true` runs the pending `ConfirmAction` through the same `run_action` the TUI
  uses, then clears `app.confirm`.
* `accept: false` clears `app.confirm` and does nothing else.
* `409` when no confirmation is pending — a stale modal in a second browser tab.

Reply `200 {"status": "ok"}` or `{"status": "started", ...}` when the confirmed action
spawned a task (a conversion, the `hf` install, a cache rebuild).

### 4.4 Engine

**`POST /api/engine/start`** — body `{}`. Mirrors `input::start_engine`.

Every refusal below comes from **`actions::start_blocked`**, the same predicate
`engine.start_blocked` (section 2.5) is built from, checked in this order:

| Case | Result |
|---|---|
| An engine is already live | `409 "an engine is already running; stop it first"` (and the same warn toast) |
| The state file names a live engine this process does not own | The route re-reads `serve.json` first, **adopts** that engine, and then refuses it as the case above. This is the cross-process race: a terminal and the daemon run side by side on one machine, and a start that trusted the last tick would put a second engine on the same GPU and port |
| A job is using the GPU | `409 "a convert job is using the GPU; wait for it or cancel it first"` |
| No FreeToken CLI | `503` with `app.ft_error` as the message |
| `serve.validate()` non-empty | `409` with `"--model: a model path or repo id is required"` — the first error, flag-prefixed, exactly as the TUI toasts it |
| Otherwise | `200 {"status":"started","log_path":"..."}`. Telemetry, the series and the request ring are reset, as in the TUI |

Never confirms.

**`POST /api/engine/stop`** — body `{"force": false}`. ⚠ confirms.

* `409 "no engine is running"` when `!engine.is_live()`.
* Otherwise raises the stop confirmation (`Stop the engine` / `Force-stop the engine`),
  whose body names the model and, for an adopted engine, says it was re-attached to.
  `force: true` marks the confirmation destructive.
* On accept: `Engine::stop(force)` — SIGINT then the SIGTERM/SIGKILL escalation, or SIGKILL
  outright.

**`POST /api/engine/smoke-test`** — body `{}`. Mirrors `smoke_test`.

* `409 "the server is not answering"` when `!app.server_reachable()`.
* Otherwise `200 {"status":"started"}`; the result arrives as a success or error toast
  (`Client::generate("The capital of France is", 16)`).

### 4.5 Models

**`POST /api/models/rescan`** — body `{}`. `App::request_scan()`. A no-op while a scan is
already running. `200 {"status":"started"}`.

**`POST /api/models/use`** — body `{"path": "...", "and_serve": false}`.

* `404` when no model in `app.models` has that path.
* `409` when `Model::is_partial()` — `"… is an incomplete conversion and cannot be served;
  delete it with D"` (the web UI should say "delete it" rather than name the key).
* Otherwise sets `serve.model` to the FTW build when one exists (`Model::converted_to`),
  else the checkpoint path. Emits the same info toast when the FTW build was preferred.
* `serve.served_model_name` is set to `Model::served_name()` **only when ft-man is the one
  that put the current value there** — that is, when the knob is unset, or when its value
  equals some library model's `served_name()`. A name typed by hand or loaded from a profile
  is the API this engine publishes, and clients send it in request bodies; picking a
  different checkpoint must not silently rewrite it.
* `and_serve: true` then runs the engine-start path, with every refusal of
  `POST /api/engine/start` applying. Reply `200 {"status":"ok"}` or, when the engine
  started, `{"status":"started"}`.

**`POST /api/models/convert`** — body `{"path": "..."}`. ⚠ confirms. Mirrors
`convert_selected`:

| Case | Result |
|---|---|
| Not found | `404` |
| `model.format != Hf` | `409 "… is already in FTW format; conversion only applies to Hugging Face checkpoints"` |
| `app.gpu_busy_reason()` is set | `409 "cannot convert: <reason>"` |
| A preflight is already in flight | `409 "a checkpoint check is already running"` |
| The FTW output exists and is a *complete* build | `409 "… already exists; delete it from the Models tab to reconvert"` |
| The FTW output exists and is *partial* | `200 {"status":"started"}` — sizing the leftovers is a full tree walk, so it runs on the blocking pool and the `Retry conversion` confirmation appears in a later snapshot rather than in this reply. Accepting it removes the leftovers (also on the blocking pool) and converts again (`ConfirmAction::ReconvertModel`) |
| `config.convert.preflight` is on | `200 {"status":"started"}` — the preflight runs; a clean result starts the job silently, a warn or fail raises the `Convert anyway?` confirmation |
| Preflight off, or no interpreter to run it with | `200 {"status":"started","job_id":N}` — `ft checkpoint` is spawned |

The spawned command matches the TUI's: `--model`, `--out`, `--moe-backend` (`triton` when
`serve.moe_strategy == "fused"`, else `offload`), plus `--quant-backend` and `--gpu` when
those knobs are set.

**`POST /api/models/delete`** — body `{"path": "..."}`.

| Case | Result |
|---|---|
| Not found | `404` |
| The path is inside a Hugging Face hub cache (`templates::is_hub_cache_path`) | `409`, naming `hf cache delete <repo>`. The cache belongs to `huggingface_hub`: a snapshot directory is symlinks into `blobs/`, so `remove_dir_all` frees the links, leaves the blobs, and breaks `refs/`. ft-man reads that tree and does not write it |
| Otherwise | `200 {"status":"started"}` |

Not `confirm_pending`: the confirmation's body quotes `models::dir_size`, which is a
recursive walk of a directory holding hundreds of gigabytes. It runs on the blocking pool —
neither the TUI's event loop nor the web daemon's one `App` mutex may be held for it — and
the destructive `Delete checkpoint` modal, naming the path and the space it frees, arrives
in a later snapshot. Accepting it returns `{"status":"started"}` too: `remove_dir_all` is
tens of thousands of `unlink`s and goes to the blocking pool as well, with the outcome
arriving as a toast and a rescan.

### 4.6 Hub

**`POST /api/hub/search`** — body `{"query": "Qwen3.6-35B-A3B"}`.
`400` on an empty query after trimming. Otherwise sets `hub_view.query`, marks
`searching`, and spawns `Hub::search(query, 50)`. `200 {"status":"started"}`. Results
(or an error toast, or the "no models matched that search" warning) arrive in the snapshot.

**`POST /api/hub/open`** — body `{"repo_id": "org/name", "revision": "main"}`.
`revision` is optional and defaults to `hub_view.revision` (`"main"`). Mirrors
`load_repo_files`: sets the revision, clears the stale verdict, spawns both the
`Hub::info` fetch and the compatibility check (which fetches only `config.json` and
evaluates it against `app.supported_archs` and this machine's VRAM, host RAM and free
disk). Warns when the repo is gated and no token is configured. `200 {"status":"started"}`.
`503` when the Hub client cannot be built.

**`POST /api/hub/variant`** — body `{"label": "UD-IQ3_XXS"}`.
`409` when the current layout has no such weights variant. Otherwise applies
`Layout::files_for(label)` to `hub_view.files` (`wanted` true for exactly those paths),
sets `variant`, clears `custom_selection`, and emits the same info toast
(`"UD-IQ3_XXS: 4 file(s), 18.2 GiB"`). `200 {"status":"ok"}`.

**`POST /api/hub/files/toggle`** — body `{"path": "model-00001-of-00003.gguf", "wanted": true}`.
`wanted` is optional; omitting it toggles, matching `Space`. `404` when the path is not in
the current listing. Sets `custom_selection = true`. `200 {"status":"ok","wanted":true}`.

**`POST /api/hub/files/select`** — body `{"mode": "all"}` or `{"mode": "none"}`.
The `a` and `n` keys. Sets `wanted` on every file and `custom_selection = true`.
`200 {"status":"ok","selected_count":61}`.

**`POST /api/hub/download`** — body:

```jsonc
{ "repo_id": "unsloth/Qwen3.8-Flash-Next-GGUF",
  "revision": "main",
  "variant": "UD-IQ3_XXS",          // optional
  "files": ["...", "..."] }          // optional
```

Resolution order, so the browser can send whichever it has:

1. When `files` is present and non-empty, exactly those paths are downloaded. Any path not
   in the repo's listing is a `400`.
2. Otherwise, when `variant` is present, the server expands it with
   `Layout::files_for(variant)`.
3. Otherwise the currently `wanted` files in `hub_view.files` are used — the TUI's
   behavior.

Refusals mirror `begin_download`:

| Case | Result |
|---|---|
| No repo has been opened and the body names none | `409 "select a repo and press Enter to list its files first"` |
| The resolved set is empty | `409 "no files selected"` |
| That repo is already downloading | `409 "<repo> is already downloading"` |
| `app.hf_cli` is `None` | `503 "the hf CLI is not installed"` |
| Otherwise | `200 {"status":"started"}` — the download registers a moment later (`Message::RegisterDownload`) and appears in `jobs.downloads` |

Files always land in the Hugging Face cache (`config.library.hub_cache()`), never in an
arbitrary directory; `hub.target` is informational. The `hf` child is given `HF_ENDPOINT`
= `config.hub.endpoint` alongside `HF_HUB_CACHE`, so the weights come from the same Hub
ft-man listed the repo from — otherwise a configured mirror decided what was offered and
huggingface.co delivered it.

**`POST /api/hub/install-cli`** — body `{}`. ⚠ confirms. `409` when `hf` is already
present or an install is in flight. Otherwise the `Install the Hugging Face CLI`
confirmation, whose body quotes `hub::INSTALL_COMMAND` and explains what the script does.
On accept the installer runs and `hf_installing` goes true until `Message::HfInstalled`
lands.

**`POST /api/downloads/cancel`** — body `{"id": 1}`. ⚠ confirms.
`404` for an unknown id; `409 "that download has already finished"` when it is not running.
Otherwise the (non-destructive) `Cancel download` confirmation — completed files are kept
and a partial file resumes.

### 4.7 Templates

**`POST /api/templates/list-repo`** — body `{"repo": "org/templates"}`.
`400` on empty. Sets `templates_view.repo`, marks `loading`, fetches the repo's `.jinja`
listing at `main`, and records the resolved commit as `remote_revision`. Warns when the
repo holds no `.jinja` files. `200 {"status":"started"}`.

**`POST /api/templates/fetch`** — body
`{"repo": "org/templates", "revision": "<sha>", "path": "qwen/chat_template.jinja"}`.
`revision` is optional, defaulting to the listing's `remote_revision`. Downloads the file
and saves it into the store under `templates::name_for(repo, path)`, with provenance in
`TemplateMeta`. `200 {"status":"started"}`; a success toast and the refreshed store arrive
in the snapshot.

**`POST /api/templates/apply`** — body `{"template": "qwen-sharp", "model_path": "/models/x"}`.
⚠ confirms. This is the route that replaces the TUI's cross-tab dependency: both
identities are explicit.

| Case | Result |
|---|---|
| No stored template by that name | `404 "no template named …"` |
| No model at that path | `404 "that model is no longer in the library"` |
| `templates::targets(model)` is empty | `409 "… has no directory to write a template into"` |
| Otherwise | `200 {"status":"confirm_pending"}` |

The confirmation body is built exactly as `apply_template` builds it, and the frontend
renders it verbatim because it carries the facts the decision needs: every target directory
(`templates::targets`), what the checkpoint's current template is
(`templates::status` — built-in preserved / a foreign file will be backed up / which
override is being replaced), the extra paragraph when a target is inside the Hugging Face
cache (`templates::is_hub_cache_path`), and the reminder to restart a live engine.

On accept: `templates::apply` into every target, then — when `config.templates.preflight`
is on — a render check against the first target, whose outcome lands in
`templates.preflight`.

**`POST /api/templates/revert`** — body `{"model_path": "/models/x"}`. ⚠ confirms.
`404` when unknown; `409 "… is not using an ft-man template override"` when
`templates::status(path)` is not `Overridden`. Otherwise the `Restore built-in template`
confirmation naming every directory. On accept, `templates::revert` runs over each target
that is actually overridden, and the last preflight result is cleared.

**`POST /api/templates/verify`** — body `{"template": "qwen-sharp", "model_path": "/models/x"}`.
`404` for either unknown identity. `503 "cannot verify the template without the FreeToken
CLI"`, or `503 "…: no Python found beside the FreeToken CLI"` when there is no interpreter.
Otherwise `200 {"status":"started"}`; `templates.checking` goes true and the result appears
as `templates.preflight` plus a toast.

**`POST /api/templates/delete`** — body `{"name": "qwen-sharp"}`. ⚠ confirms. `404` when
unknown. The destructive `Delete template` confirmation; the body states that checkpoints
it was already applied to keep using it.

**`GET /api/templates/preview?name=<name>`** — the head of a stored template, for the
preview pane, for any template rather than only the one `App` has cached.
`200 {"name": "...", "text": "...", "truncated": false}`, capped at 8 KiB. `404` when
unknown. Reading it also refreshes `App`'s own cached preview, so the TUI and the browser
agree.

### 4.8 Serve configuration

**`POST /api/serve/knob`** — body `{"key": "kv_reserve_tokens", "value": "262144"}`.

* `value: null` (or `""`) **unsets** the knob, which is what `x`, `Del` and `Backspace` do,
  and what committing an empty editor does. Reply `200 {"status":"ok","set":false}`.
* Otherwise the value is trimmed and checked with **`knobs::validate_value(knob, value)`**.
  A failure is `409 {"error": "--kv-reserve-tokens: must be at least 0"}` — the flag
  spelling followed by the message — and nothing is stored.
  **This refusal pushes no toast** (the exception in section 1.2). The message belongs
  under the field that produced it, where the reader is already looking and where it
  disappears when the value is corrected; a floating toast beside it would be the same
  problem rendered twice, and a toast cannot say *which* field when two are wrong. The
  terminal still toasts, because a terminal has no inline slot: `actions::set_knob`
  returns the `Refusal` silently and `input::commit_knob_edit` raises the toast on the TUI
  side. The behavior of the `Enter` key is unchanged.
* On success `ServeConfig::set` runs, which also clears every knob in `exclusive_with`
  (setting `--moe-cache-size` clears `--moe-cache-rate` and `--moe-cache-auto`). Reply
  `200 {"status":"ok","set":true,"cleared":["moe_cache_rate"]}`.
* **`set` and `cleared` are read back after the write, not predicted from it.**
  `ServeConfig::set` has one case that stores nothing: a `Flag` given `"false"` *unsets* the
  knob and clears nothing. `"false"` is a valid value for a flag — `knobs::validate_value`
  accepts it — so the reply there is `{"set": false, "cleared": []}`, and a browser that
  believed `set: true` rendered a ticked box for a flag that was off.
* `404` for an unknown key. Both this and the validation `409` carry `"toasted": false`.

**`POST /api/serve/flag`** — body `{"key": "moe_cache_auto"}`, optionally `{"on": true}`.
`409` when the knob is not `Kind::Flag`. Without `on` it toggles
(`ServeConfig::toggle_flag`), matching `Enter` and `Space`; with `on` it sets the state
outright, which is what a checkbox wants. Reply `200 {"status":"ok","on":true}`. Turning a
flag on emits the TUI's `"MoE cache auto-size on"` info toast.

**`POST /api/serve/cycle`** — body `{"key": "moe_strategy", "delta": 1}`.
`409` when the knob is not `Kind::Choice`. Walks the option list by `delta` (±1) and wraps
*through unset*, exactly as `cycle_knob`: from unset, `+1` selects the first option and
`-1` the last; stepping off either end unsets the knob, so there is always a way back to
the default. Reply `200 {"status":"ok","value":"hybrid"}` or `{"value":null}` when it
landed on unset. For a `Flag` knob this route is accepted and behaves as a toggle, which is
what `cycle_knob` does.

**`POST /api/serve/plan`** — body `{}`. Runs `views::plan::build(app)`, which is pure and
instant — it needs no job and raises no confirmation.

| Case | Result |
|---|---|
| No model configured | `409 "cannot plan: no model is configured — set one on the Serve or Models tab"` |
| The plan is empty and nothing is unpriced | `200 {"status":"ok","plan":false}` plus the success toast `"nothing to change — this configuration is already optimal here"`. `serve.plan` stays `null` |
| Otherwise | `200 {"status":"ok","plan":true}`; `serve.plan` is populated in the next snapshot |

**`POST /api/serve/plan/apply`** — body `{}`. `409` when no plan is held. Otherwise
`Plan::apply(&mut serve)` folds every edit in, the plan is cleared, and the reply says how
many knobs actually changed: `200 {"status":"ok","changed":3}`. `changed: 0` is the TUI's
"the configuration already matched the plan".

**`POST /api/serve/plan/dismiss`** — body `{}`. Clears `serve_view.plan`. Always
`200 {"status":"ok"}`, whether or not one was held.

### 4.9 Profiles

**`POST /api/profiles/save`** — body `{"name": "qwen-256k"}`.
`400` on an empty name (`"a profile needs a name"`). Upserts
`Profile { name, notes: "", serve: current }`, sets `last_used`, writes `profiles.toml`.
Reply `200 {"status":"ok","created":true}` (`false` when it replaced an existing profile,
which is the difference between the TUI's "saved" and "updated" toasts). A write failure is
`500` and the configuration in memory is still updated, as in the TUI.

The TUI pre-fills the name field with the model's basename; the browser can do the same
from `serve.values.model`, or send anything.

**`POST /api/profiles/load`** — body `{"name": "qwen-256k"}`. `404` when unknown. Replaces
`app.serve` wholesale with the profile's, sets `last_used`, saves. Reply
`200 {"status":"ok"}`. Note that a profile written against an older FreeToken is migrated
on the way in (`knobs::migrate`), so the loaded values may differ from the file.

**`POST /api/profiles/delete`** — body `{"name": "qwen-256k"}`. ⚠ confirms. `404` when
unknown. The destructive `Delete profile` confirmation. On accept, the profile is removed,
`last_used` is cleared when it pointed at it, and `profiles.toml` is rewritten.

### 4.10 Cache

Every route here requires a live geometry (`telemetry.cache_status`); without one they
return `503 "cache geometry is only available while the engine is serving"`, which is the
tab's own message. A pool the geometry does not expose (`pool_present` is false) is `404`.

**`POST /api/cache/pending`** — body `{"pool": "kv", "value": 8192}`.

* `value: null` clears the pending edit for that pool — the `r` key.
* A number is clamped into `[min, max]` by `cache_pools::PoolGeometry::clamp` — literally
  the `PoolRow.min` and `PoolRow.max` the snapshot sent, from the one module that converts
  FreeToken's published limits into the pool's own unit — and is stored as `None` when it
  equals `current`, matching `views::cache::adjust`'s behavior of treating "back to where it
  started" as no edit.
* Reply `200 {"status":"ok","pending":8192}` (or `"pending":null`).

**`POST /api/cache/adjust`** — body `{"pool": "kv", "percent": 0.01}`.
The arrow keys: `±0.01` normally, `±0.10` with Shift. The step is `round(max * |percent|)`,
at least 1, applied to `pending ?? current`, clamped to `[min, max]`, and cleared when the
result equals `current` — `views::cache::adjust` verbatim, against the same bounds. Reply as above. This route
exists so a keyboard-driven web UI nudges by exactly the same amount the TUI does; a
slider should use `/api/cache/pending` instead.

**`POST /api/cache/reset-all`** — body `{}`. `CacheView::clear_pending()`.
`200 {"status":"ok"}`.

**`POST /api/cache/apply`** — body `{}`. ⚠ confirms.
`409 "nothing to apply"` when nothing is pending. Otherwise the `Rebuild cache`
confirmation, whose body lists every pending pool as `label: current → pending unit` and
then says either how many requests are in flight (the rebuild will be rejected until they
finish) or that weights stay loaded. On accept, `POST /v1/cache/rebuild` is called with
`rebuild_from_pending` (`mode: "if_idle"`, `timeout: 300`), `cache.applying` goes true, and
the outcome arrives as a toast; a success clears the pending edits.

### 4.11 Jobs

**`POST /api/jobs/bench`** — body `{}`. Mirrors `run_bench`.
`409 "cannot benchmark: <reason>"` from `app.gpu_busy_reason()`; `503` with no FreeToken
CLI. Otherwise spawns `ft bench bw` (plus `--gpu` when that knob is set) and returns
`200 {"status":"started","job_id":4}`.

**`POST /api/jobs/cancel`** — body `{"id": 3}`. ⚠ confirms.
`404` for an unknown job id; `409 "that job has already finished"` when it is not running.
Otherwise the destructive `Cancel job` confirmation, whose body warns that a partly written
FTW directory is left behind and must be deleted before a retry. On accept, `Job::cancel()`
sends SIGINT to the job's process group.

**`POST /api/jobs/clear-finished`** — body `{}`. Retains only running jobs and downloads,
exactly as `X` does, and reports how many rows went:
`200 {"status":"ok","removed":3}`.

---

## 5. Things the frontend must render

Per tab, the panes and the snapshot data behind each, so parity with the TUI needs no Rust
reading. Layout is not prescribed; content is.

### 5.1 Chrome (every screen)

* **Tab bar** — nine tabs in order: Dashboard, Models, Hub, Templates, Serve, Cache, Jobs,
  Requests, Logs. Badges: Jobs shows `active_jobs + active_downloads` when non-zero, Models
  shows `models.items.length` when non-empty, Templates shows `templates.stored.length`
  when non-empty.
* **Status, right-aligned** — a colored dot (`engine.status_class`) plus
  `engine.status_text` · `engine.model ?? "no model"`.
* **Footer hints** — the TUI's context-sensitive hint line; the web equivalent is the
  keyboard shortcuts the page binds. See 5.10.
* **Version** — `snapshot.version`, bottom right.
* **Toasts** — bottom right, newest last, at most four, colored by `kind`, fading on
  `ttl_ms - age_ms`.
* **Confirmation modal** — when `confirm` is non-null: `title`, every line of `body`
  (preserving blank lines), and two buttons from `options`, defaulting to index
  `default_index` with the affirmative one styled dangerous when `destructive`. Both
  buttons post to `/api/confirm`.
* **Plan overlay** — when `serve.plan` is non-null; see 5.5.
* **Help** — the key map in 5.11.

### 5.2 Dashboard

Six panes.

1. **Engine** — `engine.status_text` with its dot; `engine.model`; `engine.endpoint`
   (colored by `server_reachable`); `engine.pid` with "(attached)" when
   `engine.adopted`; uptime from `telemetry.stats.uptime_s` (falling back to
   `health.uptime_s` when ready); a load meter from `telemetry.health_load_ratio` with
   `health.progress.done_bytes / total_bytes` and `health.phase` while loading;
   `telemetry.error` with `telemetry.age_ms` when polling failed; model sampling from
   `stats.model.sampling` (temperature, top_p, top_k, min_p, repetition_penalty); a
   **Shape** line combining context, `stats.model.attn` and `MoE`, where context is
   `engine.context_fit.summary` when truncated (rendered as a warning) and
   `stats.model.ctx` otherwise; and, when truncated, the line
   `KV holds <usable> of <ceiling> — plan a fix on the Serve tab`.
2. **Throughput** — `stats.throughput.decode_tps` with `series.decode_peak`, a sparkline
   over `series.decode_tps`; `stats.throughput.prefill_tps` with a sparkline over
   `series.prefill_tps`.
3. **Cache pools** — meters for KV (`telemetry.kv_used_tokens` / `kv_total_tokens`,
   `kv_ratio`), MoE (`cache_status.geometry.moe_cache_size` against
   `telemetry.total_experts`), GDN state (`stats.mamba`) and SWA (`stats.swa`) — each
   rendered only when its pool has a non-zero total, otherwise an idle bar; then a VRAM
   line, `telemetry.pool_bytes.total` of `geometry.cache_budget_bytes`, broken down as
   `KV / MoE / GDN / SWA`.
4. **GPU (`hardware.gpu_source`)** — per `hardware.gpus`: index, name, a `← engine` marker
   when `uuid == hardware.engine_gpu_uuid`, a VRAM meter (`memory_ratio`, used / total /
   free), a utilization meter, and a facts line (temperature, power / limit, `pcie_link`,
   `short_uuid`). When `gpus` is empty, `hardware.reported_gpus` instead, or "no NVIDIA GPU
   detected". Below: `hardware.bench_summary`, or the prompt to run a benchmark, plus the
   benched core count and CPU vs PCIe ceilings from `bench_profile`.
5. **Activity** — `stats.requests.active` with `engine.completed_rate` as
   `0.31 completed/s`; `completed`; `p95_ms` and `ttft_mean_ms`; `prompt_tokens_total`;
   `completion_tokens_total`; `engine.prefix_reuse.summary` when present (and only then);
   `stats.vram_bytes` when non-zero; a line for running jobs and downloads; the library
   count; and a sparkline over `series.active` labeled "concurrent requests".
6. **Host** — title from `hardware.host.hostname` and `uptime_s`; CPU meter
   (`cpu_percent`, `cpu_cores`, `load_avg[0]`); RAM meter (`memory_ratio`, used / total);
   a swap meter when `swap_total > 0`; and a tail line, `memory_free` "free for expert
   banks" plus the kernel version.

### 5.3 Models

* **Library list** — `models.items`, two rows per entry: format badge (`format_label`,
  colored per format), `name`, a `→` marker when `converted_to` is set, `size_bytes`; then
  a dim `summary`. Title shows the count, `(scanning…)` when `models.scanning`, and
  `n of m` when the browser's filter is active.
* **Filter box** — browser-local, matching the TUI's rule: name, path or `arch` contains
  the needle, case-insensitively.
* **Empty state** — when `models.items` is empty: list `models.roots`, marking each
  `exists: false` as "(does not exist)", and name `models.config_path`.
* **Details** — for the browser's selected entry: `name`, `path`, `format_description`,
  `size_bytes`, `modified_ms`, `arch`, `model_type`, `quant` (uppercased), `num_layers`,
  `num_experts` when `is_moe`, `max_position`, `ftw_fingerprint`,
  `template_status.label`, and `converted_to`. Then the `guidance` bullets in order,
  colored by `level`.

### 5.4 Hub

* **Search box** — `hub.query`; title says `(searching…)` when `hub.searching` and
  `(authenticated)` when `environment.hub_token_present`.
* **Results** — `hub.results`, two rows each: `id` with `gated` and `private` markers;
  then `downloads`, `likes`, the date part of `last_modified`, and `interesting_tags`.
  Empty state names FreeToken's known-good checkpoints and states the token situation from
  `environment.hub_token_source`.
* **Quantization** — rendered only when `hub.layout.is_multi`. Each weights variant:
  `label`, `bytes`, `file_count` ("4 pts"), with a filled marker on `hub.variant` when
  `hub.custom_selection` is false. Title is `Quantization — <variant>` or
  `Quantization — N available, none chosen`.
* **Files** — `hub.files`, each with its `wanted` marker, `path` and `size`. Title
  `Files — <selected_count> of <total> selected, <selected_bytes>`, or `(loading…)` when
  `hub.loading_info`.
* **Compatibility / Download to** — one pane whose title follows `hub.compat`:
  `Compatibility — <verdict_label>`, `— checking…`, `— could not check`, or `Download to`.
  Contents: the verdict, `compat.summary`, up to three `compat.notes` with a marker per
  `level`, or the "nothing known stands in the way" sentence when there are none;
  `hub.compat_error` and its explanation when set; `info.id @ revision (sha[..12])` with a
  gated marker; `hub.target`; the "hf CLI is not installed" warning when `hub.hf_cli` is
  null (or "Installing…" when `hub.hf_installing`); and, when something is selected,
  `selected_bytes` "to download" beside `hub.disk_free.free_bytes` free on
  `hub.disk_free.measured_path` — **the path must be shown with the number**.

### 5.5 Templates

* **Repo bar** — `templates.repo`, `(loading…)` when `templates.loading`.
* **Stored templates** — `templates.stored`, two rows each: `name` and `size`; then
  `subtitle` (version and source). Title carries the count.
* **Repo contents** — `templates.remote`, each `path`, with a `✓` when its name is in
  `templates.remote_stored_names`. Title `In <repo-basename> (n)`.
* **Preview** — for the selected stored template: `meta.version`, `meta.source`,
  `meta.repo_path`, `meta.revision` (first 12 characters), `meta.fetched_at`; then the
  render check — `templates.preflight.outcome.detail` colored by its `kind` when
  `preflight.template` matches, or "running…" when `templates.checking`; then the template
  text from `GET /api/templates/preview`.
* **Apply to** — for the model the browser has chosen as the apply target:
  `template_status.label` as "Currently", and every `template_targets` path as "Writes
  into", with the explanation when there are two. When the last preflight failed and the
  model is overridden, the "restore the checkpoint's own template" warning. With no model
  chosen, the explanation that applying writes `chat_template.jinja` into the checkpoint
  directory.

### 5.6 Serve

* **Groups** — `GET /api/knobs` `groups`, each with `serve.set_counts[group]`.
* **Knob list** — for the active group, the knobs from `/api/knobs` in order. Each row:
  `label` and the effective value, which is `serve.values[key]` when set (`"on"` for a
  flag) and `(<default>)` in muted text when not. A key present in `serve.errors` is
  rendered as an error.
* **What it does** — for the highlighted knob: `flag`, `help`, `default`, the `choice`
  options, and the flags it excludes (from `exclusive_with`, minus itself).
* **Command** — `serve.command_preview`, plus up to four `serve.errors` as
  `<flag>: <message>`. `errors[].key` is whatever key the configuration held, so it is not
  always a knob: `serve.validate` emits `("<key>", "unknown knob")` for a key the schema
  does not know, which a profile from a newer FreeToken or a hand-edited `profiles.toml`
  produces. Resolve the key to a flag through `GET /api/knobs` and **fall back to printing
  the key itself** — an unresolved key must read `moe_fanout_beta: unknown knob`, never
  `undefined: unknown knob`. Such a key has no row in the knob list, so the Command pane is
  the only place it appears, which is why the fallback matters.
* **Profiles** — `serve.profiles`, two rows each: `name` with an `active` marker when it
  equals `serve.last_used_profile`; then `model`. Empty state explains save and load.
* **Plan overlay** — when `serve.plan` is set: the headline from `plan.fit`
  ("Context after this plan — 233.1k of the 256k this model offers (91%)", or "the full
  256k this model offers"); `plan.unpriced` when set; "nothing to change" when
  `plan.is_empty`; then each `plan.steps` entry — a marker per `level`, `label` in bold for
  a step that sets a knob, and `reason` beneath it (wrapped, with a hanging indent);
  finally the apply line using `plan.edit_count`.

### 5.7 Cache

* **Pools** — title `Pools`, `Pools (rebuilding…)` when `cache.state == "rebuilding"`,
  `Pools (applying…)` when `cache.applying`. One entry per `cache.pools` row: `label`, a
  bar at `ratio` (colored as pending when `pending` is non-null), `shown` and `unit`; then
  a detail line with `was <current> (<delta>)` when pending, `max <max>`, and `note`.
* **VRAM budget** — `cache.current_bytes.total` broken down by pool; when pending,
  `cache.proposed_bytes` with `cache.delta_bytes` and the "exceeds the engine's cache
  budget" error when `cache.over_budget`; a budget meter at `cache.budget_ratio` against
  `cache.budget_bytes`; `cache.facts`; `cache.last_rebuild_summary`; and the apply hint.
* **Empty states** — "only available while the engine is serving" when `cache.pools` is
  null; "this model exposes no resizable pools" when it is empty.

### 5.8 Jobs

* **List** — `jobs.items` then `jobs.downloads`, two rows each.
  Jobs: `kind_label`, `title`, `status_label`, `elapsed_s`; then a bar at
  `progress_ratio` with `progress_detail`, or `failure_reason` in red when failed, or the
  phase when there is no ratio.
  Downloads: `download`, `repo`, `status_label`, `elapsed_s`; then a bar at `ratio` with
  `done_bytes / total_bytes`, `rate_bps`, `eta_s` and `files_done of file_count`.
  Title shows the active count. Empty state shows `jobs.convert_checking` when a preflight
  is running, otherwise the three ways to start work.
* **Output** — for a selected job: `command_line`, `log_path`, `output_path` when set, then
  the tail from `GET /api/jobs/{id}/output`. For a selected download: `repo`, `revision`,
  `target`, `files_done of file_count`, transferred of total, and — while running —
  `rate_bps` and `current`; plus `failure_reason` when failed. With nothing selected: the
  bandwidth profile from `hardware.bench_profile` — version, GPU, UUID, host, timestamp,
  CPU cores and threads, the three ceilings, the hybrid threshold, and the per-format
  table (`dtype_kernels`: CPU GB/s, PCIe GB/s, ratio, `recommended`, `cpu_moe_isa`, the
  contended overlap line, and any `note`).

### 5.9 Requests and Logs

**Requests** — a table from `GET /api/requests`: time (the `HH:MM:SS` part of `ts`),
`method`, `path`, `status` (colored 2xx/3xx/4xx/5xx), `duration_ms`, `ttft_ms`,
`prompt_tokens`, `completion_tokens`. Title carries the count and `— paused` when
`requests.paused`. Detail pane for the selected row: `ts`, `method` + `path`, `status`,
`model`, `duration_ms`, `ttft_ms`, tokens in and out, `decode_tps`, `stream`, and `error`.
Empty state differs by `engine.server_reachable`.

**Logs** — lines from `GET /api/logs`, colored by the rule in 3.1, with browser-side
filter, errors-only, wrap and follow. Title `Engine log (n lines)` plus `— errors only`
and `— paused` as they apply. Empty state names `logs.log_path` when there is one, and
otherwise explains that no engine has been started from this daemon.

### 5.10 Keys with no route

These are entirely browser state and must not reach the server: tab switching (`1`–`9`,
`Tab`, `Shift+Tab`), the help overlay (`?`, `F1`), `Esc` to close an overlay or cancel an
edit, every cursor movement (`↑ ↓ j k`, `PgUp`, `PgDn`, `Home`, `End`, `G`), the Models
filter (`/`), the Hub pane focus (`Tab`, `Esc`), the Templates pane switch (`Tab`), the
Serve group switch (`← →`) and profile focus (`Tab`), the command preview toggle (`p` —
`serve.command_preview` is always in the snapshot), the Jobs output focus (`Tab`) and
scrolling, the Requests detail toggle (`Enter`) and follow (`f`), and the Logs filter
(`/`), errors-only (`e`), wrap (`w`) and follow (`f`).

`q` and `Ctrl-C` quit the TUI; the daemon has no quit action and the browser simply closes
the tab.

### 5.11 The help overlay

Reproduce `src/ui/views/help.rs` verbatim, with the two corrections noted in section 7.
Sections and bindings:

**Global** — `1-9 / Tab` switch view · `? or F1` this help · `q` quit (asks first if the
engine is running) · `Ctrl-C` quit immediately · `Esc` close an overlay, or cancel an edit

**Dashboard** — `e` start the engine with the current Serve configuration · `s` stop the
engine · `S` force-stop the engine (SIGKILL) · `t` run a /generate smoke test · `r` rescan
the model library

**Models** — `↑ ↓ / j k` move · `/` filter; Esc clears · `Enter` load into the Serve
configuration · `c` convert to FTW · `s` serve this model now · `D` delete the checkpoint
from disk · `r` rescan

**Hub** — `/` search · `Enter` list a repo's files · `Tab` move between results and files ·
`Space` toggle a file · `a / n` select all / none · `d` download the selected files ·
`i` install the hf CLI when it is missing

**Templates** — `r` set the repo to browse · `Enter` list that repo's templates · `Tab`
move between stored and remote · `f` fetch the highlighted template · `a` apply to the
selected model · `u` restore the model's built-in template · `v` check that it renders ·
`D` delete a stored template

**Serve** — `↑ ↓` move between knobs · `← →` switch knob group · `Enter` edit a value, or
toggle a flag · `Space` cycle a choice knob · `x / Del` unset a knob, back to its default ·
`a` plan this serve for the hardware; A applies it · `p` show the resolved command line ·
`Tab` move to the profile list · `S` save as a profile · `P` load the selected profile ·
`D` delete the selected profile · `g` start the engine

**Cache** — `↑ ↓` select a pool · `← →` adjust by 1% · `Shift + ← →` adjust by 10% ·
`r` reset the selected pool · `R` reset every pending change · `a` apply the rebuild

**Jobs** — `↑ ↓` select · `b` run ft bench bw · `Tab` focus the output pane · `x` cancel
the selected job · `X` clear finished entries

**Requests** — `↑ ↓` move · `Enter` toggle the detail pane · `f` follow the newest entry ·
`p` pause polling · `c` clear

**Logs** — `↑ ↓ / PgUp PgDn` scroll · `G / End` jump to the tail and follow · `f` toggle
follow · `/` filter · `e` errors only · `w` wrap long lines · `c` clear the buffer

---

## 6. Rust types that need `Serialize`

The backend should serialize existing types directly wherever one exists, so the JSON is
the struct's fields verbatim and there is one definition of each shape. Derived values go
alongside under clearly separate names (`*_label`, `*_summary`, `*_ratio`, `*_ms`), never
replacing a field.

Already `Serialize`: `knobs::ServeConfig`, `plan::Costs`, `config::Config` and every
`*Cfg`, `config::Profile`, `config::Profiles`, `templates::TemplateMeta`,
`templates::AppliedTemplate`, `ft::proc::ServeState`, `ft::api::CacheRebuild`.

**`Serialize` must be added to:**

| Module | Types |
|---|---|
| `src/ft/types.rs` | `Health`, `LoadProgress`, `Stats`, `ModelCard`, `PagePool`, `SlotPool`, `GpuCard`, `Throughput`, `RequestStats`, `CacheStatus`, `CacheGeometry`, `PoolBytes`, `UnitBytes`, `Reasoning`, `RequestRecord`, `BenchProfile`, `BenchGpu`, `BenchCpu`, `BenchCeilings`, `BenchKernel` |
| `src/ft/proc.rs` | `EngineState`, `LogLine`, `JobKind`, `JobStatus`, `JobProgress` |
| `src/ft/preflight.rs` | `Outcome` |
| `src/hub.rs` | `RepoSummary`, `RepoInfo`, `Sibling`, `RepoFile`, `DownloadStatus` |
| `src/models.rs` | `Model`, `Format` |
| `src/probe.rs` | `Gpu`, `Host` |
| `src/plan.rs` | `Plan`, `Step`, `Level`, `ContextFit`, `Startup` |
| `src/compat.rs` | `Report`, `Verdict`, `Level`, `Hardware` |
| `src/templates.rs` | `StoredTemplate`, `Status` |
| `src/variants.rs` | `Layout`, `Variant`, `Role` |
| `src/knobs.rs` | `Knob`, `Kind`, `Group` |
| `src/reuse.rs` | `Reuse` |
| `src/config.rs` | `HubToken` — **with `value` skipped**; only `source` is ever sent |
| `src/ui/app.rs` | `Pool` |
| `src/ui/widgets.rs` | `Toast`, `ToastKind`, `Confirm`, `ConfirmAction` |

Notes on how:

* Data-carrying enums take `#[serde(tag = "kind", rename_all = "snake_case")]`; where a
  variant holds an unnamed payload it needs a named field, so `JobStatus::Failed(String)`
  serializes through a small adapter or a `#[serde(rename = "reason")]` newtype wrapper.
  Section 2.3 is the normative shape.
* `Job` and `Download` cannot derive `Serialize` as they stand — both hold a `LogRing`, an
  `Ema`, an `Instant` sample and a `pid`. The web layer builds `JobEntry` and
  `DownloadEntry` explicitly (section 2.14), reading `elapsed()`, `rate.get()` and
  `done()` at snapshot time.
* `Engine` likewise is not serialized; `EngineSnapshot` is assembled field by field.
* `Toast::at` is an `Instant`; serialize `age_ms` instead, computed at snapshot time.
* `Model::modified` is a `SystemTime`; serialize `modified_ms`.
* `Job::started_at` / `finished_at` are `chrono::DateTime<Local>`, which serializes to
  RFC-3339 with an offset — send those as-is.
* `Knob` holds `&'static str` fields, which serialize as strings with no change needed.
* `CacheGeometry::limits` and `CacheStatus::last_rebuild` are `serde_json::Value` and pass
  through unchanged, as do `RepoSummary::gated`, `RepoInfo::gated` and
  `ModelCard::sampling`.

---

## 7. Ambiguities in the TUI and how they were resolved

1. **`requests_view.paused` does nothing in the TUI.** The `p` key flips it and the pane
   title reads it, but the telemetry poller never consults it and entries keep arriving.
   Resolved: `POST /api/requests/pause` sets the same flag and the snapshot reports it;
   collection continues, and the flag's meaning is "stop auto-following", which is what a
   reader who pressed `p` wants. Documented in 3.3 rather than silently changed.
2. **The help overlay says `1-8 / Tab`, but there are nine tabs**, and lists
   `Ctrl-Enter or g` for starting the engine although only `g` is bound. Resolved: the web
   help page says `1-9` and `g`. The TUI's own text is left alone; this document does not
   change behavior.
3. **The Hub help section omits `i`**, which installs the `hf` CLI and is a real binding.
   Resolved: added to the web key map.
4. **The Jobs output pane shows the in-memory ring; the log file has more in it.**
   `spawn_job_reader` keeps `FTCONVERT`/`FTBENCH`/`FTBENCH_OUT` lines out of the ring but
   writes them to the file, and the file merges stdout and stderr so `err` is unrecoverable.
   Resolved: `GET /api/jobs/{id}/output` reads the file (a byte offset is the only stable
   cursor into it) and filters those three prefixes, so the browser sees what the TUI sees;
   `err` is always false and coloring is by content, which is what the Logs view already
   does.
5. **The Cache tab shows a maximum but no minimum.** The engine publishes both in
   `limits.<pool>`. Resolved: `PoolRow.min` is sent, converted into the pool's own unit like
   `max` and floored at 1, so a web slider can clamp against the same number the daemon
   clamps against. It is never null — an absent published bound is 1, not "unknown", because
   a slider still needs a floor. `src/cache_pools.rs` is the single conversion the Cache
   view, this document and `POST /api/cache/pending` all read.
6. **Job ids and download ids are independent counters and collide.** Both start at 1.
   Resolved: `POST /api/jobs/cancel` and `POST /api/downloads/cancel` are separate routes,
   and the two lists stay separate in the snapshot rather than being merged into the TUI's
   unified `views::jobs::Row`.
7. **`p` on the Serve tab only toggles whether the preview is drawn.** Resolved:
   `serve.command_preview` is always present in the snapshot and the toggle is browser
   state; no route.
8. **The Hub revision is fixed at `main` with no key to change it.** Resolved: `revision`
   is an optional field on the Hub routes, defaulting to `hub.revision`, so the web UI can
   offer what the TUI cannot without changing the default behavior.
9. **Template apply and verify depend on "the model selected on the Models tab".**
   Resolved as described in 1.5: `model_path` is a required field on both routes, and no
   server-side selection exists.
10. **`ConfirmAction::Quit` has no meaning for a daemon.** Resolved: the web layer never
    pushes it; it is in the wire enum only so the type is complete.
11. **The knob schema would dominate the snapshot** (about 12 KiB of help text that never
    changes). Resolved: `GET /api/knobs` serves it once; the snapshot carries values and
    errors only.
12. **`hub.target` reads like an editable destination in the TUI** but downloads always go
    to the Hugging Face cache. Resolved: it is informational in the snapshot and no route
    accepts a target directory.
13. **A browser session is a cookie, and a cookie is what a cross-site request forgery
    spends.** The TUI has no equivalent exposure and so no equivalent rule. Resolved: an
    `Origin` whose authority is not this daemon's `Host` is `403`, and a `POST` declaring
    anything but `application/json` is `415` — together they put every state-changing route
    behind a preflight no hostile page can get through, at no cost to `curl` (section 1.2).
14. **A confirmation whose wording needs the filesystem cannot be raised synchronously.**
    `Delete checkpoint` quotes `dir_size`, and `Retry conversion` quotes the size of the
    leftovers; both are recursive walks of directories measured in hundreds of gigabytes.
    Resolved: those routes reply `{"status":"started"}` and the modal arrives in a later
    snapshot, so neither the TUI's event loop nor the daemon's shared mutex waits on a
    filesystem (sections 4.5 and 1.2).
15. **`err` on a log line says nothing**, because FreeToken logs its whole life to stderr —
    yet both front ends have to color the same line the same way. Resolved: one classifier,
    `views::logs::classify`, whose name is sent as `severity` on every `LogLine` and every
    job output line (sections 3.1 and 3.2). The terminal turns it into a theme color; the
    browser turns it into a class.
