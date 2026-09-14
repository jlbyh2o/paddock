//! The `Snapshot` document, built field by field from `App`.
//!
//! Every derived value here comes from the method the TUI's own views call — the status
//! line from `App::engine_status_text`, the pool table from `views::cache`, the guidance
//! bullets from `views::models`. Nothing is re-derived, because a browser that recomputed
//! any of it would sooner or later disagree with the terminal about the same machine.

use serde::Serialize;

use crate::compat::Report;
use crate::ft::proc::JobStatus;
use crate::ft::types::*;
use crate::hub::{DownloadStatus, RepoFile, RepoInfo, RepoSummary};
use crate::knobs::Group;
use crate::models::Model;
use crate::plan::ContextFit;
use crate::probe::{Gpu, Host};
use crate::templates::TemplateMeta;
use crate::ui::app::{App, Pool};
use crate::ui::views;
use crate::ui::widgets::{ConfirmAction, ToastKind};

/// The most recent samples any sparkline needs. `History` holds 240; sending half of it
/// keeps the whole document inside the size budget and no browser draws more.
const SERIES_TAIL: usize = 120;

/// The template preview is the one unbounded string in the snapshot.
const PREVIEW_CAP: usize = 8 * 1024;

#[derive(Serialize)]
pub struct Snapshot<'a> {
    pub seq: u64,
    pub ts_ms: i64,
    pub version: &'static str,
    pub engine: EngineSnapshot<'a>,
    pub telemetry: TelemetrySnapshot<'a>,
    pub series: SeriesSnapshot<'a>,
    pub hardware: HardwareSnapshot<'a>,
    pub models: ModelsSnapshot<'a>,
    pub hub: HubSnapshot<'a>,
    pub templates: TemplatesSnapshot<'a>,
    pub serve: ServeSnapshot<'a>,
    pub cache: CacheSnapshot,
    pub jobs: JobsSnapshot,
    pub requests: RequestsSnapshot,
    pub logs: LogsSnapshot,
    pub toasts: Vec<ToastOut<'a>>,
    pub confirm: Option<ConfirmOut<'a>>,
    pub config: ConfigSnapshot,
    pub environment: EnvironmentSnapshot<'a>,
}

/// Build the whole document. `seq` is assigned by the caller so it stays monotonic across
/// every snapshot the process builds, whoever asked for it.
pub fn build(app: &App, seq: u64) -> Snapshot<'_> {
    Snapshot {
        seq,
        ts_ms: chrono::Utc::now().timestamp_millis(),
        version: env!("CARGO_PKG_VERSION"),
        engine: engine(app),
        telemetry: telemetry(app),
        series: series(app),
        hardware: hardware(app),
        models: models(app),
        hub: hub(app),
        templates: templates(app),
        serve: serve(app),
        cache: cache(app),
        jobs: jobs(app),
        requests: requests(app),
        logs: logs(app),
        toasts: app.toasts.iter().map(toast).collect(),
        confirm: app.confirm.as_ref().map(confirm),
        config: config(app),
        environment: environment(app),
    }
}

// ---------------------------------------------------------------- engine

#[derive(Serialize)]
pub struct EngineSnapshot<'a> {
    state: &'a crate::ft::EngineState,
    is_live: bool,
    status_text: String,
    status_class: &'static str,
    pid: Option<u32>,
    adopted: bool,
    model: Option<String>,
    port: Option<u16>,
    command_line: Option<&'a str>,
    log_path: Option<String>,
    endpoint: &'a str,
    server_reachable: bool,
    context_fit: Option<ContextFitOut>,
    prefix_reuse: Option<ReuseOut>,
    completed_rate: f64,
    active_jobs: usize,
    active_downloads: usize,
    gpu_busy_reason: Option<String>,
    start_blocked: Option<String>,
}

fn engine(app: &App) -> EngineSnapshot<'_> {
    EngineSnapshot {
        state: &app.engine.state,
        is_live: app.engine.is_live(),
        status_text: app.engine_status_text(),
        status_class: app.engine_status_class(),
        pid: app.engine.pid,
        adopted: app.engine.state == crate::ft::EngineState::Adopted,
        model: app.current_model(),
        port: app.engine.port,
        command_line: app.engine.command_line.as_deref(),
        log_path: app.engine.log_path.as_ref().map(|p| p.display().to_string()),
        endpoint: app.client.base_url(),
        server_reachable: app.server_reachable(),
        context_fit: app.context_fit().map(context_fit),
        prefix_reuse: app.prefix_reuse().map(|r| ReuseOut {
            fraction: r.fraction,
            cold_rate: r.cold_rate,
            samples: r.samples,
            summary: r.summary(),
        }),
        completed_rate: app.completed_rate.get(),
        active_jobs: app.active_jobs(),
        active_downloads: app.active_downloads(),
        gpu_busy_reason: app.gpu_busy_reason(),
        // The same predicate `POST /api/engine/start` runs, so a button that offers to
        // start cannot disagree with the daemon that would refuse.
        start_blocked: crate::actions::start_blocked(app),
    }
}

#[derive(Serialize)]
pub struct ContextFitOut {
    usable: u64,
    ceiling: u64,
    is_truncated: bool,
    ratio: f64,
    summary: String,
    verdict: String,
}

fn context_fit(fit: ContextFit) -> ContextFitOut {
    ContextFitOut {
        usable: fit.usable,
        ceiling: fit.ceiling,
        is_truncated: fit.is_truncated(),
        ratio: fit.ratio(),
        summary: fit.summary(),
        // The plan overlay's headline, worded once: the terminal and the browser print it
        // side by side on the same machine.
        verdict: fit.verdict(),
    }
}

#[derive(Serialize)]
pub struct ReuseOut {
    fraction: f64,
    cold_rate: f64,
    samples: usize,
    summary: String,
}

// ---------------------------------------------------------------- telemetry

#[derive(Serialize)]
pub struct TelemetrySnapshot<'a> {
    health: Option<&'a Health>,
    stats: Option<&'a Stats>,
    cache_status: Option<&'a CacheStatus>,
    error: Option<&'a str>,
    age_ms: Option<u128>,
    health_load_ratio: Option<f64>,
    pool_bytes: Option<PoolBytesOut>,
    total_experts: Option<u64>,
    kv_used_tokens: Option<u64>,
    kv_total_tokens: Option<u64>,
    kv_ratio: Option<f64>,
    swa_used_tokens: Option<u64>,
    swa_total_tokens: Option<u64>,
    swa_ratio: Option<f64>,
    mamba_ratio: Option<f64>,
    last_rebuild_summary: Option<String>,
    sampling_summary: Option<String>,
}

/// `PoolBytes` plus the total nothing on the wire should have to add up itself.
#[derive(Serialize)]
pub struct PoolBytesOut {
    kv: u64,
    moe: u64,
    mamba: u64,
    swa: u64,
    total: u64,
}

fn pool_bytes(p: PoolBytes) -> PoolBytesOut {
    PoolBytesOut { kv: p.kv, moe: p.moe, mamba: p.mamba, swa: p.swa, total: p.total() }
}

fn telemetry(app: &App) -> TelemetrySnapshot<'_> {
    let t = &app.telemetry;
    let geo = t.cache.as_ref().map(|c| &c.geometry);
    let kv = t.stats.as_ref().and_then(|s| s.kv);
    let swa = t.stats.as_ref().and_then(|s| s.swa);
    TelemetrySnapshot {
        health: t.health.as_ref(),
        stats: t.stats.as_ref(),
        cache_status: t.cache.as_ref(),
        error: t.error.as_deref(),
        age_ms: t.at.map(|at| at.elapsed().as_millis()),
        health_load_ratio: t.health.as_ref().and_then(Health::load_ratio),
        pool_bytes: geo.map(|g| pool_bytes(g.pool_bytes())),
        total_experts: geo.map(CacheGeometry::total_experts),
        kv_used_tokens: kv.map(|p| p.used_tokens()),
        kv_total_tokens: kv.map(|p| p.total_tokens()),
        kv_ratio: kv.map(|p| p.ratio()),
        swa_used_tokens: swa.map(|p| p.used_tokens()),
        swa_total_tokens: swa.map(|p| p.total_tokens()),
        swa_ratio: swa.map(|p| p.ratio()),
        mamba_ratio: t.stats.as_ref().and_then(|s| s.mamba).map(|p| p.ratio()),
        last_rebuild_summary: views::cache::last_rebuild_summary(app),
        sampling_summary: t
            .stats
            .as_ref()
            .and_then(|s| s.model.sampling.as_ref())
            .and_then(views::dashboard::format_sampling),
    }
}

// ---------------------------------------------------------------- series

#[derive(Serialize)]
pub struct SeriesSnapshot<'a> {
    decode_tps: &'a [u64],
    prefill_tps: &'a [u64],
    gpu_util: &'a [u64],
    vram: &'a [u64],
    active: &'a [u64],
    decode_peak: f64,
}

fn series(app: &App) -> SeriesSnapshot<'_> {
    let s = &app.series;
    SeriesSnapshot {
        decode_tps: s.decode_tps.tail(SERIES_TAIL),
        prefill_tps: s.prefill_tps.tail(SERIES_TAIL),
        gpu_util: s.gpu_util.tail(SERIES_TAIL),
        vram: s.vram.tail(SERIES_TAIL),
        active: s.active.tail(SERIES_TAIL),
        decode_peak: s.decode_peak,
    }
}

// ---------------------------------------------------------------- hardware

#[derive(Serialize)]
pub struct HardwareSnapshot<'a> {
    gpu_source: &'static str,
    gpus: Vec<GpuOut<'a>>,
    engine_gpu_uuid: Option<&'a str>,
    reported_gpus: &'a [GpuCard],
    host: HostOut<'a>,
    bench_profile: Option<&'a BenchProfile>,
    bench_summary: Option<String>,
    bench_profile_path: Option<String>,
}

#[derive(Serialize)]
pub struct GpuOut<'a> {
    #[serde(flatten)]
    gpu: &'a Gpu,
    memory_free: u64,
    memory_ratio: f64,
    short_uuid: String,
}

#[derive(Serialize)]
pub struct HostOut<'a> {
    #[serde(flatten)]
    host: &'a Host,
    memory_free: u64,
    memory_ratio: f64,
}

fn hardware(app: &App) -> HardwareSnapshot<'_> {
    let reported: &[GpuCard] = app.telemetry.stats.as_ref().map(|s| &s.gpus[..]).unwrap_or(&[]);
    HardwareSnapshot {
        gpu_source: app.gpu_source,
        gpus: app
            .gpus
            .iter()
            .map(|g| GpuOut {
                gpu: g,
                memory_free: g.memory_free(),
                memory_ratio: g.memory_ratio(),
                short_uuid: g.short_uuid(),
            })
            .collect(),
        engine_gpu_uuid: reported.first().and_then(|g| g.uuid.as_deref()),
        reported_gpus: reported,
        host: HostOut {
            host: &app.host,
            memory_free: app.host.memory_free(),
            memory_ratio: app.host.memory_ratio(),
        },
        bench_profile: app.bench_profile.as_ref(),
        bench_summary: app.bench_summary(),
        // Resolved when the profile was loaded, not now: finding it reads a directory.
        bench_profile_path: app.bench_profile_path.as_ref().map(|p| p.display().to_string()),
    }
}

// ---------------------------------------------------------------- models

#[derive(Serialize)]
pub struct ModelsSnapshot<'a> {
    scanning: bool,
    items: Vec<ModelEntry<'a>>,
    roots: Vec<RootOut>,
    config_path: String,
}

#[derive(Serialize)]
pub struct RootOut {
    path: String,
    exists: bool,
}

#[derive(Serialize)]
pub struct ModelEntry<'a> {
    #[serde(flatten)]
    model: &'a Model,
    format_label: &'static str,
    format_description: String,
    modified_ms: Option<i64>,
    summary: String,
    served_name: String,
    convertible: bool,
    is_partial: bool,
    template_status: crate::templates::Status,
    template_targets: Vec<String>,
    ftw_output_path: String,
    guidance: Vec<Guidance>,
}

#[derive(Serialize)]
pub struct Guidance {
    level: &'static str,
    text: String,
}

fn models(app: &App) -> ModelsSnapshot<'_> {
    let ftw_dir = app.config.library.ftw_dir();
    ModelsSnapshot {
        scanning: app.models_view.scanning,
        items: app
            .models
            .iter()
            .map(|m| ModelEntry {
                model: m,
                format_label: m.format.label(),
                format_description: views::models::format_description(m),
                modified_ms: m.modified.and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
                }),
                summary: m.summary(),
                served_name: m.served_name(),
                convertible: m.convertible(),
                is_partial: m.is_partial(),
                template_status: app.template_status(m),
                template_targets: crate::templates::targets(m)
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
                ftw_output_path: crate::models::ftw_output_path(
                    &m.path,
                    m.repo.as_deref(),
                    m.variant.as_deref(),
                    &ftw_dir,
                )
                .display()
                .to_string(),
                guidance: views::models::guidance_notes(app, m)
                    .into_iter()
                    .map(|(level, text)| Guidance { level, text })
                    .collect(),
            })
            .collect(),
        // Recorded by the scan, which runs on the blocking pool. Stat-ing a root here
        // would do it under the mutex every connected browser shares.
        roots: app
            .model_roots
            .iter()
            .map(|r| RootOut { exists: r.exists, path: r.path.display().to_string() })
            .collect(),
        config_path: crate::config::config_path().display().to_string(),
    }
}

// ---------------------------------------------------------------- hub

#[derive(Serialize)]
pub struct HubSnapshot<'a> {
    query: &'a str,
    searching: bool,
    results: Vec<RepoSummaryOut<'a>>,
    revision: &'a str,
    loading_info: bool,
    info: Option<RepoInfoOut<'a>>,
    layout: Option<LayoutOut<'a>>,
    variant: Option<&'a str>,
    custom_selection: bool,
    files: &'a [RepoFile],
    selected_bytes: u64,
    selected_count: usize,
    compat: Option<CompatOut<'a>>,
    compat_error: Option<&'a str>,
    checking_compat: bool,
    target: &'a str,
    disk_free: Option<DiskFree>,
    hf_cli: Option<String>,
    hf_installing: bool,
    hf_install_command: &'static str,
}

#[derive(Serialize)]
pub struct RepoSummaryOut<'a> {
    #[serde(flatten)]
    repo: &'a RepoSummary,
    is_gated: bool,
    interesting_tags: Vec<&'a str>,
}

#[derive(Serialize)]
pub struct RepoInfoOut<'a> {
    #[serde(flatten)]
    info: &'a RepoInfo,
    is_gated: bool,
}

#[derive(Serialize)]
pub struct LayoutOut<'a> {
    variants: Vec<VariantOut<'a>>,
    shared: &'a [String],
    is_multi: bool,
}

#[derive(Serialize)]
pub struct VariantOut<'a> {
    #[serde(flatten)]
    variant: &'a crate::variants::Variant,
    file_count: usize,
}

#[derive(Serialize)]
pub struct CompatOut<'a> {
    #[serde(flatten)]
    report: &'a Report,
    verdict: crate::compat::Verdict,
    verdict_label: &'static str,
    summary: String,
}

/// Free space and — mandatory beside it — the directory the figure was taken from.
#[derive(Serialize)]
pub struct DiskFree {
    measured_path: String,
    free_bytes: u64,
}

/// A sampled free-space figure, as the wire wants it.
///
/// Sampled, never measured here: `hub::disk_free_at` is a `statvfs` plus a walk up to an
/// existing ancestor, and a snapshot is built under the one `App` mutex. `App` refreshes
/// both figures on the hardware tick and after anything that moves real bytes.
fn disk_free(sample: Option<&(std::path::PathBuf, u64)>) -> Option<DiskFree> {
    sample.map(|(measured, free)| DiskFree {
        measured_path: measured.display().to_string(),
        free_bytes: *free,
    })
}

fn hub(app: &App) -> HubSnapshot<'_> {
    let v = &app.hub_view;
    let layout = &v.layout;
    let (selected_bytes, selected_count) = v.selected();
    HubSnapshot {
        query: &v.query.value,
        searching: v.searching,
        results: v
            .results
            .iter()
            .map(|r| RepoSummaryOut {
                repo: r,
                is_gated: r.is_gated(),
                interesting_tags: r.interesting_tags(),
            })
            .collect(),
        revision: &v.revision,
        loading_info: v.loading_info,
        info: v.info.as_ref().map(|i| RepoInfoOut { info: i, is_gated: i.is_gated() }),
        // An unanalyzed layout is absence, not an empty grouping: no repo has been opened.
        layout: (!layout.variants.is_empty() || !layout.shared.is_empty()).then(|| LayoutOut {
            variants: layout
                .variants
                .iter()
                .map(|v| VariantOut { variant: v, file_count: v.file_count() })
                .collect(),
            shared: &layout.shared,
            is_multi: layout.is_multi(),
        }),
        variant: v.variant.as_deref(),
        custom_selection: v.custom_selection,
        files: &v.files,
        selected_bytes,
        selected_count,
        compat: v.compat.as_ref().map(|r| CompatOut {
            report: r,
            verdict: r.verdict(),
            verdict_label: r.verdict().label(),
            summary: r.summary(),
        }),
        compat_error: v.compat_error.as_deref(),
        checking_compat: v.checking_compat,
        target: &v.target.value,
        disk_free: disk_free(app.disk_free_target.as_ref()),
        hf_cli: app.hf_cli.as_ref().map(|p| p.display().to_string()),
        hf_installing: app.hf_installing,
        hf_install_command: crate::hub::INSTALL_COMMAND,
    }
}

// ---------------------------------------------------------------- templates

#[derive(Serialize)]
pub struct TemplatesSnapshot<'a> {
    stored: Vec<StoredTemplateOut<'a>>,
    repo: &'a str,
    loading: bool,
    remote: &'a [crate::hub::Sibling],
    remote_repo: Option<&'a str>,
    remote_revision: Option<&'a str>,
    remote_stored_names: Vec<String>,
    preview: Option<PreviewOut<'a>>,
    checking: bool,
    preflight: Option<PreflightOut<'a>>,
    sources: &'a [String],
    preflight_enabled: bool,
}

#[derive(Serialize)]
pub struct StoredTemplateOut<'a> {
    name: &'a str,
    path: String,
    size: u64,
    meta: &'a TemplateMeta,
    subtitle: String,
}

#[derive(Serialize)]
pub struct PreviewOut<'a> {
    name: &'a str,
    text: &'a str,
    truncated: bool,
}

#[derive(Serialize)]
pub struct PreflightOut<'a> {
    template: &'a str,
    outcome: &'a crate::ft::Preflight,
}

/// Cut a template at the byte cap without splitting a character in half.
pub fn capped(text: &str) -> (&str, bool) {
    if text.len() <= PREVIEW_CAP {
        return (text, false);
    }
    let mut end = PREVIEW_CAP;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

fn templates(app: &App) -> TemplatesSnapshot<'_> {
    let v = &app.templates_view;
    TemplatesSnapshot {
        stored: v
            .stored
            .iter()
            .map(|t| StoredTemplateOut {
                name: &t.name,
                path: t.path.display().to_string(),
                size: t.size,
                meta: &t.meta,
                subtitle: t.subtitle(),
            })
            .collect(),
        repo: &v.repo.value,
        loading: v.loading,
        remote: &v.remote,
        remote_repo: v.remote_repo.as_deref(),
        remote_revision: v.remote_revision.as_deref(),
        // The naming rule lives here; the browser only needs to know which listed files
        // it would land on top of.
        remote_stored_names: match &v.remote_repo {
            Some(repo) => {
                v.remote.iter().map(|f| crate::templates::name_for(repo, &f.path)).collect()
            }
            None => Vec::new(),
        },
        preview: v.preview.as_ref().map(|(name, text)| {
            let (text, truncated) = capped(text);
            PreviewOut { name, text, truncated }
        }),
        checking: v.checking,
        preflight: v
            .preflight
            .as_ref()
            .map(|(template, outcome)| PreflightOut { template, outcome }),
        sources: &app.config.templates.sources,
        preflight_enabled: app.config.templates.preflight,
    }
}

// ---------------------------------------------------------------- serve

#[derive(Serialize)]
pub struct ServeSnapshot<'a> {
    values: &'a crate::knobs::ServeConfig,
    errors: Vec<KnobError>,
    command_preview: String,
    set_counts: std::collections::BTreeMap<Group, usize>,
    plan: Option<PlanOut<'a>>,
    profiles: Vec<ProfileOut<'a>>,
    last_used_profile: Option<&'a str>,
}

#[derive(Serialize)]
pub struct KnobError {
    key: String,
    /// The knob's flag spelling, resolved here rather than in the browser: `errors[].key`
    /// is whatever the configuration held, which for a profile from a newer FreeToken is a
    /// key the schema does not know. `null` then, and the client prints the key.
    flag: Option<&'static str>,
    message: String,
}

#[derive(Serialize)]
pub struct PlanOut<'a> {
    steps: Vec<PlanStepOut<'a>>,
    fit: Option<ContextFitOut>,
    unpriced: Option<&'a str>,
    is_empty: bool,
    edit_count: usize,
}

#[derive(Serialize)]
pub struct PlanStepOut<'a> {
    level: crate::plan::Level,
    label: String,
    key: Option<&'static str>,
    value: Option<&'a str>,
    reason: &'a str,
}

#[derive(Serialize)]
pub struct ProfileOut<'a> {
    name: &'a str,
    notes: &'a str,
    model: Option<&'a str>,
}

fn serve(app: &App) -> ServeSnapshot<'_> {
    let program = app.ft.as_ref().map(|f| f.display_program()).unwrap_or_else(|| "ft".into());
    ServeSnapshot {
        values: &app.serve,
        errors: app
            .serve
            .validate()
            .into_iter()
            .map(|(key, message)| KnobError {
                flag: crate::knobs::knob(&key).map(|k| k.flag),
                key,
                message,
            })
            .collect(),
        command_preview: app.serve.preview(&program),
        set_counts: Group::ALL
            .iter()
            .map(|g| {
                let n = crate::knobs::knobs_in(*g).filter(|k| app.serve.is_set(k.key)).count();
                (*g, n)
            })
            .collect(),
        plan: app.serve_view.plan.as_ref().map(|p| PlanOut {
            steps: p
                .steps
                .iter()
                .map(|s| PlanStepOut {
                    level: s.level,
                    label: s.label(),
                    key: s.set.as_ref().map(|(k, _)| *k),
                    value: s.set.as_ref().map(|(_, v)| v.as_str()),
                    reason: &s.reason,
                })
                .collect(),
            fit: p.fit.map(context_fit),
            unpriced: p.unpriced.as_deref(),
            is_empty: p.is_empty(),
            edit_count: p.edits().len(),
        }),
        profiles: app
            .profiles
            .items
            .iter()
            .map(|p| ProfileOut { name: &p.name, notes: &p.notes, model: p.serve.get("model") })
            .collect(),
        last_used_profile: app.profiles.last_used.as_deref(),
    }
}

// ---------------------------------------------------------------- cache

#[derive(Serialize)]
pub struct CacheSnapshot {
    state: Option<String>,
    applying: bool,
    has_pending: bool,
    pools: Option<Vec<PoolRow>>,
    current_bytes: Option<PoolBytesOut>,
    proposed_bytes: Option<u64>,
    delta_bytes: Option<i64>,
    budget_bytes: Option<u64>,
    over_budget: bool,
    budget_ratio: Option<f64>,
    facts: Vec<String>,
    last_rebuild_summary: Option<String>,
    active_requests: u64,
}

#[derive(Serialize)]
pub struct PoolRow {
    pool: Pool,
    label: &'static str,
    unit: &'static str,
    current: u64,
    max: u64,
    min: u64,
    pending: Option<u64>,
    shown: u64,
    delta: Option<i64>,
    ratio: f64,
    note: String,
}

fn cache(app: &App) -> CacheSnapshot {
    let active = app.telemetry.stats.as_ref().map(|s| s.requests.active).unwrap_or(0);
    let Some(geo) = app.telemetry.cache.as_ref().map(|c| &c.geometry) else {
        return CacheSnapshot {
            state: None,
            applying: app.cache_view.applying,
            has_pending: app.cache_view.has_pending(),
            pools: None,
            current_bytes: None,
            proposed_bytes: None,
            delta_bytes: None,
            budget_bytes: None,
            over_budget: false,
            budget_ratio: None,
            facts: Vec::new(),
            last_rebuild_summary: None,
            active_requests: active,
        };
    };

    let rows: Vec<PoolRow> = Pool::ALL
        .iter()
        .copied()
        .filter(|p| crate::cache_pools::present(geo, *p))
        .map(|pool| {
            // One implementation of the bounds, shared with the Cache view and with the
            // clamping `POST /api/cache/pending` does.
            let bounds = crate::cache_pools::geometry(geo, pool);
            let pending = app.cache_view.pending_for(pool);
            let shown = pending.unwrap_or(bounds.current);
            PoolRow {
                pool,
                label: pool.label(),
                unit: bounds.unit,
                current: bounds.current,
                max: bounds.max,
                min: bounds.min,
                pending,
                shown,
                delta: pending.map(|p| p as i64 - bounds.current as i64),
                ratio: crate::util::ratio(shown, bounds.max),
                note: crate::cache_pools::note(geo, pool, shown),
            }
        })
        .collect();

    let current = geo.pool_bytes();
    let has_pending = app.cache_view.has_pending();
    let proposed = has_pending.then(|| views::cache::proposed_bytes(app, geo));
    let budget = geo.cache_budget_bytes;
    let against = proposed.unwrap_or(current.total());

    CacheSnapshot {
        state: Some(app.telemetry.cache.as_ref().map(|c| c.state.clone()).unwrap_or_default()),
        applying: app.cache_view.applying,
        has_pending,
        pools: Some(rows),
        current_bytes: Some(pool_bytes(current)),
        proposed_bytes: proposed,
        delta_bytes: proposed.map(|p| p as i64 - current.total() as i64),
        budget_bytes: Some(budget),
        over_budget: budget > 0 && against > budget,
        budget_ratio: (budget > 0).then(|| crate::util::ratio(against, budget)),
        facts: views::cache::facts(geo),
        last_rebuild_summary: views::cache::last_rebuild_summary(app),
        active_requests: active,
    }
}

// ---------------------------------------------------------------- jobs

#[derive(Serialize)]
pub struct JobsSnapshot {
    items: Vec<JobEntry>,
    downloads: Vec<DownloadEntry>,
    convert_checking: Option<String>,
}

#[derive(Serialize)]
pub struct JobEntry {
    id: u64,
    kind: crate::ft::proc::JobKind,
    kind_label: &'static str,
    title: String,
    command_line: String,
    status: JobStatus,
    status_label: &'static str,
    progress: crate::ft::proc::JobProgress,
    progress_ratio: Option<f64>,
    progress_detail: String,
    rate_bps: f64,
    elapsed_s: u64,
    started_at: String,
    finished_at: Option<String>,
    log_path: String,
    output_path: Option<String>,
    /// The job's output line counter, so a client polling `GET /api/jobs/{id}/output`
    /// knows there is something new without asking — and without the daemon stat-ing a
    /// file per job per frame. It moves with the job's final status line too, so no extra
    /// read is needed after the job stops.
    output_seq: u64,
    failure_reason: Option<String>,
    is_running: bool,
}

#[derive(Serialize)]
pub struct DownloadEntry {
    id: u64,
    repo: String,
    revision: String,
    target: String,
    total_bytes: u64,
    done_bytes: u64,
    ratio: f64,
    file_count: usize,
    files_done: usize,
    current: String,
    status: DownloadStatus,
    status_label: &'static str,
    rate_bps: f64,
    eta_s: Option<u64>,
    elapsed_s: u64,
    started_at: String,
    finished_at: Option<String>,
    failure_reason: Option<String>,
    is_running: bool,
}

fn jobs(app: &App) -> JobsSnapshot {
    JobsSnapshot {
        items: app
            .jobs
            .iter()
            .map(|j| JobEntry {
                id: j.id,
                kind: j.kind,
                kind_label: j.kind.label(),
                title: j.title.clone(),
                command_line: j.command_line.clone(),
                status_label: j.status.label(),
                status: j.status.clone(),
                progress_ratio: j.progress.ratio(),
                progress_detail: views::jobs::progress_detail(j),
                progress: j.progress.clone(),
                rate_bps: j.rate.get(),
                elapsed_s: j.elapsed().as_secs(),
                started_at: j.started_at.to_rfc3339(),
                finished_at: j.finished_at.map(|t| t.to_rfc3339()),
                log_path: j.log_path.display().to_string(),
                output_path: j.output_path.as_ref().map(|p| p.display().to_string()),
                output_seq: j.log.stats().last_seq,
                failure_reason: matches!(j.status, JobStatus::Failed(_))
                    .then(|| j.failure_reason())
                    .flatten(),
                is_running: j.is_running(),
            })
            .collect(),
        downloads: app
            .downloads
            .iter()
            .map(|d| {
                let done = d.done();
                let rate = d.rate.get();
                DownloadEntry {
                    id: d.id,
                    repo: d.repo.clone(),
                    revision: d.revision.clone(),
                    target: d.target.display().to_string(),
                    total_bytes: d.total_bytes,
                    done_bytes: done,
                    ratio: d.ratio(),
                    file_count: d.file_count,
                    files_done: d.files_done,
                    current: d.current.clone(),
                    status_label: d.status.label(),
                    status: d.status.clone(),
                    rate_bps: rate,
                    // Below a byte a second the arithmetic produces centuries, which is
                    // worse than saying nothing; the TUI prints `--` for the same reason.
                    eta_s: (rate > 1.0)
                        .then(|| (d.total_bytes.saturating_sub(done) as f64 / rate) as u64),
                    elapsed_s: d.elapsed().as_secs(),
                    started_at: d.started_at.to_rfc3339(),
                    finished_at: d.finished_at.map(|t| t.to_rfc3339()),
                    failure_reason: d.status.failure_reason().map(str::to_string),
                    is_running: d.is_running(),
                }
            })
            .collect(),
        convert_checking: app.convert_checking.as_ref().map(|p| p.display().to_string()),
    }
}

// ---------------------------------------------------------------- chrome

#[derive(Serialize)]
pub struct ToastOut<'a> {
    id: u64,
    text: &'a str,
    kind: ToastKind,
    age_ms: u128,
    ttl_ms: u128,
}

fn toast(t: &crate::ui::widgets::Toast) -> ToastOut<'_> {
    ToastOut {
        id: t.id,
        text: &t.text,
        kind: t.kind,
        age_ms: t.at.elapsed().as_millis(),
        ttl_ms: t.ttl().as_millis(),
    }
}

#[derive(Serialize)]
pub struct ConfirmOut<'a> {
    title: &'a str,
    body: &'a [String],
    options: &'a [String],
    default_index: usize,
    destructive: bool,
    action: &'a ConfirmAction,
}

fn confirm(c: &crate::ui::widgets::Confirm) -> ConfirmOut<'_> {
    ConfirmOut {
        title: &c.title,
        body: &c.body,
        options: &c.options,
        default_index: c.selected,
        destructive: c.destructive,
        action: &c.action,
    }
}

#[derive(Serialize)]
pub struct RequestsSnapshot {
    paused: bool,
    count: usize,
    first_seq: u64,
    last_seq: u64,
    dropped: u64,
    engine_cursor: u64,
}

fn requests(app: &App) -> RequestsSnapshot {
    let v = &app.requests_view;
    RequestsSnapshot {
        paused: v.paused,
        count: v.entries.len(),
        first_seq: v.first_seq(),
        last_seq: v.last_seq(),
        dropped: v.dropped(),
        engine_cursor: v.cursor,
    }
}

#[derive(Serialize)]
pub struct LogsSnapshot {
    count: usize,
    first_seq: u64,
    last_seq: u64,
    dropped: u64,
    capacity: usize,
    log_path: Option<String>,
}

fn logs(app: &App) -> LogsSnapshot {
    let stats = app.engine.log.stats();
    LogsSnapshot {
        count: stats.count,
        first_seq: stats.first_seq,
        last_seq: stats.last_seq,
        dropped: stats.dropped,
        capacity: app.config.ui.log_capacity,
        log_path: app.engine.log_path.as_ref().map(|p| p.display().to_string()),
    }
}

// ---------------------------------------------------------------- config

#[derive(Serialize)]
pub struct ConfigSnapshot {
    theme: String,
    confirm_destructive: bool,
    tick_ms: u64,
    log_capacity: usize,
    poll_ms: u64,
    server_host: String,
    server_port: u16,
    download_dir: String,
    ftw_dir: String,
    hub_cache: String,
    hub_endpoint: String,
    hub_concurrency: usize,
    hub_ignore: Vec<String>,
    convert_preflight: bool,
    templates_preflight: bool,
    template_sources: Vec<String>,
    config_path: String,
    state_dir: String,
    disk_free: Option<DiskFree>,
}

fn config(app: &App) -> ConfigSnapshot {
    let c = &app.config;
    let download_dir = crate::models::expand_tilde(&c.library.download_dir);
    ConfigSnapshot {
        theme: c.ui.theme.clone(),
        confirm_destructive: c.ui.confirm_destructive,
        tick_ms: c.ui.tick_ms,
        log_capacity: c.ui.log_capacity,
        poll_ms: c.server.poll_ms,
        server_host: c.server.host.clone(),
        server_port: c.server.port,
        disk_free: disk_free(app.disk_free_download.as_ref()),
        download_dir: download_dir.display().to_string(),
        ftw_dir: c.library.ftw_dir().display().to_string(),
        hub_cache: c.library.hub_cache().display().to_string(),
        hub_endpoint: c.hub.endpoint.clone(),
        hub_concurrency: c.hub.concurrency,
        hub_ignore: c.hub.ignore.clone(),
        convert_preflight: c.convert.preflight,
        templates_preflight: c.templates.preflight,
        template_sources: c.templates.sources.clone(),
        config_path: crate::config::config_path().display().to_string(),
        state_dir: crate::config::state_dir().display().to_string(),
    }
}

// ---------------------------------------------------------------- environment

#[derive(Serialize)]
pub struct EnvironmentSnapshot<'a> {
    ft_found: bool,
    ft_program: Option<String>,
    ft_display: Option<String>,
    ft_version: Option<&'a str>,
    ft_origin: Option<&'a str>,
    ft_error: Option<&'a str>,
    supported_archs: Option<&'a [String]>,
    hub_token_present: bool,
    hub_token_source: Option<&'static str>,
    endpoint: &'a str,
    hostname: &'a str,
    // Local FreeToken vendor checkout status.
    ft_upstream_sha: Option<&'a str>,
    ft_origin_sha: Option<&'a str>,
    ft_upstream_behind: Option<usize>,
    ft_origin_ahead: Option<usize>,
    ft_origin_behind: Option<usize>,
    ft_dirty: Option<bool>,
    ft_checkout_note: Option<String>,
    /// The commit the working tree is on.
    ft_local_sha: Option<&'a str>,
    /// Which tree was read. The checkout is found at run time, so the pane has to be
    /// able to say which one it found.
    ft_checkout_path: Option<&'a str>,
    /// The built kernels are older than the last commit to touch their sources: the tree
    /// was pulled but not rebuilt, so the running engine is not the code on disk.
    ft_kernels_stale: Option<bool>,
}

fn environment(app: &App) -> EnvironmentSnapshot<'_> {
    let note = app.ft_checkout.as_ref().map(checkout_note);
    EnvironmentSnapshot {
        ft_found: app.ft.is_some(),
        ft_program: app.ft.as_ref().map(|f| f.program.display().to_string()),
        ft_display: app.ft.as_ref().map(|f| f.display_program()),
        ft_version: app.ft_version.as_deref(),
        ft_origin: app.ft.as_ref().map(|f| f.origin.as_str()),
        ft_error: app.ft_error.as_deref(),
        supported_archs: app.supported_archs.as_deref(),
        hub_token_present: app.hub_token.is_some(),
        hub_token_source: app.hub_token.as_ref().map(|t| t.source),
        endpoint: app.client.base_url(),
        hostname: &app.host.hostname,
        ft_upstream_sha: app.ft_checkout.as_ref().map(|c| c.upstream_sha.as_str()),
        ft_origin_sha: app.ft_checkout.as_ref().map(|c| c.origin_sha.as_str()),
        ft_upstream_behind: app.ft_checkout.as_ref().map(|c| c.upstream_behind),
        ft_origin_ahead: app.ft_checkout.as_ref().map(|c| c.origin_ahead),
        ft_origin_behind: app.ft_checkout.as_ref().map(|c| c.origin_behind),
        ft_dirty: app.ft_checkout.as_ref().map(|c| c.dirty),
        ft_local_sha: app.ft_checkout.as_ref().map(|c| c.local_sha.as_str()),
        ft_checkout_path: app.ft_checkout.as_ref().map(|c| c.path.as_str()),
        ft_kernels_stale: app.ft_checkout.as_ref().and_then(|c| c.kernels_stale),
        ft_checkout_note: note,
    }
}

/// A short, human-readable summary for the dashboard.
fn checkout_note(c: &crate::ft::FtCheckout) -> String {
    if c.upstream_behind > 0 {
        format!("{} commits behind upstream", c.upstream_behind)
    } else if c.kernels_stale == Some(true) {
        // Current with upstream and still not running that code: the pull landed but
        // the kernels were never rebuilt.
        "kernels need a rebuild".into()
    } else if c.origin_ahead > 0 || c.origin_behind > 0 {
        format!(
            "{} ahead, {} behind origin",
            c.origin_ahead, c.origin_behind
        )
    } else if c.dirty {
        "local changes present".into()
    } else {
        "up to date".into()
    }
}
