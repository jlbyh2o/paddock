/**
 * The ft-man web API, as TypeScript.
 *
 * This file is the frontend's source of truth and the exact counterpart of
 * `docs/web-api.md`; the two are maintained together. Field names are snake_case
 * because they come straight out of serde. Types only — no route constants, no
 * helpers, no runtime code.
 *
 * Conventions used throughout:
 *   - `| null` means the server sends JSON null, not that the key is absent.
 *   - Optional (`?`) is used only where the server genuinely omits the key.
 *   - `*_ms` is milliseconds, `*_s` is seconds, `*_bytes` and `*_bps` are bytes and
 *     bytes per second. No Rust `Instant` or `Duration` ever reaches the wire.
 *   - Rust enums that carry data are tagged objects with a `kind` discriminant;
 *     enums that carry none are string literal unions.
 */

// ---------------------------------------------------------------- primitives

/** Severity name for a value the server has already color-coded. */
export type Severity = "good" | "warn" | "bad" | "dim";

/** Toast severity. Mirrors `ui::widgets::ToastKind`. */
export type ToastKind = "info" | "success" | "warn" | "error";

/** The four resizable cache pools. Mirrors `ui::app::Pool`. */
export type PoolId = "moe" | "kv" | "mamba" | "swa";

/** Knob groups, in `knobs::Group::ALL` order. */
export type KnobGroup = "model" | "server" | "runtime" | "memory" | "moe" | "api";

/** Local checkpoint formats. Mirrors `models::Format`. */
export type ModelFormat = "hf" | "ftw" | "gguf" | "partial_ftw";

/** Compatibility verdict. Mirrors `compat::Verdict`. */
export type CompatVerdict = "supported" | "caution" | "unsupported" | "unknown";

/** Compatibility note severity. Mirrors `compat::Level`. */
export type CompatLevel = "info" | "caution" | "blocker";

/** Plan step severity. Mirrors `plan::Level`. */
export type PlanLevel = "info" | "advice" | "warning";

/** What a group of repo files is for. Mirrors `variants::Role`. */
export type VariantRole = "weights" | "projector" | "draft";

/** Which long-running FreeToken command a job is. Mirrors `ft::proc::JobKind`. */
export type JobKind = "convert" | "bench";

/** Where the GPU numbers came from. */
export type GpuSource = "NVML" | "nvidia-smi";

// ---------------------------------------------------------------- tagged enums

/** Supervisor state of the engine. Mirrors `ft::proc::EngineState`. */
export type EngineState =
  | { kind: "stopped" }
  | { kind: "starting" }
  | { kind: "running" }
  | { kind: "stopping" }
  | { kind: "adopted" }
  | { kind: "exited"; code: number | null; signal: number | null };

/** Terminal or in-flight state of a job. Mirrors `ft::proc::JobStatus`. */
export type JobStatus =
  | { kind: "running" }
  | { kind: "done" }
  | { kind: "canceled" }
  | { kind: "failed"; reason: string };

/** Terminal or in-flight state of a download. Mirrors `hub::DownloadStatus`. */
export type DownloadStatus =
  | { kind: "running" }
  | { kind: "done" }
  | { kind: "canceled" }
  | { kind: "failed"; reason: string };

/** Result of a FreeToken-side check. Mirrors `ft::preflight::Outcome`. */
export type Preflight =
  | { kind: "ok"; detail: string }
  | { kind: "warn"; detail: string }
  | { kind: "fail"; detail: string };

/** What ft-man recorded when it applied a template. Mirrors `templates::AppliedTemplate`. */
export interface AppliedTemplate {
  name: string;
  source: string | null;
  revision: string | null;
  version: string | null;
  /** ISO-8601. */
  applied_at: string;
  /** Whether the checkpoint had its own chat_template.jinja before this. */
  had_original: boolean;
}

/**
 * A checkpoint's chat-template situation. Mirrors `templates::Status`, with
 * `label` from `Status::label()` so the UI never rebuilds the phrasing.
 */
export type TemplateStatus =
  | { kind: "built_in"; label: string }
  | { kind: "foreign"; label: string }
  | { kind: "overridden"; label: string; applied: AppliedTemplate };

/** A knob's type and domain. Mirrors `knobs::Kind`. */
export type KnobKind =
  | { kind: "text" }
  | { kind: "flag" }
  | { kind: "int"; min: number | null; max: number | null }
  | { kind: "float"; min: number; max: number }
  | { kind: "choice"; options: string[] }
  /** Any subset of `options`, stored space-separated. */
  | { kind: "multi"; options: string[] };

/**
 * What a pending confirmation would do if accepted. Mirrors
 * `ui::widgets::ConfirmAction`. Informational only: `POST /api/confirm` carries
 * just `{accept}`, because the pending action lives on the server.
 *
 * `quit` exists for completeness; the daemon never raises it.
 */
export type ConfirmAction =
  | { kind: "stop_engine"; force: boolean }
  | { kind: "delete_model"; path: string }
  | { kind: "cancel_job"; id: number }
  | { kind: "cancel_download"; id: number }
  | { kind: "delete_profile"; name: string }
  | { kind: "apply_cache_rebuild" }
  | { kind: "apply_template"; template: string; model: string }
  | { kind: "revert_template"; model: string }
  | { kind: "delete_template"; name: string }
  | { kind: "reconvert_model"; source: string }
  | { kind: "install_hf_cli" }
  | { kind: "convert_anyway"; source: string }
  | { kind: "quit" };

// ---------------------------------------------------------------- engine control plane

/** `GET /health` on the engine. Mirrors `ft::types::Health`, verbatim. */
export interface Health {
  /** `loading`, `ok`, or `error`. */
  status: string;
  model: string | null;
  message: string | null;
  uptime_s: number | null;
  /** `serving`, `loading`, or `rebuilding`. */
  maintenance: string | null;
  /** Weight-loading phase, present only while loading. */
  phase: string | null;
  progress: LoadProgress | null;
}

/** Weight-load progress. Mirrors `ft::types::LoadProgress`. */
export interface LoadProgress {
  done_bytes: number;
  total_bytes: number;
}

/** `GET /v1/stats` on the engine. Mirrors `ft::types::Stats`, verbatim. */
export interface Stats {
  model: ModelCard;
  uptime_s: number;
  kv: PagePool | null;
  mamba: SlotPool | null;
  swa: PagePool | null;
  vram_bytes: number;
  gpus: GpuCard[];
  throughput: Throughput;
  requests: RequestStats;
}

/** The served model as the engine describes it. Mirrors `ft::types::ModelCard`. */
export interface ModelCard {
  id: string | null;
  /** Advertised context length in tokens — the checkpoint's ceiling, not what KV holds. */
  ctx: number;
  /** `mha`, `hybrid_linear`, or `hybrid_swa`. */
  attn: string | null;
  moe: boolean;
  /**
   * What the API accepts right now, e.g. `["text"]` or `["text", "image"]`. The server's
   * current behavior, not the checkpoint's capability: a vision checkpoint served with
   * `--text-model-only` reports text alone.
   */
  input_modalities: string[];
  /** The checkpoint's recommended sampling parameters, raw JSON. */
  sampling: Record<string, unknown> | null;
}

/** A paged pool (KV, SWA). Mirrors `ft::types::PagePool`. */
export interface PagePool {
  used_pages: number;
  total_pages: number;
  page_size: number;
}

/** A slot pool (GDN state). Mirrors `ft::types::SlotPool`. */
export interface SlotPool {
  used_slots: number;
  total_slots: number;
}

/** A GPU as the engine reports it. Mirrors `ft::types::GpuCard`. */
export interface GpuCard {
  index: number | null;
  name: string | null;
  uuid: string | null;
  total_bytes: number;
}

/** Mirrors `ft::types::Throughput`. */
export interface Throughput {
  decode_tps: number;
  prefill_tps: number;
}

/** Mirrors `ft::types::RequestStats`. */
export interface RequestStats {
  active: number;
  completed: number;
  p95_ms: number;
  ttft_mean_ms: number;
  prompt_tokens_total: number;
  completion_tokens_total: number;
}

/** `GET /v1/cache/status`. Mirrors `ft::types::CacheStatus`, verbatim. */
export interface CacheStatus {
  /** `serving`, `loading`, or `rebuilding`. */
  state: string;
  /** Whatever the engine last rebuilt, raw JSON. */
  last_rebuild: Record<string, unknown> | null;
  geometry: CacheGeometry;
}

/** The live cache geometry. Mirrors `ft::types::CacheGeometry`, verbatim. */
export interface CacheGeometry {
  num_pages: number;
  page_size: number;
  moe_cache_size: number;
  num_mamba_slots: number;
  num_experts: number;
  num_moe_layers: number;
  moe_cache_policy: string | null;
  unit_bytes: UnitBytes;
  swa_full_tokens_ratio: number;
  swa_page_size: number;
  num_swa_pages: number;
  cache_budget_bytes: number;
  /**
   * Per-pool bounds the engine publishes, keyed by FreeToken's own names
   * (`moe_experts`, `kv_tokens`, `mamba_slots`, `swa_tokens`) and denominated in
   * FreeToken's units. Do not read this directly: `CachePoolRow` carries the
   * converted values.
   */
  limits: Record<string, { min?: number; max?: number }> | null;
  reasoning: Reasoning | null;
}

/** Per-unit VRAM costs the engine measured. Mirrors `ft::types::UnitBytes`. */
export interface UnitBytes {
  kv_per_token: number;
  moe_per_expert: number;
  mamba_per_slot: number;
  swa_per_token: number;
}

/** Thinking gears the served template exposes. Mirrors `ft::types::Reasoning`. */
export interface Reasoning {
  gears: string[];
  default: string | null;
}

/** VRAM each pool occupies. Mirrors `ft::types::PoolBytes` plus `total()`. */
export interface PoolBytes {
  kv: number;
  moe: number;
  mamba: number;
  swa: number;
  total: number;
}

/** `ft bench bw`'s per-GPU profile. Mirrors `ft::types::BenchProfile`, verbatim. */
export interface BenchProfile {
  version: number;
  timestamp: string | null;
  host: string | null;
  gpu: BenchGpu;
  cpu: BenchCpu;
  /** How much CPU must beat PCIe by before hybrid is recommended. */
  threshold: number;
  ceilings: BenchCeilings;
  /** Format -> `offload` | `hybrid` | null. */
  dtypes: Record<string, string | null>;
  /** Format -> the kernel measurements behind that verdict. */
  dtype_kernels: Record<string, BenchKernel>;
}

/** Mirrors `ft::types::BenchGpu`. */
export interface BenchGpu {
  index: number | null;
  name: string | null;
  uuid: string | null;
}

/** Mirrors `ft::types::BenchCpu`. */
export interface BenchCpu {
  physical_cores: number;
  threads_used: number;
}

/** Mirrors `ft::types::BenchCeilings`. */
export interface BenchCeilings {
  cpu_stream_read_gbs: number;
  pcie_linear_h2d_gbs: number;
  pcie_linear_d2h_gbs: number;
}

/** One format's measured kernels. Mirrors `ft::types::BenchKernel`. */
export interface BenchKernel {
  cpu_moe_gbs: number | null;
  cpu_moe_isa: string | null;
  pcie_gather_gbs: number | null;
  cpu_moe_overlap_gbs: number | null;
  pcie_gather_overlap_gbs: number | null;
  ratio: number | null;
  /** `hybrid`, `offload`, or null. */
  recommended: string | null;
  note: string | null;
}

// ---------------------------------------------------------------- hardware

/** A local GPU. Mirrors `probe::Gpu` plus the derived helpers. */
export interface Gpu {
  index: number;
  name: string;
  uuid: string;
  memory_total: number;
  memory_used: number;
  /** Percent, 0-100. */
  utilization: number | null;
  /** Degrees Celsius. */
  temperature: number | null;
  power_watts: number | null;
  power_limit_watts: number | null;
  /** Current PCIe link, e.g. `gen5 x16`. */
  pcie_link: string | null;
  /** Derived: `Gpu::memory_free()`. */
  memory_free: number;
  /** Derived: `Gpu::memory_ratio()`, 0.0-1.0. */
  memory_ratio: number;
  /** Derived: `Gpu::short_uuid()`. */
  short_uuid: string;
}

/** Host telemetry. Mirrors `probe::Host` plus the derived helpers. */
export interface Host {
  cpu_percent: number;
  cpu_cores: number;
  /** Physical cores — what FreeToken's CPU MoE executor sizes itself against. */
  physical_cores: number;
  memory_total: number;
  memory_used: number;
  swap_total: number;
  swap_used: number;
  /** 1, 5 and 15 minute load averages. */
  load_avg: [number, number, number];
  hostname: string;
  kernel: string;
  uptime_s: number;
  /** Derived: `Host::memory_free()` — the headroom offloaded expert banks need. */
  memory_free: number;
  /** Derived: `Host::memory_ratio()`, 0.0-1.0. */
  memory_ratio: number;
}

// ---------------------------------------------------------------- hub

/** A Hub search result. Mirrors `hub::RepoSummary` plus derived fields. */
export interface RepoSummary {
  id: string;
  downloads: number;
  likes: number;
  last_modified: string | null;
  tags: string[];
  /** Raw: the Hub returns false, a string, or an object. Use `is_gated`. */
  gated: unknown;
  private: boolean;
  /** Derived: `RepoSummary::is_gated()`. */
  is_gated: boolean;
  /** Derived: `RepoSummary::interesting_tags()` — at most six, noise removed. */
  interesting_tags: string[];
}

/** One file in a repo listing. Mirrors `hub::Sibling`. */
export interface Sibling {
  path: string;
  size: number | null;
}

/** Full repo metadata. Mirrors `hub::RepoInfo` plus `is_gated`. */
export interface RepoInfo {
  id: string;
  /** The commit the requested revision resolved to. */
  sha: string | null;
  gated: unknown;
  siblings: Sibling[];
  is_gated: boolean;
}

/** A file resolved for download. Mirrors `hub::RepoFile`. */
export interface RepoFile {
  path: string;
  size: number;
  wanted: boolean;
}

/** One quantization (or projector, or draft model). Mirrors `variants::Variant`. */
export interface Variant {
  /** What the user picks: `UD-IQ3_XXS`, `Q8_0`, `safetensors`. */
  label: string;
  role: VariantRole;
  /** Subdirectory holding the weights; null means the repo root. This is what `--model` must point at. */
  subdir: string | null;
  files: string[];
  bytes: number;
  /** Derived: `Variant::file_count()`. */
  file_count: number;
}

/** A repo's files, grouped. Mirrors `variants::Layout` plus `is_multi`. */
export interface Layout {
  variants: Variant[];
  /** Files every variant needs: config.json, the tokenizer, a chat template. */
  shared: string[];
  /** Derived: `Layout::is_multi()` — whether there is a real choice to make. */
  is_multi: boolean;
}

/** One compatibility note. */
export interface CompatNote {
  level: CompatLevel;
  text: string;
}

/** A pre-download verdict from config.json alone. Mirrors `compat::Report`. */
export interface CompatReport {
  arch: string | null;
  model_type: string | null;
  is_moe: boolean;
  num_experts: number | null;
  num_layers: number | null;
  quant: string | null;
  context: number | null;
  /** Most severe first. */
  notes: CompatNote[];
  /** Derived: `Report::verdict()`. */
  verdict: CompatVerdict;
  /** Derived: `Verdict::label()`, e.g. "supported, with caveats". */
  verdict_label: string;
  /** Derived: `Report::summary()`, e.g. "Qwen3MoeForCausalLM · MoE x128 · NVFP4 · 256k ctx". */
  summary: string;
}

/** Free space, and the directory the figure was actually measured at. */
export interface DiskFree {
  /** `hub::disk_free_at` walks up to an existing ancestor; this is where it stopped. Always show it with the number. */
  measured_path: string;
  free_bytes: number;
}

// ---------------------------------------------------------------- knobs

/** One `ft serve` flag. Mirrors `knobs::Knob`. Served by `GET /api/knobs`. */
export interface Knob {
  /** Stable identifier — the target of every serve action. Never the flag spelling. */
  key: string;
  /** The flag as `ft serve` spells it, e.g. `--moe-cache-size`. */
  flag: string;
  label: string;
  group: KnobGroup;
  kind: KnobKind;
  /** What FreeToken does when the flag is absent; shown as the placeholder. */
  default: string;
  help: string;
  /** Keys that cannot be set at the same time; may include this knob's own key. */
  exclusive_with: string[];
}

/** `GET /api/knobs` — the static schema, cacheable for the connection's lifetime. */
export interface KnobSchema {
  groups: { group: KnobGroup; title: string }[];
  /** In `knobs::KNOBS` order, which is the order the Serve view presents them. */
  knobs: Knob[];
}

// ---------------------------------------------------------------- snapshot: engine

/** How much of the advertised context the engine can really serve. Mirrors `plan::ContextFit`. */
export interface ContextFit {
  /** `num_pages * page_size` — what KV actually holds. */
  usable: number;
  /** What `/v1/models` advertises. */
  ceiling: number;
  is_truncated: boolean;
  /** 0.0-1.0. */
  ratio: number;
  /** `ContextFit::summary()`, e.g. "32k of 256k". */
  summary: string;
  /** `ContextFit::verdict()` — the plan's headline sentence, e.g. "the full 256k this model offers". */
  verdict: string;
}

/** Estimated prefix-cache reuse. Mirrors `reuse::Reuse`. Null when the evidence is too thin. */
export interface Reuse {
  /** Fraction of prompt tokens served from cache, 0.0-1.0. */
  fraction: number;
  /** The cold prefill rate the estimate was anchored on, tokens per second. */
  cold_rate: number;
  samples: number;
  /** `Reuse::summary()` — always marked as an estimate. */
  summary: string;
}

/** Engine state, the status line, and the derived figures the Dashboard prints. */
export interface EngineSnapshot {
  state: EngineState;
  is_live: boolean;
  /** `App::engine_status_text()`. Render this; do not re-derive it. */
  status_text: string;
  /** The theme role `App::engine_status_color()` picks. */
  status_class: Severity;
  pid: number | null;
  /** True when this engine was adopted from serve.json rather than started here. */
  adopted: boolean;
  /** `App::current_model()`. */
  model: string | null;
  port: number | null;
  command_line: string | null;
  log_path: string | null;
  /** `Client::base_url()` — the URL actually polled, wildcards resolved to loopback. */
  endpoint: string;
  server_reachable: boolean;
  /** `App::context_fit()`. */
  context_fit: ContextFit | null;
  /** `App::prefix_reuse()`. Null means show nothing, never a zero. */
  prefix_reuse: Reuse | null;
  /** `App::completed_rate.get()` — requests completed per second, smoothed. */
  completed_rate: number;
  active_jobs: number;
  active_downloads: number;
  /** `App::gpu_busy_reason()` — why a conversion or benchmark would be refused now. */
  gpu_busy_reason: string | null;
  /**
   * Why `POST /api/engine/start` would be refused right now, computed by the same
   * predicate the route uses; null when a start would be attempted.
   */
  start_blocked: string | null;
}

/** The raw control-plane documents, plus the values the views derive from them. */
export interface TelemetrySnapshot {
  health: Health | null;
  stats: Stats | null;
  cache_status: CacheStatus | null;
  /** The last poll failure, e.g. connection refused. */
  error: string | null;
  /** Age of the last poll. */
  age_ms: number | null;
  /** `Health::load_ratio()`. */
  health_load_ratio: number | null;
  /** `CacheGeometry::pool_bytes()` plus its total. */
  pool_bytes: PoolBytes | null;
  /** `CacheGeometry::total_experts()`. */
  total_experts: number | null;
  kv_used_tokens: number | null;
  kv_total_tokens: number | null;
  kv_ratio: number | null;
  swa_used_tokens: number | null;
  swa_total_tokens: number | null;
  swa_ratio: number | null;
  mamba_ratio: number | null;
  /** "last rebuild: MoE 2,403  KV 8,192". */
  last_rebuild_summary: string | null;
  /** `views::dashboard::format_sampling` — the checkpoint's recommended sampling, e.g. "temperature 0.6  top_p 0.95". */
  sampling_summary: string | null;
}

/** Sparkline series: up to 120 samples each, oldest first. */
export interface SeriesSnapshot {
  decode_tps: number[];
  prefill_tps: number[];
  /** Percent. */
  gpu_util: number[];
  /** MiB. */
  vram: number[];
  /** Concurrent requests. */
  active: number[];
  /** Running maximum decode throughput. */
  decode_peak: number;
}

/** GPUs, host and the bandwidth profile. */
export interface HardwareSnapshot {
  gpu_source: GpuSource;
  gpus: Gpu[];
  /** The card the engine actually bound, flagged with "← engine". */
  engine_gpu_uuid: string | null;
  /** Rendered only when `gpus` is empty. */
  reported_gpus: GpuCard[];
  host: Host;
  bench_profile: BenchProfile | null;
  /** "bench: nvfp4→hybrid  mxfp4→offload", or null when there is no profile. */
  /**
   * Per-format bandwidth verdicts, grouped by verdict, largest group first. Empty when
   * `bench_profile` is null, or when a profile was measured without per-format answers.
   */
  bench_verdicts: BenchVerdict[];
  bench_profile_path: string | null;
}

// ---------------------------------------------------------------- snapshot: models

/** One bullet in the Models detail pane, pre-computed against this machine. */
export interface ModelGuidance {
  level: Severity;
  text: string;
}

/** A local checkpoint. Mirrors `models::Model` plus every derived field the views show. */
export interface ModelEntry {
  name: string;
  /** The Hugging Face repo id when this came from the hub cache. */
  repo: string | null;
  /** Which quantization of `repo` this is, when the repo ships more than one. */
  variant: string | null;
  /** The identity every model action targets. */
  path: string;
  format: ModelFormat;
  /** `Format::label()` — HF, FTW, GGUF, PART. */
  format_label: string;
  /** The detail pane's long form, e.g. "FTW — FreeToken fast-load". */
  format_description: string;
  size_bytes: number;
  arch: string | null;
  model_type: string | null;
  is_moe: boolean;
  num_experts: number | null;
  num_layers: number | null;
  quant: string | null;
  /** `max_position_embeddings`. */
  max_position: number | null;
  ftw_fingerprint: string | null;
  /** The FTW build made from this checkpoint, when the scan found one. */
  converted_to: string | null;
  modified_ms: number | null;
  /** `Model::summary()` — the dim second line. */
  summary: string;
  /** `Model::served_name()` — `repo:variant`; also the costs.json key. */
  served_name: string;
  convertible: boolean;
  /** True for the wreckage of a failed conversion. */
  is_partial: boolean;
  /** `templates::status(path)`. */
  template_status: TemplateStatus;
  /** Directories an apply would write into: the checkpoint, plus its FTW build. */
  template_targets: string[];
  /** Where a conversion would write, whether or not it exists yet. */
  ftw_output_path: string;
  guidance: ModelGuidance[];
}

/** The local library. */
export interface ModelsSnapshot {
  scanning: boolean;
  /** Unfiltered; filtering is a browser concern. */
  items: ModelEntry[];
  /** Every scanned root, marked present or missing. */
  roots: { path: string; exists: boolean }[];
  config_path: string;
}

// ---------------------------------------------------------------- snapshot: hub

/** The Hub tab's whole state. */
export interface HubSnapshot {
  query: string;
  searching: boolean;
  results: RepoSummary[];
  /** `main` unless an action set another. */
  revision: string;
  loading_info: boolean;
  info: RepoInfo | null;
  layout: Layout | null;
  /** The chosen quantization. */
  variant: string | null;
  /** True once files were toggled by hand, so the UI stops calling the selection a quantization. */
  custom_selection: boolean;
  files: RepoFile[];
  selected_bytes: number;
  selected_count: number;
  compat: CompatReport | null;
  compat_error: string | null;
  checking_compat: boolean;
  /** The cache directory the download lands in. Informational. */
  target: string;
  disk_free: DiskFree | null;
  /** The resolved `hf` binary; null means downloads are impossible until it is installed. */
  hf_cli: string | null;
  hf_installing: boolean;
  /** `hub::INSTALL_COMMAND`, quoted in the install confirmation. */
  hf_install_command: string;
}

// ---------------------------------------------------------------- snapshot: templates

/** Provenance for a stored template. Mirrors `templates::TemplateMeta`. */
export interface TemplateMeta {
  source: string | null;
  revision: string | null;
  repo_path: string | null;
  fetched_at: string | null;
  /** `template_version` declared inside the jinja. */
  version: string | null;
}

/** A template in the local store. Mirrors `templates::StoredTemplate` plus `subtitle`. */
export interface StoredTemplateEntry {
  /** The identity every template action targets. */
  name: string;
  path: string;
  size: number;
  meta: TemplateMeta;
  /** `StoredTemplate::subtitle()` — version and source, or "imported". */
  subtitle: string;
}

/** The head of a template, for the preview pane. */
export interface TemplatePreview {
  name: string;
  /** Capped at 8 KiB. */
  text: string;
  truncated: boolean;
}

/** The Templates tab's whole state. */
export interface TemplatesSnapshot {
  stored: StoredTemplateEntry[];
  /** The repo in the browse field. */
  repo: string;
  loading: boolean;
  /** The repo's .jinja files. */
  remote: Sibling[];
  remote_repo: string | null;
  /** The commit the listing resolved to. */
  remote_revision: string | null;
  /** Store names each remote file would be saved under, so the UI can draw the ✓. */
  remote_stored_names: string[];
  preview: TemplatePreview | null;
  checking: boolean;
  /** The last render check and which template it was for. */
  preflight: { template: string; outcome: Preflight } | null;
  /** `config.templates.sources`. */
  sources: string[];
  /** Whether an apply runs a render check first. */
  preflight_enabled: boolean;
}

// ---------------------------------------------------------------- snapshot: serve

/** One validation failure from `ServeConfig::validate()`. */
export interface KnobError {
  key: string;
  /** The knob's flag spelling, resolved by the daemon; null when the key names no knob. */
  flag: string | null;
  message: string;
}

/** One recommendation from a plan. Mirrors `plan::Step`. */
export interface PlanStep {
  level: PlanLevel;
  /** `Step::label()`, e.g. "--kv-reserve-tokens 262144", or "—" for a note. */
  label: string;
  /** The knob to change, or null for a note that carries no edit. */
  key: string | null;
  value: string | null;
  /** Why, with the numbers it was derived from. Unwrapped; the browser wraps it. */
  reason: string;
}

/** What a planning run concluded. Mirrors `plan::Plan`. */
export interface Plan {
  steps: PlanStep[];
  /** The context this plan expects to deliver, against the model's ceiling. */
  fit: ContextFit | null;
  /** Set when there was not enough measured information to plan the memory split. */
  unpriced: string | null;
  is_empty: boolean;
  /** `Plan::edits().len()` — how many knobs applying would change. */
  edit_count: number;
}

/** A saved profile, as the list renders it. The profile's knob values stay server-side. */
export interface ProfileEntry {
  name: string;
  notes: string;
  /** `profile.serve.get("model")` — the dim second line. */
  model: string | null;
}

/** The Serve tab's state. The knob schema itself comes from `GET /api/knobs`. */
export interface ServeSnapshot {
  /** Only the knobs actually set. A flag that is on is present as "true". */
  values: Record<string, string>;
  /** `ServeConfig::validate()`, sorted and deduplicated. */
  errors: KnobError[];
  /** `ServeConfig::preview(program)` — the exact shell-quoted command line. */
  command_preview: string;
  /** How many knobs are set, per group. */
  set_counts: Record<KnobGroup, number>;
  plan: Plan | null;
  profiles: ProfileEntry[];
  last_used_profile: string | null;
}

// ---------------------------------------------------------------- snapshot: cache

/**
 * One resizable pool. Every number is computed server-side — the browser does no
 * pool arithmetic, because the token/page conversions have to match the engine.
 */
export interface CachePoolRow {
  pool: PoolId;
  /** `Pool::label()`, e.g. "MoE expert slots". */
  label: string;
  /** `Pool::unit()` — "slots" or "pages". */
  unit: string;
  /** `views::cache::pool_current`. */
  current: number;
  /** `views::cache::pool_max`, in the pool's own unit. */
  max: number;
  /** The effective minimum in the pool's unit, already clamped to at least 1. */
  min: number;
  /** Null means "leave this pool alone". */
  pending: number | null;
  /** `pending ?? current` — what the bar and the number render. */
  shown: number;
  /** `pending - current`, signed; null when nothing is pending. */
  delta: number | null;
  /** Bar fill, 0.0-1.0. */
  ratio: number;
  /** `views::cache::pool_note`, e.g. "64% of 6,144 experts resident". */
  note: string;
}

/** The Cache tab. `pools` is null when no engine is publishing a geometry. */
export interface CacheSnapshot {
  state: string | null;
  applying: boolean;
  has_pending: boolean;
  pools: CachePoolRow[] | null;
  current_bytes: PoolBytes | null;
  /** `views::cache::proposed_bytes` — the total if every pending edit were applied. */
  proposed_bytes: number | null;
  /** Signed difference against `current_bytes.total`. */
  delta_bytes: number | null;
  budget_bytes: number | null;
  /** The rebuild would exceed the engine's budget and be rejected. */
  over_budget: boolean;
  budget_ratio: number | null;
  /** Pre-formatted footnotes: eviction policy, window ratio, thinking gears. */
  facts: string[];
  last_rebuild_summary: string | null;
  /** A rebuild is rejected while requests are in flight. */
  active_requests: number;
}

// ---------------------------------------------------------------- snapshot: jobs

/** A conversion or benchmark. Built from `ft::proc::Job`. */
export interface JobEntry {
  /** The cancel identity. Job ids and download ids are separate counters and do collide. */
  id: number;
  kind: JobKind;
  kind_label: string;
  title: string;
  /** The first line of the output pane. */
  command_line: string;
  status: JobStatus;
  /** "running" | "done" | "failed" | "canceled". */
  status_label: string;
  progress: JobProgress;
  /** `JobProgress::ratio()`; null when no total is known, so there is no bar. */
  progress_ratio: number | null;
  /** The list's second line, pre-formatted (bytes, rate, phase, or step counts). */
  progress_detail: string;
  rate_bps: number;
  /** Frozen once the job finished. */
  elapsed_s: number;
  /** ISO-8601 with offset. */
  started_at: string;
  finished_at: string | null;
  log_path: string;
  /** Where a bench run wrote its profile. */
  output_path: string | null;
  /**
   * The job's output line counter (`LogRing` sequence). It is the change counter for
   * `GET /api/jobs/{id}/output`: poll when it moves, not on a timer, and it moves with
   * the job's final status line, so no extra read is needed after the job stops.
   */
  output_seq: number;
  /** `Job::failure_reason()` — the most informative line the process printed. */
  failure_reason: string | null;
  is_running: boolean;
}

/** Progress parsed from a job's machine-readable output. Mirrors `ft::proc::JobProgress`. */
export interface JobProgress {
  /** `dense`, `experts`, `finalize` for a convert; the current format for a bench. */
  phase: string;
  done: number;
  total: number;
  /** Whether done/total are byte counts (convert) or step counts (bench). */
  bytes: boolean;
}

/** A repo download. Built from `hub::Download`. */
export interface DownloadEntry {
  /** The cancel identity, numbered independently of job ids. */
  id: number;
  repo: string;
  revision: string;
  target: string;
  total_bytes: number;
  done_bytes: number;
  ratio: number;
  file_count: number;
  files_done: number;
  /** The file in flight. */
  current: string;
  status: DownloadStatus;
  /** "downloading" | "done" | "failed" | "canceled". */
  status_label: string;
  rate_bps: number;
  /** Null when the rate is not yet meaningful; the TUI prints "--". */
  eta_s: number | null;
  elapsed_s: number;
  started_at: string;
  finished_at: string | null;
  failure_reason: string | null;
  is_running: boolean;
}

/** Jobs and downloads are separate lists; the TUI merges them only for display. */
export interface JobsSnapshot {
  items: JobEntry[];
  downloads: DownloadEntry[];
  /** The checkpoint whose conversion preflight is in flight. */
  convert_checking: string | null;
}

// ---------------------------------------------------------------- snapshot: chrome

/** A transient notification. Mirrors `ui::widgets::Toast`. */
export interface Toast {
  /** Server-assigned, monotonic, so the UI can animate without matching on text. */
  id: number;
  text: string;
  kind: ToastKind;
  /** Computed server-side; `Instant` cannot cross the wire. */
  age_ms: number;
  /** 4000 info/success, 8000 warn, 12000 error — the ladder `Toast::is_expired()` uses. */
  ttl_ms: number;
}

/** A pending confirmation. Mirrors `ui::widgets::Confirm`. */
export interface Confirm {
  title: string;
  /** One entry per line; empty strings are deliberate blank lines. Render verbatim. */
  body: string[];
  /** Always ["Cancel", "Confirm"] today. */
  options: string[];
  /** Always 0 — the safe option. */
  default_index: number;
  /** Color the affirmative option as dangerous. */
  destructive: boolean;
  /** What accepting would do. Informational; POST /api/confirm carries only {accept}. */
  action: ConfirmAction;
}

/** Counters for the request ring; entries come from `GET /api/requests`. */
export interface RequestsSnapshot {
  /** A display flag: collection continues while paused. */
  paused: boolean;
  count: number;
  first_seq: number;
  /** 0 when empty. */
  last_seq: number;
  dropped: number;
  /** The engine's own `/v1/requests?since=` cursor. Diagnostic. */
  engine_cursor: number;
}

/** Counters for the engine log; lines come from `GET /api/logs`. */
export interface LogsSnapshot {
  count: number;
  first_seq: number;
  /** 0 when empty. */
  last_seq: number;
  dropped: number;
  capacity: number;
  log_path: string | null;
}

/** Config values the UI needs. Secrets are never included. */
export interface ConfigSnapshot {
  /** auto | dark | light | mono. */
  theme: string;
  /** When false, actions run immediately and `confirm` is never populated. */
  confirm_destructive: boolean;
  tick_ms: number;
  log_capacity: number;
  poll_ms: number;
  server_host: string;
  server_port: number;
  download_dir: string;
  ftw_dir: string;
  hub_cache: string;
  hub_endpoint: string;
  hub_concurrency: number;
  hub_ignore: string[];
  convert_preflight: boolean;
  templates_preflight: boolean;
  template_sources: string[];
  config_path: string;
  state_dir: string;
  /** Free space at `download_dir`, with the path actually measured. */
  disk_free: DiskFree | null;
}

/** One bandwidth verdict and every quantization format that earned it. */
export interface BenchVerdict {
  /** `offload` or `hybrid`. */
  verdict: string;
  formats: string[];
}

/**
 * A summary of the upstream commits this checkout is missing, written by the model the
 * engine has loaded — and the conditions it was written under, because a summary is only
 * as good as what was loaded and how much of the diff fitted.
 */
export interface UpstreamSummary {
  /** The commit range summarized, e.g. `e0886cc..84d236c`. */
  range: string;
  commits: number;
  /** The model that wrote it. */
  model: string;
  /** True while the request is still out. */
  pending: boolean;
  /** The patch was cut to fit; the diffstat the model saw was still complete. */
  truncated: boolean;
  text: string | null;
  error: string | null;
}

/** What this installation has, and what it is missing. */
export interface EnvironmentSnapshot {
  ft_found: boolean;
  ft_program: string | null;
  /** How the command reads, e.g. `ft` or `/venv/bin/python -m freetoken.cli`. */
  ft_display: string | null;
  /** `ft --version` output, e.g. `freetoken version 0.1.2`. */
  ft_version: string | null;
  ft_origin: string | null;
  /** Why the CLI could not be found. Non-null means every FreeToken route returns 503. */
  ft_error: string | null;
  /** FreeToken's registry. Null means it could not be read, so verdicts say "unverified". */
  supported_archs: string[] | null;
  hub_token_present: boolean;
  /** Provenance only, e.g. "the HF_TOKEN environment variable". The value is never sent. */
  hub_token_source: string | null;
  endpoint: string;
  hostname: string;
  // Git status of the FreeToken checkout this machine builds from. Null throughout when
  // there is no checkout — FreeToken installed from a wheel, or git unavailable.
  ft_upstream_sha: string | null;
  ft_origin_sha: string | null;
  ft_upstream_behind: number | null;
  ft_origin_ahead: number | null;
  ft_origin_behind: number | null;
  ft_dirty: boolean | null;
  ft_checkout_note: string | null;
  /** The engine's account of the upstream commits, once asked for. Null until then. */
  upstream_summary: UpstreamSummary | null;
  /** The commit the working tree is on. */
  ft_local_sha: string | null;
  /** Which tree was read; it is resolved at run time, not baked into the binary. */
  ft_checkout_path: string | null;
  /** The built kernels predate their sources: pulled, but not rebuilt. */
  ft_kernels_stale: boolean | null;
}

// ---------------------------------------------------------------- the snapshot

/**
 * The whole application state, minus the three append-only collections
 * (engine log, job output, request ring), which are fetched incrementally.
 *
 * Delivered by `GET /api/snapshot` and by every `snapshot` event on
 * `GET /api/events`. Coalesced to at most 10 per second.
 */
export interface Snapshot {
  /** Monotonic from 1, for this process. A lower value than the one held means the daemon restarted. */
  seq: number;
  /** Unix milliseconds when the snapshot was built. */
  ts_ms: number;
  /** The ft-man version. */
  version: string;
  engine: EngineSnapshot;
  telemetry: TelemetrySnapshot;
  series: SeriesSnapshot;
  hardware: HardwareSnapshot;
  models: ModelsSnapshot;
  hub: HubSnapshot;
  templates: TemplatesSnapshot;
  serve: ServeSnapshot;
  cache: CacheSnapshot;
  jobs: JobsSnapshot;
  requests: RequestsSnapshot;
  logs: LogsSnapshot;
  /** At most four, newest last. */
  toasts: Toast[];
  confirm: Confirm | null;
  config: ConfigSnapshot;
  environment: EnvironmentSnapshot;
}

// ---------------------------------------------------------------- SSE payloads

/** `event: snapshot` — data is one complete Snapshot. */
export interface SnapshotEvent {
  event: "snapshot";
  data: Snapshot;
}

/** `event: heartbeat` — once a second when nothing changed. */
export interface HeartbeatEvent {
  event: "heartbeat";
  data: Record<string, never>;
}

/** Every frame `GET /api/events` can deliver. */
export type ServerEvent = SnapshotEvent | HeartbeatEvent;

// ---------------------------------------------------------------- incremental pages

/** The shared envelope for every sequence-numbered collection. */
export interface SeqPage<T> {
  items: T[];
  /** Sequence of the oldest retained item. If this exceeds the last one rendered + 1, items were lost. */
  first_seq: number;
  /** Sequence of the newest item; 0 when empty. */
  last_seq: number;
  /** Items evicted or cleared since the process started. */
  dropped: number;
  /** Pass back as `after` on the next request. */
  next_after: number;
}

/** One line of engine output. Mirrors `ft::proc::LogLine` plus its sequence. */
export interface LogLine {
  seq: number;
  /** Unmodified: no truncation, no wrapping, no ANSI stripping. */
  text: string;
  /** True for lines that arrived on stderr. */
  err: boolean;
  /** `views::logs::classify` — the same coloring the TUI applies. Render it; do not re-derive. */
  severity: LogSeverity;
}

/** How a log line is colored. "meta" is a `[ft-man]` line of ft-man's own. */
export type LogSeverity = "error" | "warn" | "meta" | "normal";

/** One line of a job's output, classified like an engine log line. */
export interface JobOutputLine {
  text: string;
  severity: LogSeverity;
}

/** `GET /api/logs?after=&limit=`. */
export type LogPage = SeqPage<LogLine>;

/** One served request. Mirrors `ft::types::RequestRecord` plus derived fields. */
export interface RequestRecord {
  /** ft-man's own append sequence — not the engine's cursor. */
  seq: number;
  /** ISO-8601, as the engine wrote it. */
  ts: string;
  method: string;
  path: string;
  status: number;
  model: string | null;
  duration_ms: number;
  /** Null for a non-streamed request. */
  ttft_ms: number | null;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  stream: boolean | null;
  error: string | null;
  /** Derived: completion_tokens / seconds. Null unless both are known and non-zero. */
  decode_tps: number | null;
}

/** `GET /api/requests?after=&limit=`. */
export type RequestPage = SeqPage<RequestRecord>;

/**
 * `GET /api/jobs/{id}/output?offset=&limit=` — the tail of a job's log file, with
 * the FTCONVERT / FTBENCH / FTBENCH_OUT progress protocol removed so it matches
 * what the TUI's output pane shows.
 */
export interface JobOutputPage {
  id: number;
  /** Where the read started, after clamping to the file size. */
  offset: number;
  /** Always a line boundary; a partial trailing line is withheld. */
  next_offset: number;
  eof: boolean;
  /** The requested offset was past the end of the file, so the read restarted at 0. */
  truncated: boolean;
  lines: JobOutputLine[];
}

// ---------------------------------------------------------------- errors and replies

/** Every non-2xx /api response. */
export interface ApiError {
  error: string;
  /**
   * True when the daemon also pushed this refusal as a toast, which arrives in the next
   * snapshot: render one problem, not two. False for a field-shaped refusal (a rejected
   * knob value) that the client shows inline.
   */
  toasted: boolean;
}

/** The action completed. */
export interface OkReply {
  status: "ok";
}

/**
 * A task is now running (a conversion, a benchmark, a download, a search, a
 * render check, an engine start). Watch the snapshot for its effect.
 */
export interface StartedReply {
  status: "started";
  job_id?: number;
  log_path?: string;
}

/**
 * `app.confirm` is now set and **nothing has happened yet**. Render the modal from
 * the next snapshot and answer with `POST /api/confirm`.
 */
export interface ConfirmPendingReply {
  status: "confirm_pending";
}

/** Any action reply. Routes marked "⚠ confirms" in the spec can return the third. */
export type ActionReply = OkReply | StartedReply | ConfirmPendingReply;

// ---------------------------------------------------------------- auth

/** `GET /api/auth`. */
export interface AuthStatus {
  auth_required: boolean;
  authorized: boolean;
}

/** `POST /api/login`. */
export interface LoginRequest {
  token: string;
}

/** `POST /api/login` / `POST /api/logout`. */
export interface AuthReply {
  authorized: boolean;
}

// ---------------------------------------------------------------- request bodies

/** `POST /api/confirm` — accept or dismiss the pending confirmation. */
export interface ConfirmRequest {
  accept: boolean;
}

/** `POST /api/engine/stop`. Confirms. */
export interface EngineStopRequest {
  /** SIGKILL rather than the SIGINT escalation. Marks the confirmation destructive. */
  force: boolean;
}

/** `POST /api/models/use` — load a checkpoint into the Serve configuration. */
export interface ModelUseRequest {
  path: string;
  /** Start the engine straight away, as the Models tab's `s` does. */
  and_serve?: boolean;
}

/** `POST /api/models/convert`. May confirm (leftovers to delete, or a preflight concern). */
export interface ModelConvertRequest {
  path: string;
}

/** `POST /api/models/delete`. Confirms, destructively. */
export interface ModelDeleteRequest {
  path: string;
}

/** `POST /api/hub/search`. */
export interface HubSearchRequest {
  query: string;
}

/** `POST /api/hub/open` — list a repo's files and check its compatibility. */
export interface HubOpenRequest {
  repo_id: string;
  /** Defaults to the current `hub.revision` ("main"). */
  revision?: string;
}

/** `POST /api/hub/variant` — choose a quantization, which sets the file selection. */
export interface HubVariantRequest {
  label: string;
}

/** `POST /api/hub/files/toggle`. Omit `wanted` to toggle. */
export interface HubFileToggleRequest {
  path: string;
  wanted?: boolean;
}

/** `POST /api/hub/files/select` — the `a` and `n` keys. */
export interface HubFileSelectRequest {
  mode: "all" | "none";
}

/**
 * `POST /api/hub/download`. Resolution order: explicit `files`, else `variant`
 * expanded through the layout, else whatever is currently `wanted`.
 */
export interface HubDownloadRequest {
  repo_id: string;
  revision?: string;
  variant?: string;
  files?: string[];
}

/** `POST /api/downloads/cancel`. Confirms. */
export interface DownloadCancelRequest {
  id: number;
}

/** `POST /api/templates/list-repo`. */
export interface TemplateListRepoRequest {
  repo: string;
}

/** `POST /api/templates/fetch` — save a repo's .jinja into the local store. */
export interface TemplateFetchRequest {
  repo: string;
  /** Defaults to the listing's resolved commit. */
  revision?: string;
  path: string;
}

/** `POST /api/templates/apply`. Confirms. Both identities are explicit — no server-side selection. */
export interface TemplateApplyRequest {
  template: string;
  model_path: string;
}

/** `POST /api/templates/revert`. Confirms. */
export interface TemplateRevertRequest {
  model_path: string;
}

/** `POST /api/templates/verify` — render the template against the model's real tokenizer. */
export interface TemplateVerifyRequest {
  template: string;
  model_path: string;
}

/** `POST /api/templates/delete`. Confirms, destructively. */
export interface TemplateDeleteRequest {
  name: string;
}

/** `POST /api/serve/knob` — `value: null` unsets, which is what x / Del do. */
export interface ServeKnobRequest {
  key: string;
  value: string | null;
}

/** Reply to `POST /api/serve/knob`; `cleared` lists knobs the exclusion rules unset. */
export interface ServeKnobReply {
  status: "ok";
  set: boolean;
  cleared: string[];
}

/** `POST /api/serve/flag` — omit `on` to toggle. */
export interface ServeFlagRequest {
  key: string;
  on?: boolean;
}

/** Reply to `POST /api/serve/flag`. */
export interface ServeFlagReply {
  status: "ok";
  on: boolean;
}

/** `POST /api/serve/cycle` — walks a choice knob's options, wrapping through unset. */
export interface ServeCycleRequest {
  key: string;
  /** +1 or -1. */
  delta: number;
}

/** Reply to `POST /api/serve/cycle`; `value` is null when it landed on unset. */
export interface ServeCycleReply {
  status: "ok";
  value: string | null;
}

/** Reply to `POST /api/serve/plan`; `plan` is false when there was nothing to change. */
export interface ServePlanReply {
  status: "ok";
  plan: boolean;
}

/** Reply to `POST /api/serve/plan/apply`; 0 means the configuration already matched. */
export interface ServePlanApplyReply {
  status: "ok";
  changed: number;
}

/** `POST /api/profiles/save`. */
export interface ProfileSaveRequest {
  name: string;
}

/** Reply to `POST /api/profiles/save`; false means an existing profile was updated. */
export interface ProfileSaveReply {
  status: "ok";
  created: boolean;
}

/** `POST /api/profiles/load`. */
export interface ProfileLoadRequest {
  name: string;
}

/** `POST /api/profiles/delete`. Confirms, destructively. */
export interface ProfileDeleteRequest {
  name: string;
}

/** `POST /api/cache/pending` — `value: null` clears the pending edit (the `r` key). */
export interface CachePendingRequest {
  pool: PoolId;
  value: number | null;
}

/** `POST /api/cache/adjust` — the arrow keys: ±0.01, or ±0.10 with Shift. */
export interface CacheAdjustRequest {
  pool: PoolId;
  percent: number;
}

/** Reply to both cache pending routes. */
export interface CachePendingReply {
  status: "ok";
  pending: number | null;
}

/** `POST /api/jobs/cancel`. Confirms, destructively. */
export interface JobCancelRequest {
  id: number;
}

/** Reply to `POST /api/jobs/clear-finished`. */
export interface JobsClearReply {
  status: "ok";
  removed: number;
}

/** `POST /api/requests/pause`. */
export interface RequestsPauseRequest {
  paused: boolean;
}

/** Reply to `POST /api/requests/pause`. */
export interface RequestsPauseReply {
  paused: boolean;
}

/** Reply to `POST /api/hub/files/select`. */
export interface HubFileSelectReply {
  status: "ok";
  selected_count: number;
}

/** Reply to `POST /api/hub/files/toggle`. */
export interface HubFileToggleReply {
  status: "ok";
  wanted: boolean;
}
