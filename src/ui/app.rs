//! Application state and the event loop's model half.
//!
//! `App` owns everything the views read and the background tasks write into. Polling is
//! decoupled from rendering: a telemetry task pushes snapshots over a channel on its own
//! cadence, so a slow or unreachable server slows nothing down visibly.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc;

use crate::config::{Config, HubToken, Profile, Profiles};
use crate::ft::proc::{JobKind, JobProgress, JobStatus};
use crate::ft::{
    api::CacheRebuild, types::*, Client, Engine, EngineEvent, EngineState, Freetoken, Job, JobEvent,
};
use crate::hub::{Download, DownloadEvent, RepoFile, RepoInfo, RepoSummary};
use crate::knobs::{Group, ServeConfig};
use crate::models::Model;
use crate::probe::{Gpu, Host, Probe};
use crate::util::{Ema, History};

use super::theme::Theme;
use super::widgets::{Confirm, Selection, TextInput, Toast, ToastKind};

// ---------------------------------------------------------------- tabs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Models,
    Hub,
    Templates,
    Serve,
    Cache,
    Jobs,
    Requests,
    Logs,
}

impl Tab {
    pub const ALL: [Tab; 9] = [
        Tab::Dashboard,
        Tab::Models,
        Tab::Hub,
        Tab::Templates,
        Tab::Serve,
        Tab::Cache,
        Tab::Jobs,
        Tab::Requests,
        Tab::Logs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Models => "Models",
            Tab::Hub => "Hub",
            Tab::Templates => "Templates",
            Tab::Serve => "Serve",
            Tab::Cache => "Cache",
            Tab::Jobs => "Jobs",
            Tab::Requests => "Requests",
            Tab::Logs => "Logs",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn next(self) -> Tab {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Tab {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn from_digit(d: u32) -> Option<Tab> {
        (d >= 1).then(|| Self::ALL.get(d as usize - 1).copied()).flatten()
    }
}

// ---------------------------------------------------------------- telemetry

/// One poll of the server's control plane. `Option` throughout: a server that is still
/// loading answers `/health` long before `/v1/stats` means anything.
#[derive(Debug, Default)]
pub struct Telemetry {
    pub health: Option<Health>,
    pub stats: Option<Stats>,
    pub cache: Option<CacheStatus>,
    pub error: Option<String>,
    pub at: Option<Instant>,
}

/// The chat templates a repo holds, at the revision the listing resolved to.
#[derive(Debug)]
pub struct TemplateListing {
    pub repo: String,
    pub revision: String,
    pub files: Vec<crate::hub::Sibling>,
}

#[derive(Debug)]
pub enum Message {
    Telemetry(Box<Telemetry>),
    /// The `hf` installer finished.
    HfInstalled(Result<std::path::PathBuf, String>),
    Hardware {
        gpus: Vec<Gpu>,
        host: Host,
    },
    Engine(EngineEvent),
    Job(JobEvent),
    Download(DownloadEvent),
    /// A download that finished starting up and is now ready to be tracked.
    RegisterDownload(Box<Download>),
    /// A rescan of the local library finished: the checkpoints, and which roots were
    /// actually there.
    Models {
        items: Vec<Model>,
        roots: Vec<Root>,
    },
    /// A confirmation whose wording needed the filesystem, now that it has been costed on
    /// the blocking pool.
    AskConfirm(Box<Confirm>),
    /// `remove_dir_all` of a checkpoint finished.
    ModelDeleted(PathBuf, Result<(), String>),
    /// The leftovers of a failed conversion are gone; the retry can start.
    LeftoversRemoved(PathBuf, Result<(), String>),
    /// Hub search results.
    HubSearch(Result<Vec<RepoSummary>, String>),
    /// Full repo metadata for the selected result.
    HubInfo(Box<Result<RepoInfo, String>>),
    /// New request-ring entries and the cursor to poll with next.
    Requests {
        entries: Vec<RequestRecord>,
        next_cursor: u64,
    },
    /// FreeToken's model registry, read once at startup.
    Architectures(Result<Vec<String>, String>),
    /// A candidate repo's `config.json`, evaluated for compatibility.
    Compatibility(Box<Result<crate::compat::Report, String>>),
    /// The `.jinja` listing for a template repo.
    TemplateRepo(Box<Result<TemplateListing, String>>),
    /// A template was fetched and saved into the store.
    TemplateFetched(Result<String, String>),
    /// A render preflight finished: (template name, outcome).
    TemplatePreflight(String, crate::ft::Preflight),
    /// A conversion preflight finished: (source path, outcome).
    ConvertPreflight(std::path::PathBuf, crate::ft::Preflight),
    /// A cache rebuild finished.
    CacheRebuilt(Result<String, String>),
    /// The `/generate` smoke test finished.
    SmokeTest(Result<String, String>),
    Toast(Toast),
}

// ---------------------------------------------------------------- per-tab state

/// A configured library root and whether it exists, recorded during the scan.
///
/// The web snapshot is built under the `App` mutex on the runtime, so it may not stat a
/// path; a root on unmounted network storage would block every connected browser.
#[derive(Debug, Clone)]
pub struct Root {
    pub path: PathBuf,
    pub exists: bool,
}

#[derive(Default)]
pub struct ModelsView {
    pub sel: Selection,
    pub filter: TextInput,
    pub filtering: bool,
    pub scanning: bool,
}

/// Which pane of the Hub tab has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HubFocus {
    #[default]
    Results,
    /// The quantization list — the normal way to choose what to download.
    Variants,
    /// The raw file list, kept as the escape hatch for a repo whose layout the grouping
    /// cannot express.
    Files,
}

#[derive(Default)]
pub struct HubView {
    pub query: TextInput,
    pub editing: bool,
    pub results: Vec<RepoSummary>,
    pub sel: Selection,
    pub searching: bool,
    pub info: Option<RepoInfo>,
    pub files: Vec<RepoFile>,
    pub file_sel: Selection,
    /// The repo's files grouped into the quantizations a reader chooses between.
    pub layout: crate::variants::Layout,
    pub variant_sel: Selection,
    /// The chosen quantization. Drives the file selection, the path the engine is pointed
    /// at, and the name it serves the model under.
    pub variant: Option<String>,
    /// True once files have been toggled by hand, so the footer stops claiming the
    /// selection is a quantization it no longer matches.
    pub custom_selection: bool,
    pub focus: HubFocus,
    pub revision: String,
    pub loading_info: bool,
    pub target: TextInput,
    /// Compatibility verdict for the repo whose files are listed.
    pub compat: Option<crate::compat::Report>,
    /// Why the verdict is missing, when the check could not be made.
    pub compat_error: Option<String>,
    pub checking_compat: bool,
}

impl HubView {
    /// How much is selected, and how many files that is.
    ///
    /// Three callers wanted this number — the Files pane title, the download confirmation
    /// and the web snapshot — and three folds over the same vector is three chances to
    /// count `wanted` differently.
    pub fn selected(&self) -> (u64, usize) {
        self.files.iter().filter(|f| f.wanted).fold((0, 0), |(b, n), f| (b + f.size, n + 1))
    }
}

pub struct ServeView {
    pub group: Group,
    pub sel: Selection,
    pub editing: bool,
    pub editor: TextInput,
    /// Index into a `Choice` knob's options while cycling.
    pub profile_name: TextInput,
    pub naming: bool,
    pub show_preview: bool,
    pub profile_sel: Selection,
    pub in_profiles: bool,
    /// The last plan built for this configuration, shown until it is applied or the
    /// model changes under it.
    pub plan: Option<crate::plan::Plan>,
}

impl Default for ServeView {
    fn default() -> Self {
        Self {
            group: Group::Model,
            sel: Selection::default(),
            editing: false,
            editor: TextInput::default(),
            profile_name: TextInput::default(),
            naming: false,
            show_preview: false,
            profile_sel: Selection::default(),
            in_profiles: false,
            plan: None,
        }
    }
}

/// The four resizable pools, in the order the Cache view lists them.
///
/// `Deserialize` as well as `Serialize`: the web API names a pool by exactly these
/// strings, so the route bodies parse straight into this rather than into a copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pool {
    Moe,
    Kv,
    Mamba,
    Swa,
}

impl Pool {
    pub const ALL: [Pool; 4] = [Pool::Moe, Pool::Kv, Pool::Mamba, Pool::Swa];
    pub fn label(self) -> &'static str {
        match self {
            Pool::Moe => "MoE expert slots",
            Pool::Kv => "KV pages",
            Pool::Mamba => "GDN state slots",
            Pool::Swa => "SWA window pages",
        }
    }
    pub fn unit(self) -> &'static str {
        match self {
            Pool::Moe => "slots",
            Pool::Kv => "pages",
            Pool::Mamba => "slots",
            Pool::Swa => "pages",
        }
    }
}

#[derive(Default)]
pub struct CacheView {
    pub sel: Selection,
    /// Pending edits, keyed by pool. Absent means "leave this pool alone".
    pub pending: [Option<u64>; 4],
    pub applying: bool,
}

impl CacheView {
    pub fn pending_for(&self, pool: Pool) -> Option<u64> {
        self.pending[Pool::ALL.iter().position(|p| *p == pool).unwrap()]
    }
    pub fn set_pending(&mut self, pool: Pool, value: Option<u64>) {
        self.pending[Pool::ALL.iter().position(|p| *p == pool).unwrap()] = value;
    }
    pub fn has_pending(&self) -> bool {
        self.pending.iter().any(Option::is_some)
    }
    pub fn clear_pending(&mut self) {
        self.pending = [None; 4];
    }
}

/// Which pane of the Templates view has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TemplatePane {
    /// The templates already fetched into the store.
    #[default]
    Store,
    /// The `.jinja` files in the repo being browsed.
    Remote,
}

#[derive(Default)]
pub struct TemplatesView {
    pub sel: Selection,
    pub pane: TemplatePane,
    /// Templates in the local store, refreshed from disk.
    pub stored: Vec<crate::templates::StoredTemplate>,
    /// The repo currently being browsed, and its `.jinja` files.
    pub repo: TextInput,
    pub editing_repo: bool,
    pub remote: Vec<crate::hub::Sibling>,
    pub remote_sel: Selection,
    pub remote_repo: Option<String>,
    pub remote_revision: Option<String>,
    pub loading: bool,
    /// A preview of the highlighted template's first lines.
    pub preview: Option<(String, String)>,
    /// Result of the last render preflight, shown beside the template it checked.
    pub preflight: Option<(String, crate::ft::Preflight)>,
    pub checking: bool,
}

#[derive(Default)]
pub struct JobsView {
    pub sel: Selection,
    /// Focus is on the selected job's output rather than the job list.
    pub in_output: bool,
    pub output_scroll: usize,
}

#[derive(Default)]
pub struct RequestsView {
    pub sel: Selection,
    pub cursor: u64,
    pub entries: VecDeque<RequestRecord>,
    pub paused: bool,
    pub show_details: bool,
    /// Records appended since the process started. Counted separately from the engine's
    /// own cursor, which resets whenever the engine restarts, so a browser fetching by
    /// sequence keeps its place across one.
    pushed: u64,
    /// Entries evicted by the ring or discarded by a clear.
    dropped: u64,
}

impl RequestsView {
    /// Append one record, evicting the oldest past the ring's 512.
    pub fn push(&mut self, record: RequestRecord) {
        if self.entries.len() >= 512 {
            self.entries.pop_front();
            self.dropped += 1;
        }
        self.pushed += 1;
        self.entries.push_back(record);
    }

    /// Sequence of the newest record ever appended; 0 before the first one.
    pub fn last_seq(&self) -> u64 {
        self.pushed
    }

    /// Sequence of the oldest retained record, which is `last_seq` when none is.
    pub fn first_seq(&self) -> u64 {
        self.pushed
            .saturating_sub(self.entries.len() as u64)
            .saturating_add(u64::from(!self.entries.is_empty()))
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Retained records after `after`, oldest first, paired with their sequence.
    pub fn since(&self, after: u64, limit: usize) -> Vec<(u64, &RequestRecord)> {
        let first = self.first_seq();
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| (first + i as u64, e))
            .filter(|(seq, _)| *seq > after)
            .take(limit)
            .collect()
    }

    /// Discard every retained record. The sequence keeps counting and `dropped` absorbs
    /// what went, so a reader holding a sequence can still tell where it stands.
    pub fn clear(&mut self) {
        self.dropped += self.entries.len() as u64;
        self.entries.clear();
    }
}

#[derive(Default)]
pub struct LogsView {
    /// Lines from the bottom; 0 means follow the tail.
    pub scroll: usize,
    pub follow: bool,
    pub filter: TextInput,
    pub filtering: bool,
    pub wrap: bool,
    pub errors_only: bool,
}

/// Rolling series behind the Dashboard sparklines.
#[derive(Debug)]
pub struct Series {
    pub decode_tps: History,
    pub prefill_tps: History,
    pub gpu_util: History,
    pub vram: History,
    pub active: History,
    pub decode_peak: f64,
}

impl Default for Series {
    fn default() -> Self {
        Self {
            decode_tps: History::new(240),
            prefill_tps: History::new(240),
            gpu_util: History::new(240),
            vram: History::new(240),
            active: History::new(240),
            decode_peak: 0.0,
        }
    }
}

// ---------------------------------------------------------------- app

pub struct App {
    pub config: Config,
    pub profiles: Profiles,
    pub theme: Theme,
    pub ft: Option<Freetoken>,
    /// Why the CLI could not be found, when it could not be.
    pub ft_error: Option<String>,
    /// The `hf` CLI that Hub downloads are delegated to. `None` means the Hub tab cannot
    /// download anything, which it says rather than failing at the keypress.
    pub hf_cli: Option<std::path::PathBuf>,
    pub hf_installing: bool,
    pub client: Client,

    pub tab: Tab,
    pub should_quit: bool,
    pub show_help: bool,
    pub confirm: Option<Confirm>,
    pub toasts: VecDeque<Toast>,

    pub engine: Engine,
    pub telemetry: Telemetry,
    pub gpus: Vec<Gpu>,
    pub host: Host,
    pub series: Series,
    pub gpu_source: &'static str,

    /// The Hugging Face token, resolved once at startup. `None` means none was found.
    pub hub_token: Option<HubToken>,
    /// The checkpoint whose conversion preflight is in flight, if any.
    pub convert_checking: Option<PathBuf>,
    /// The architectures FreeToken can load; `None` until the registry has been read.
    pub supported_archs: Option<Vec<String>>,
    pub models: Vec<Model>,
    pub jobs: Vec<Job>,
    pub downloads: Vec<Download>,
    pub bench_profile: Option<BenchProfile>,
    /// The file [`Self::bench_profile`] was read from, remembered rather than re-resolved:
    /// finding it globs a directory, and the web snapshot may not touch the filesystem.
    pub bench_profile_path: Option<PathBuf>,
    /// Every configured library root and whether it existed at scan time.
    pub model_roots: Vec<Root>,
    /// Free space at the download directory and at the Hub tab's target, each with the
    /// directory the figure was actually measured at. Sampled once a second on the
    /// hardware tick; `statvfs` on network storage is not something a snapshot may do.
    pub disk_free_download: Option<(PathBuf, u64)>,
    pub disk_free_target: Option<(PathBuf, u64)>,
    /// Per-unit VRAM costs remembered from earlier serves, so a launch can be
    /// planned before the engine that would measure them is running.
    pub cost_store: crate::plan::CostStore,
    cost_store_warned: bool,

    pub serve: ServeConfig,
    pub models_view: ModelsView,
    pub hub_view: HubView,
    pub templates_view: TemplatesView,
    pub serve_view: ServeView,
    pub cache_view: CacheView,
    pub jobs_view: JobsView,
    pub requests_view: RequestsView,
    pub logs_view: LogsView,

    pub tx: mpsc::UnboundedSender<Message>,
    /// The endpoint the telemetry task polls. Rewritten when the serve configuration
    /// changes which host or port the engine will bind.
    pub endpoint_tx: tokio::sync::watch::Sender<String>,
    pub job_tx: mpsc::UnboundedSender<JobEvent>,
    pub download_tx: mpsc::UnboundedSender<DownloadEvent>,
    /// Smoothed request rate for the Dashboard.
    pub completed_rate: Ema,
    last_completed: (Instant, u64),
}

impl App {
    pub fn new(
        config: Config,
        profiles: Profiles,
        ft: Option<Freetoken>,
        ft_error: Option<String>,
        tx: mpsc::UnboundedSender<Message>,
    ) -> Result<Self> {
        let theme = Theme::from_name(&config.ui.theme);

        // Bridge the typed channels the supervisor and job runner use into the single
        // message stream the event loop drains.
        let (engine_tx, mut engine_rx) = mpsc::unbounded_channel();
        let (job_tx, mut job_rx) = mpsc::unbounded_channel();
        let (download_tx, mut download_rx) = mpsc::unbounded_channel();
        {
            let fwd = tx.clone();
            tokio::spawn(async move {
                while let Some(e) = engine_rx.recv().await {
                    if fwd.send(Message::Engine(e)).is_err() {
                        break;
                    }
                }
            });
        }
        {
            let fwd = tx.clone();
            tokio::spawn(async move {
                while let Some(e) = job_rx.recv().await {
                    if fwd.send(Message::Job(e)).is_err() {
                        break;
                    }
                }
            });
        }
        {
            let fwd = tx.clone();
            tokio::spawn(async move {
                while let Some(e) = download_rx.recv().await {
                    if fwd.send(Message::Download(e)).is_err() {
                        break;
                    }
                }
            });
        }

        let mut engine = Engine::new(config.ui.log_capacity, engine_tx);
        let adopted = engine.adopt();

        let mut serve = ServeConfig::new();
        // Seed from the last-used profile so restarting ft-man lands where it left off.
        if let Some(last) = profiles.last_used.as_ref().and_then(|n| profiles.get(n)) {
            serve = last.serve.clone();
        }
        // Seed bind address and port only when the profile did not already pin them:
        // a saved profile that serves on 1920 must keep doing so, and ft-man then polls
        // 1920 rather than the config's default.
        if !serve.is_set("host") {
            serve.set("host", config.server.host.clone());
        }
        if !serve.is_set("port") {
            serve.set("port", config.server.port.to_string());
        }
        if let Some(state) = &adopted {
            serve.set("model", state.model.clone());
            serve.set("port", state.port.to_string());
        }

        let probe = Probe::new();
        let gpu_source = probe.gpu_source;
        let config_hub_token = config.hub.resolve_token();
        let endpoint = endpoint_for(&config, &serve);
        let client = Client::new(&endpoint, Duration::from_millis(config.server.timeout_ms))?;
        let (endpoint_tx, _) = tokio::sync::watch::channel(endpoint);
        // Resolved once here rather than per keypress: the Hub tab needs to say up front
        // that it cannot download, not discover it when someone presses d.
        let hf_cli = crate::hub::locate_cli(&config, ft.as_ref().map(|f| f.program.as_path())).ok();
        let mut app = Self {
            config,
            profiles,
            theme,
            ft,
            ft_error,
            client,
            tab: Tab::Dashboard,
            should_quit: false,
            show_help: false,
            confirm: None,
            toasts: VecDeque::new(),
            engine,
            telemetry: Telemetry::default(),
            gpus: Vec::new(),
            host: Host::default(),
            series: Series::default(),
            gpu_source,
            hub_token: config_hub_token,
            convert_checking: None,
            supported_archs: None,
            models: Vec::new(),
            jobs: Vec::new(),
            downloads: Vec::new(),
            bench_profile: None,
            bench_profile_path: None,
            model_roots: Vec::new(),
            disk_free_download: None,
            disk_free_target: None,
            cost_store: crate::plan::CostStore::load(),
            cost_store_warned: false,
            serve,
            models_view: ModelsView::default(),
            hf_cli,
            hf_installing: false,
            hub_view: HubView { revision: "main".into(), ..Default::default() },
            templates_view: TemplatesView::default(),
            serve_view: ServeView::default(),
            cache_view: CacheView::default(),
            jobs_view: JobsView::default(),
            requests_view: RequestsView::default(),
            logs_view: LogsView { follow: true, wrap: false, ..Default::default() },
            tx,
            endpoint_tx,
            job_tx,
            download_tx,
            completed_rate: Ema::new(0.25),
            last_completed: (Instant::now(), 0),
        };
        app.hub_view.target.set(
            crate::models::expand_tilde(&app.config.library.download_dir).display().to_string(),
        );
        app.reload_bench_profile();
        app.templates_view
            .repo
            .set(app.config.templates.sources.first().cloned().unwrap_or_default());
        app.reload_templates();
        Ok(app)
    }

    // ---- notifications ----------------------------------------------

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push_back(Toast::new(text, kind));
        while self.toasts.len() > 4 {
            self.toasts.pop_front();
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.toast(text, ToastKind::Info);
    }
    pub fn success(&mut self, text: impl Into<String>) {
        self.toast(text, ToastKind::Success);
    }
    pub fn warn(&mut self, text: impl Into<String>) {
        self.toast(text, ToastKind::Warn);
    }
    pub fn error(&mut self, text: impl Into<String>) {
        self.toast(text, ToastKind::Error);
    }

    /// Drop every toast past its lifetime. Returns whether any went, so an idle daemon
    /// does not rebuild a snapshot five times a second to report that nothing happened.
    pub fn expire_toasts(&mut self) -> bool {
        let before = self.toasts.len();
        while self.toasts.front().is_some_and(Toast::is_expired) {
            self.toasts.pop_front();
        }
        self.toasts.len() != before
    }

    // ---- derived state ----------------------------------------------

    /// True when the server answered its last poll.
    pub fn server_reachable(&self) -> bool {
        self.telemetry.health.is_some() && self.telemetry.error.is_none()
    }

    pub fn engine_status_text(&self) -> String {
        if let Some(h) = &self.telemetry.health {
            if h.is_loading() {
                return match h.load_ratio() {
                    Some(r) => format!("loading {:.0}%", r * 100.0),
                    None => format!("loading ({})", h.phase.as_deref().unwrap_or("weights")),
                };
            }
            if h.is_error() {
                return format!("error: {}", h.message.as_deref().unwrap_or("unknown"));
            }
            if h.is_ready() {
                return match h.maintenance.as_deref() {
                    Some("rebuilding") => "rebuilding cache".into(),
                    _ => "serving".into(),
                };
            }
        }
        match &self.engine.state {
            EngineState::Starting => "starting".into(),
            EngineState::Stopping => "stopping".into(),
            EngineState::Exited { code, signal } => {
                format!("exited {}", crate::ft::proc::describe_exit(*code, *signal))
            }
            EngineState::Adopted => "attached (unreachable)".into(),
            EngineState::Running => "running (unreachable)".into(),
            EngineState::Stopped => "not running".into(),
        }
    }

    pub fn engine_status_color(&self) -> ratatui::style::Color {
        if let Some(h) = &self.telemetry.health {
            if h.is_ready() {
                return self.theme.good;
            }
            if h.is_loading() {
                return self.theme.warn;
            }
            if h.is_error() {
                return self.theme.bad;
            }
        }
        match self.engine.state {
            EngineState::Starting | EngineState::Stopping => self.theme.warn,
            EngineState::Exited { .. } => self.theme.bad,
            _ => self.theme.dim,
        }
    }

    /// The severity behind [`Self::engine_status_color`], named rather than colored.
    ///
    /// The two must stay in step: a browser has no access to the theme, so it colors the
    /// status dot from this name, and the terminal colors it from the theme directly.
    pub fn engine_status_class(&self) -> &'static str {
        if let Some(h) = &self.telemetry.health {
            if h.is_ready() {
                return "good";
            }
            if h.is_loading() {
                return "warn";
            }
            if h.is_error() {
                return "bad";
            }
        }
        match self.engine.state {
            EngineState::Starting | EngineState::Stopping => "warn",
            EngineState::Exited { .. } => "bad",
            _ => "dim",
        }
    }

    /// One line describing the machine's bandwidth profile, or `None` when none has been
    /// measured — which the Dashboard turns into the prompt to run a benchmark.
    pub fn bench_summary(&self) -> Option<String> {
        let p = self.bench_profile.as_ref()?;
        let verdicts: Vec<String> = p
            .dtypes
            .iter()
            .filter_map(|(fmt, rec)| rec.as_ref().map(|r| format!("{fmt}→{r}")))
            .collect();
        Some(if verdicts.is_empty() {
            "bandwidth profile present".to_string()
        } else {
            format!("bench: {}", verdicts.join("  "))
        })
    }

    /// The model currently in play: what the server reports, else what is configured.
    pub fn current_model(&self) -> Option<String> {
        self.telemetry
            .stats
            .as_ref()
            .and_then(|s| s.model.id.clone())
            .or_else(|| self.telemetry.health.as_ref().and_then(|h| h.model.clone()))
            .or_else(|| self.engine.model.clone())
    }

    pub fn selected_model(&self) -> Option<&Model> {
        self.filtered_models().get(self.models_view.sel.index).copied()
    }

    /// Library entries matching the current filter, in display order.
    pub fn filtered_models(&self) -> Vec<&Model> {
        let needle = self.models_view.filter.value.to_lowercase();
        self.models
            .iter()
            .filter(|m| {
                needle.is_empty()
                    || m.name.to_lowercase().contains(&needle)
                    || m.path.to_string_lossy().to_lowercase().contains(&needle)
                    || m.arch.as_deref().is_some_and(|a| a.to_lowercase().contains(&needle))
            })
            .collect()
    }

    pub fn active_jobs(&self) -> usize {
        self.jobs.iter().filter(|j| j.is_running()).count()
    }

    pub fn active_downloads(&self) -> usize {
        self.downloads.iter().filter(|d| d.is_running()).count()
    }

    /// A GPU-heavy job and a serve cannot share the card, so both `ft checkpoint` and
    /// `ft bench bw` refuse to start while an engine is up. Surfacing that as a reason
    /// string keeps the explanation in one place.
    pub fn gpu_busy_reason(&self) -> Option<String> {
        if self.engine.is_live() {
            return Some(
                "the engine is running; stop it first (Serve tab, or s on the Dashboard)".into(),
            );
        }
        if let Some(j) = self.jobs.iter().find(|j| j.is_running()) {
            return Some(format!("a {} job is already using the GPU", j.kind.label()));
        }
        None
    }

    /// Re-read the template store from disk and keep the cursor and preview in step.
    pub fn reload_templates(&mut self) {
        self.templates_view.stored = crate::templates::list();
        self.templates_view.sel.clamp(self.templates_view.stored.len());
        self.refresh_template_preview();
    }

    /// The template highlighted in the store pane, if any.
    pub fn selected_template(&self) -> Option<&crate::templates::StoredTemplate> {
        self.templates_view.stored.get(self.templates_view.sel.index)
    }

    /// Load the head of the selected template for the preview pane. Cached by name so a
    /// 30 KiB file is not re-read on every one of the five frames a second.
    pub fn refresh_template_preview(&mut self) {
        let Some(t) = self.selected_template() else {
            self.templates_view.preview = None;
            return;
        };
        if self.templates_view.preview.as_ref().is_some_and(|(n, _)| *n == t.name) {
            return;
        }
        let name = t.name.clone();
        let text = t.read().unwrap_or_else(|e| format!("could not read it: {e:#}"));
        self.templates_view.preview = Some((name, text));
    }

    /// Chat template status for a checkpoint, as the scan recorded it.
    ///
    /// Read from the `Model` rather than from disk: the Models pane asks for every row on
    /// every frame, and the web snapshot asks under a mutex the whole daemon shares.
    /// [`Self::refresh_template_status`] puts it back in step after an apply or a revert.
    pub fn template_status(&self, model: &Model) -> crate::templates::Status {
        model.template_status.clone()
    }

    /// Re-read the on-disk template status for one checkpoint and its FTW build.
    ///
    /// Called from the apply and revert actions, which have just written those very
    /// directories and are already doing filesystem work.
    pub fn refresh_template_status(&mut self, path: &std::path::Path) {
        let mut paths = vec![path.to_path_buf()];
        if let Some(m) = self.models.iter().find(|m| m.path == path) {
            paths.extend(m.converted_to.clone());
        }
        for p in paths {
            let status = crate::templates::status(&p);
            if let Some(m) = self.models.iter_mut().find(|m| m.path == p) {
                m.template_status = status;
            }
        }
    }

    pub fn reload_bench_profile(&mut self) {
        let uuid = self.gpus.first().and_then(|g| (!g.uuid.is_empty()).then(|| g.uuid.clone()));
        self.bench_profile_path = crate::plan::bench_profile_status(uuid.as_deref());
        self.bench_profile = self
            .bench_profile_path
            .as_ref()
            .and_then(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok());
    }

    /// Re-measure free space at the download directory and at the Hub tab's target.
    ///
    /// Both are `statvfs`, both walk up to an existing ancestor, and both are read by
    /// every frame either front end draws — so they are sampled on the hardware tick and
    /// after anything that moves real bytes, never in a render or a snapshot.
    pub fn refresh_disk_free(&mut self) -> bool {
        let download = crate::models::expand_tilde(&self.config.library.download_dir);
        let next_download = crate::hub::disk_free_at(&download.display().to_string());
        let next_target = crate::hub::disk_free_at(&self.hub_view.target.value);
        let changed =
            next_download != self.disk_free_download || next_target != self.disk_free_target;
        self.disk_free_download = next_download;
        self.disk_free_target = next_target;
        changed
    }

    // ---- message handling -------------------------------------------

    pub fn handle(&mut self, msg: Message) {
        match msg {
            Message::HfInstalled(res) => {
                self.hf_installing = false;
                match res {
                    Ok(path) => {
                        self.success(format!("installed the hf CLI at {}", path.display()));
                        self.hf_cli = Some(path);
                    }
                    Err(e) => self.error(format!("could not install the hf CLI: {e}")),
                }
            }
            Message::Telemetry(t) => self.on_telemetry(*t),
            Message::Hardware { gpus, host } => {
                if self.bench_profile.is_none() && self.gpus.is_empty() && !gpus.is_empty() {
                    self.gpus = gpus.clone();
                    self.reload_bench_profile();
                }
                if let Some(g) = gpus.first() {
                    self.series.gpu_util.push(g.utilization.unwrap_or(0) as u64);
                    self.series.vram.push(g.memory_used / (1 << 20));
                }
                self.gpus = gpus;
                self.host = host;
                // Once a second, which is the cadence the probe thread runs at.
                self.refresh_disk_free();
            }
            Message::Engine(e) => self.on_engine(e),
            Message::Job(e) => self.on_job(e),
            Message::Download(e) => self.on_download(e),
            Message::RegisterDownload(dl) => {
                self.downloads.push(*dl);
                self.jobs_view.sel.last(self.jobs.len() + self.downloads.len());
            }
            Message::Models { items, roots } => {
                self.models = items;
                self.model_roots = roots;
                self.models_view.scanning = false;
                self.models_view.sel.clamp(self.filtered_models().len());
            }
            Message::AskConfirm(confirm) => {
                crate::actions::ask(self, *confirm);
            }
            Message::ModelDeleted(path, result) => match result {
                Ok(()) => {
                    self.success(format!("deleted {}", path.display()));
                    self.refresh_disk_free();
                    self.request_scan();
                }
                Err(e) => self.error(format!("could not delete {}: {e}", path.display())),
            },
            Message::LeftoversRemoved(source, result) => match result {
                Ok(()) => {
                    // The cursor follows the checkpoint through the *filtered* list, which
                    // is the one the Models pane draws and indexes.
                    if let Some(i) = self.filtered_models().iter().position(|m| m.path == source) {
                        self.models_view.sel.index = i;
                    }
                    crate::actions::start_after_leftovers(self, &source);
                    self.request_scan();
                }
                Err(e) => self.error(format!("could not remove the leftovers: {e}")),
            },
            Message::HubSearch(res) => {
                self.hub_view.searching = false;
                match res {
                    Ok(items) => {
                        if items.is_empty() {
                            self.warn("no models matched that search");
                        }
                        self.hub_view.results = items;
                        self.hub_view.sel = Selection::default();
                        self.hub_view.info = None;
                        self.hub_view.files.clear();
                        // Everything downstream of the old repo goes with it. Leaving the
                        // grouping behind offered a quantization from the previous search
                        // against a listing that no longer had it, and left the keyboard
                        // in a pane with nothing in it.
                        self.hub_view.layout = crate::variants::Layout::default();
                        self.hub_view.variant = None;
                        self.hub_view.variant_sel = Selection::default();
                        self.hub_view.file_sel = Selection::default();
                        self.hub_view.custom_selection = false;
                        self.hub_view.focus = HubFocus::Results;
                    }
                    Err(e) => self.error(format!("Hub search failed: {e}")),
                }
            }
            Message::HubInfo(res) => {
                self.hub_view.loading_info = false;
                match *res {
                    Ok(info) => {
                        self.hub_view.layout = crate::variants::analyze(&info.siblings);
                        self.hub_view.files =
                            crate::hub::select_files(&info.siblings, &self.config.hub.ignore);
                        self.hub_view.variant_sel = Selection::default();
                        self.hub_view.custom_selection = false;
                        // A repo offering one build needs no question asked; one offering
                        // eleven must not pre-tick an 82 GB answer on the reader's behalf.
                        self.hub_view.variant = None;
                        if self.hub_view.layout.is_multi() {
                            for f in &mut self.hub_view.files {
                                f.wanted = false;
                            }
                            self.hub_view.focus = HubFocus::Variants;
                        } else if let Some(only) =
                            self.hub_view.layout.weights().next().map(|v| v.label.clone())
                        {
                            self.hub_view.variant = Some(only);
                        }
                        // Do NOT clear the compatibility verdict here. Both responses
                        // arrive from one keypress and either can land first; clearing a
                        // stale verdict belongs where the request is made, which is
                        // synchronous and cannot race.
                        self.hub_view.file_sel = Selection::default();
                        // Informational now rather than a destination to edit: the
                        // download goes into the Hugging Face cache, which files a repo
                        // under its org and name so two orgs publishing the same model
                        // name cannot collide.
                        let target =
                            crate::hub::cache_repo_dir(&self.config.library.hub_cache(), &info.id);
                        self.hub_view.target.set(target.display().to_string());
                        self.hub_view.info = Some(info);
                    }
                    Err(e) => self.error(format!("could not read repo metadata: {e}")),
                }
            }
            Message::Requests { entries, next_cursor } => {
                self.requests_view.cursor = next_cursor;
                // Only chase the newest row when the cursor was already on it; a reader
                // who has scrolled back to inspect a failure keeps their place.
                let following = crate::ui::views::requests::at_tail(self);
                for e in entries {
                    self.requests_view.push(e);
                }
                if following {
                    crate::ui::views::requests::follow_tail(self);
                }
            }
            Message::Architectures(res) => match res {
                Ok(names) => self.supported_archs = Some(names),
                // Not fatal: the Hub check then says support is unverified rather than
                // inventing a verdict.
                Err(e) => tracing::warn!("could not read FreeToken's model registry: {e}"),
            },
            Message::Compatibility(res) => {
                self.hub_view.checking_compat = false;
                match *res {
                    Ok(report) => {
                        self.hub_view.compat = Some(report);
                        self.hub_view.compat_error = None;
                    }
                    Err(e) => {
                        self.hub_view.compat = None;
                        tracing::warn!("compatibility check failed: {e}");
                        self.hub_view.compat_error = Some(e);
                    }
                }
            }
            Message::TemplateRepo(res) => {
                self.templates_view.loading = false;
                match *res {
                    Ok(listing) => {
                        if listing.files.is_empty() {
                            self.warn(format!("{} holds no .jinja files", listing.repo));
                        }
                        self.templates_view.remote = listing.files;
                        self.templates_view.remote_repo = Some(listing.repo);
                        self.templates_view.remote_revision = Some(listing.revision);
                        self.templates_view.remote_sel = Selection::default();
                        self.templates_view.pane = TemplatePane::Remote;
                    }
                    Err(e) => self.error(format!("could not list that repo: {e}")),
                }
            }
            Message::TemplateFetched(res) => {
                self.templates_view.loading = false;
                match res {
                    Ok(name) => {
                        self.reload_templates();
                        self.templates_view.pane = TemplatePane::Store;
                        if let Some(i) =
                            self.templates_view.stored.iter().position(|t| t.name == name)
                        {
                            self.templates_view.sel.index = i;
                        }
                        self.success(format!("saved template '{name}'"));
                    }
                    Err(e) => self.error(format!("could not fetch that template: {e}")),
                }
            }
            Message::TemplatePreflight(name, outcome) => {
                use crate::ft::Preflight;
                self.templates_view.checking = false;
                match &outcome {
                    Preflight::Ok(d) => self.success(format!("{name} renders: {d}")),
                    Preflight::Warn(d) => self.warn(format!("{name}: {d}")),
                    Preflight::Fail(e) => self.error(format!("{name} failed to render: {e}")),
                }
                self.templates_view.preflight = Some((name, outcome));
            }
            Message::ConvertPreflight(source, outcome) => {
                self.convert_checking = None;
                if !outcome.is_clean() {
                    tracing::warn!(?source, detail = outcome.detail(), "convert preflight");
                }
                crate::actions::on_convert_preflight(self, source, outcome);
            }
            Message::CacheRebuilt(res) => {
                self.cache_view.applying = false;
                match res {
                    Ok(msg) => {
                        self.cache_view.clear_pending();
                        self.success(msg);
                    }
                    Err(e) => self.error(format!("cache rebuild failed: {e}")),
                }
            }
            Message::SmokeTest(res) => match res {
                Ok(text) => {
                    let preview: String = text.chars().take(120).collect();
                    self.success(format!("smoke test OK: {preview}"));
                }
                Err(e) => self.error(format!("smoke test failed: {e}")),
            },
            Message::Toast(t) => {
                self.toasts.push_back(t);
                while self.toasts.len() > 4 {
                    self.toasts.pop_front();
                }
            }
        }
    }

    fn on_telemetry(&mut self, t: Telemetry) {
        if let Some(s) = &t.stats {
            self.series.decode_tps.push(s.throughput.decode_tps.round() as u64);
            self.series.prefill_tps.push(s.throughput.prefill_tps.round() as u64);
            self.series.active.push(s.requests.active);
            self.series.decode_peak = self.series.decode_peak.max(s.throughput.decode_tps);

            let now = Instant::now();
            let dt = now.duration_since(self.last_completed.0).as_secs_f64();
            if dt >= 1.0 {
                let delta = s.requests.completed.saturating_sub(self.last_completed.1) as f64;
                self.completed_rate.push(delta / dt);
                self.last_completed = (now, s.requests.completed);
            }
        }
        if t.health.as_ref().is_some_and(Health::is_ready) {
            self.engine.mark_ready();
        }
        self.telemetry = t;
        self.record_costs();
    }

    /// Remember what this engine measured, so the next launch of the same model can be
    /// planned exactly instead of not at all.
    ///
    /// The per-unit VRAM costs are only knowable once the model is loaded, which is after
    /// every decision they would have informed. Writing them down turns that into a
    /// one-serve cost. Only a fully ready engine is trusted: a loading one publishes
    /// zeroes, and a rebuilding one is mid-flight.
    fn record_costs(&mut self) {
        if !self.telemetry.health.as_ref().is_some_and(Health::is_ready) {
            return;
        }
        let Some(geo) = self.telemetry.cache.as_ref().map(|c| &c.geometry) else { return };
        let Some(costs) = crate::plan::Costs::from_geometry(geo) else { return };
        let Some(model) = self.current_model() else { return };
        if self.cost_store.observe(&model, costs) {
            if let Err(e) = self.cost_store.save() {
                // Said once, not once per poll: an unwritable state directory does not
                // get better on the next tick, and the only cost is an unpriced plan.
                if !self.cost_store_warned {
                    self.cost_store_warned = true;
                    self.warn(format!("could not record cache costs for planning: {e}"));
                }
            }
        }
    }

    /// The costs to plan `model` against: what the running engine is reporting right now,
    /// else what a previous serve of the same model recorded.
    pub fn costs_for(&self, model: &str) -> Option<crate::plan::Costs> {
        let live = self
            .telemetry
            .cache
            .as_ref()
            .filter(|_| self.current_model().as_deref() == Some(model))
            .and_then(|c| crate::plan::Costs::from_geometry(&c.geometry));
        live.or_else(|| self.cost_store.get(model).copied())
    }

    /// Estimated prefix-cache reuse across the requests currently in the ring.
    ///
    /// `None` whenever the evidence is too thin to say — see [`crate::reuse`]. The
    /// Dashboard then shows nothing rather than a number that could be invented.
    pub fn prefix_reuse(&self) -> Option<crate::reuse::Reuse> {
        let entries: Vec<_> = self.requests_view.entries.iter().cloned().collect();
        crate::reuse::estimate(&entries)
    }

    /// How much of the served model's advertised context the engine can actually use.
    pub fn context_fit(&self) -> Option<crate::plan::ContextFit> {
        let geo = &self.telemetry.cache.as_ref()?.geometry;
        let ceiling = self.telemetry.stats.as_ref()?.model.ctx;
        crate::plan::ContextFit::measure(geo, ceiling)
    }

    fn on_engine(&mut self, e: EngineEvent) {
        match e {
            EngineEvent::Started { pid } => self.info(format!("engine started (pid {pid})")),
            EngineEvent::Exited { code, signal } => {
                let how = crate::ft::proc::describe_exit(code, signal);
                if code == Some(0) || signal == Some(libc::SIGINT) {
                    self.info(format!("engine stopped {how}"));
                } else {
                    self.error(format!("engine exited {how} — see the Logs tab"));
                }
                self.telemetry = Telemetry::default();
            }
        }
    }

    fn on_job(&mut self, e: JobEvent) {
        match e {
            JobEvent::Progress(id, p) => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
                    j.observe(p);
                }
            }
            JobEvent::Line => {}
            JobEvent::Output(id, path) => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
                    j.output_path = Some(path);
                }
            }
            JobEvent::Finished(id, status) => {
                let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) else { return };
                j.status = status.clone();
                j.finished_at = Some(chrono::Local::now());
                j.progress = JobProgress {
                    phase: "done".into(),
                    done: j.progress.total,
                    total: j.progress.total,
                    bytes: j.progress.bytes,
                };
                let (kind, title) = (j.kind, j.title.clone());
                match status {
                    JobStatus::Done => {
                        self.success(format!("{} finished: {title}", kind.label()));
                        self.refresh_disk_free();
                        if kind == JobKind::Convert {
                            self.request_scan();
                        } else {
                            self.reload_bench_profile();
                        }
                    }
                    JobStatus::Failed(why) => {
                        // Prefer what the process actually said over its exit code.
                        let reason = j.failure_reason().unwrap_or(why);
                        self.error(format!("{} failed: {title} — {reason}", kind.label()))
                    }
                    JobStatus::Canceled => self.warn(format!("{} canceled: {title}", kind.label())),
                    JobStatus::Running => {}
                }
            }
        }
    }

    fn on_download(&mut self, e: DownloadEvent) {
        match e {
            DownloadEvent::Progress { id, current, .. } => {
                if let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) {
                    d.current = current;
                }
            }
            DownloadEvent::FileDone { id } => {
                if let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) {
                    d.files_done += 1;
                }
            }
            DownloadEvent::Finished { id, result } => {
                let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) else { return };
                d.finished_at = Some(chrono::Local::now());
                let repo = d.repo.clone();
                match result {
                    Ok(path) => {
                        d.status = crate::hub::DownloadStatus::Done;
                        self.success(format!("downloaded {repo} to {}", path.display()));
                        self.refresh_disk_free();
                        self.request_scan();
                    }
                    Err(e) if e == "canceled" => {
                        d.status = crate::hub::DownloadStatus::Canceled;
                        self.warn(format!("download canceled: {repo}"));
                    }
                    Err(e) => {
                        d.status = crate::hub::DownloadStatus::Failed(e.clone());
                        self.error(format!("download failed ({repo}): {e}"));
                    }
                }
            }
        }
    }

    /// Kick off a library rescan on the blocking pool — a cold scan of a directory
    /// holding several hundred-gigabyte checkpoints does real I/O.
    pub fn request_scan(&mut self) {
        if self.models_view.scanning {
            return;
        }
        self.models_view.scanning = true;
        let roots = self.config.library.effective_roots();
        let ftw_dir = self.config.library.ftw_dir();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let items = crate::models::scan(&roots, &ftw_dir);
            // Whether each root is there is a `stat` too, and this is the one thread
            // allowed to spend one.
            let roots = roots
                .into_iter()
                .map(|path| {
                    let path = crate::models::expand_tilde(&path);
                    Root { exists: path.is_dir(), path }
                })
                .collect();
            let _ = tx.send(Message::Models { items, roots });
        });
    }

    /// Keep the polled endpoint in step with the serve configuration. Called each tick;
    /// a no-op unless the host or port knob actually changed.
    pub fn sync_endpoint(&mut self) -> bool {
        let wanted = endpoint_for(&self.config, &self.serve);
        if wanted == *self.endpoint_tx.borrow() {
            return false;
        }
        match Client::new(&wanted, Duration::from_millis(self.config.server.timeout_ms)) {
            Ok(client) => {
                self.client = client;
                self.telemetry = Telemetry::default();
                self.requests_view.cursor = 0;
                let _ = self.endpoint_tx.send(wanted);
            }
            Err(e) => self.error(format!("could not point at that endpoint: {e:#}")),
        }
        true
    }

    /// One turn of the supervisor. Returns whether anything actually moved.
    ///
    /// The terminal redraws on a timer and does not care, but the web daemon publishes a
    /// snapshot whenever state changes — so a tick that did nothing must say so, or an
    /// idle machine ships five identical documents a second to every open browser and the
    /// heartbeat that proves the stream is alive never gets a turn.
    pub fn tick(&mut self) -> bool {
        let mut changed = self.sync_endpoint();
        changed |= self.engine.poll();
        changed |= self.expire_toasts();
        for d in &mut self.downloads {
            // A finished download's rate is frozen; sampling it again only re-reads a
            // counter that cannot move.
            if d.is_running() {
                d.sample_rate();
                changed = true;
            }
        }
        changed
    }
}

/// The URL ft-man polls: whatever the serve configuration will bind, falling back to
/// the configured default. Either can be a wildcard bind, which `poll_host` turns into
/// the loopback the engine is also listening on.
fn endpoint_for(config: &Config, serve: &ServeConfig) -> String {
    let host =
        serve.get("host").map(str::trim).filter(|h| !h.is_empty()).unwrap_or(&config.server.host);
    let host = crate::config::poll_host(host);
    let port =
        serve.get("port").and_then(|p| p.trim().parse::<u16>().ok()).unwrap_or(config.server.port);
    format!("http://{host}:{port}")
}

/// Build the rebuild request from the Cache view's pending edits.
pub fn rebuild_from_pending(view: &CacheView) -> CacheRebuild {
    CacheRebuild {
        moe_cache_size: view.pending_for(Pool::Moe),
        num_pages: view.pending_for(Pool::Kv),
        num_mamba_slots: view.pending_for(Pool::Mamba),
        num_swa_pages: view.pending_for(Pool::Swa),
        ..Default::default()
    }
}

/// The profile the Serve view would save right now.
pub fn profile_from(name: String, serve: &ServeConfig) -> Profile {
    Profile { name, notes: String::new(), serve: serve.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_cycle_in_both_directions() {
        assert_eq!(Tab::Dashboard.prev(), Tab::Logs);
        assert_eq!(Tab::Logs.next(), Tab::Dashboard);
        assert_eq!(Tab::Dashboard.next(), Tab::Models);
    }

    #[test]
    fn digits_map_to_tabs_one_based() {
        assert_eq!(Tab::from_digit(1), Some(Tab::Dashboard));
        assert_eq!(Tab::from_digit(4), Some(Tab::Templates));
        assert_eq!(Tab::from_digit(Tab::ALL.len() as u32), Some(Tab::Logs));
        assert_eq!(Tab::from_digit(Tab::ALL.len() as u32 + 1), None);
        assert_eq!(Tab::from_digit(0), None);
    }

    #[test]
    fn the_polled_endpoint_follows_the_serve_configuration() {
        let config = Config::default();
        let mut serve = ServeConfig::new();
        assert_eq!(endpoint_for(&config, &serve), "http://127.0.0.1:1919");

        serve.set("port", "1920");
        assert_eq!(endpoint_for(&config, &serve), "http://127.0.0.1:1920");

        serve.set("host", "10.0.0.5");
        assert_eq!(endpoint_for(&config, &serve), "http://10.0.0.5:1920");
    }

    #[test]
    fn a_wildcard_bind_address_is_polled_over_loopback() {
        let config = Config::default();
        for wildcard in ["0.0.0.0", "::", "*"] {
            let mut serve = ServeConfig::new();
            serve.set("host", wildcard);
            assert_eq!(
                endpoint_for(&config, &serve),
                "http://127.0.0.1:1919",
                "binding {wildcard} is not itself a destination"
            );
        }
    }

    #[test]
    fn the_default_bind_address_is_the_wildcard_polled_over_loopback() {
        let config = Config::default();
        assert_eq!(config.server.host, "0.0.0.0", "serve should be reachable off-box by default");
        assert_eq!(config.server.base_url(), "http://127.0.0.1:1919");
        assert_eq!(endpoint_for(&config, &ServeConfig::new()), "http://127.0.0.1:1919");
    }

    #[test]
    fn an_unparseable_port_falls_back_to_the_configured_one() {
        let config = Config::default();
        let mut serve = ServeConfig::new();
        serve.set("port", "not-a-port");
        assert_eq!(endpoint_for(&config, &serve), "http://127.0.0.1:1919");
    }

    #[test]
    fn pending_pool_edits_round_trip() {
        let mut v = CacheView::default();
        assert!(!v.has_pending());
        v.set_pending(Pool::Kv, Some(4096));
        assert_eq!(v.pending_for(Pool::Kv), Some(4096));
        assert_eq!(v.pending_for(Pool::Moe), None);
        assert!(v.has_pending());

        let req = rebuild_from_pending(&v);
        assert_eq!(req.num_pages, Some(4096));
        assert!(req.moe_cache_size.is_none());

        v.clear_pending();
        assert!(!v.has_pending());
    }
}
