//! ft-man — a terminal UI for managing FreeToken.
//!
//! Download checkpoints from Hugging Face, convert them to FreeToken's FTW format, tune
//! every `ft serve` knob, launch and supervise the engine, retune its cache pools live,
//! and watch throughput, requests and logs — all from one screen on the machine the
//! engine runs on.

mod config;
mod ft;
mod hub;
mod knobs;
mod models;
mod probe;
mod templates;
mod ui;
mod util;

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::config::{Config, Profiles};
use crate::ui::app::{App, Message, Tab, Telemetry};

#[derive(Parser, Debug)]
#[command(
    name = "ft-man",
    version,
    about = "Manage FreeToken from the terminal",
    long_about = "ft-man is a terminal UI for a FreeToken install: browse and download \
                  checkpoints, convert them to FTW, configure and supervise `ft serve`, resize \
                  cache pools on a live engine, and watch throughput, requests and logs."
)]
struct Cli {
    /// Server host ft-man polls for telemetry.
    #[arg(long, value_name = "HOST")]
    host: Option<String>,

    /// Server port ft-man polls for telemetry.
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,

    /// Path to the `ft` executable, if it is not on PATH.
    #[arg(long, value_name = "PATH")]
    ft_binary: Option<std::path::PathBuf>,

    /// A virtualenv holding a FreeToken install.
    #[arg(long, value_name = "DIR")]
    venv: Option<std::path::PathBuf>,

    /// Extra directory to scan for checkpoints; repeatable.
    #[arg(long = "models", value_name = "DIR")]
    model_roots: Vec<std::path::PathBuf>,

    /// Color theme: auto, dark, light, or mono.
    #[arg(long, value_name = "NAME")]
    theme: Option<String>,

    /// Open on a specific tab.
    #[arg(long, value_name = "TAB")]
    tab: Option<String>,

    /// Write a default config file and exit.
    #[arg(long)]
    init_config: bool,

    /// Print the resolved configuration and the FreeToken install, then exit.
    #[arg(long)]
    doctor: bool,
}

fn main() -> Result<()> {
    install_panic_hook();
    let cli = Cli::parse();

    if cli.init_config {
        let cfg = Config::default();
        cfg.save()?;
        println!("wrote {}", config::config_path().display());
        return Ok(());
    }

    let config = build_config(&cli)?;
    let ft = ft::locate::resolve(&config.freetoken);

    if cli.doctor {
        return doctor(&config, ft);
    }

    init_logging()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(run(cli, config, ft))
}

/// Merge CLI overrides over the file config. The file stays authoritative for anything
/// not given on the command line, and CLI overrides are deliberately not written back.
fn build_config(cli: &Cli) -> Result<Config> {
    let mut config = Config::load()?;
    if let Some(h) = &cli.host {
        config.server.host = h.clone();
    }
    if let Some(p) = cli.port {
        config.server.port = p;
    }
    if let Some(b) = &cli.ft_binary {
        config.freetoken.binary = Some(b.clone());
    }
    if let Some(v) = &cli.venv {
        config.freetoken.venv = Some(v.clone());
    }
    if let Some(t) = &cli.theme {
        config.ui.theme = t.clone();
    }
    for root in &cli.model_roots {
        if !config.library.roots.contains(root) {
            config.library.roots.push(root.clone());
        }
    }
    Ok(config)
}

/// `--doctor`: everything ft-man resolved, so a misconfigured install can be diagnosed
/// without entering the UI.
fn doctor(config: &Config, ft: Result<ft::Freetoken, String>) -> Result<()> {
    println!("ft-man {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("config file    {}", config::config_path().display());
    println!("profiles       {}", config::profiles_path().display());
    println!("state          {}", config::state_dir().display());
    println!("logs           {}", config::log_dir().display());
    println!();

    match &ft {
        Ok(found) => {
            println!("FreeToken CLI  {}", found.program.display());
            println!("  found via    {}", found.origin);
            match ft::locate::probe_version(found) {
                Some(v) => println!("  version      {v}"),
                None => println!("  version      (could not run --version)"),
            }
        }
        Err(e) => println!("FreeToken CLI  NOT FOUND\n  {e}"),
    }
    println!();

    println!("server         {}", config.server.base_url());
    println!("model roots");
    for root in &config.library.roots {
        let path = models::expand_tilde(root);
        let exists = if path.is_dir() { "" } else { "  (missing)" };
        println!("  {}{exists}", path.display());
    }
    let found = models::scan(&config.library.roots);
    println!("  {} checkpoint(s) found", found.len());
    for m in found.iter().take(20) {
        println!("    {:<5} {:>10}  {}", m.format.label(), util::bytes(m.size_bytes), m.name);
    }
    if found.len() > 20 {
        println!("    … and {} more", found.len() - 20);
    }
    println!();

    println!("Hugging Face   {}", config.hub.endpoint);
    match config.hub.resolve_token() {
        Some(t) => println!("  token        found via {}", t.source),
        None => println!(
            "  token        NOT FOUND (checked HF_TOKEN, HUGGING_FACE_HUB_TOKEN, hub.token in \
             the config, and ~/.cache/huggingface/token)"
        ),
    }

    let probe = probe::Probe::new();
    println!("GPU source     {}", probe.gpu_source);
    if let Some(e) = &probe.nvml_error {
        println!("  NVML         {e}");
    }
    for g in probe.gpus() {
        println!(
            "  [{}] {}  {} / {}  {}",
            g.index,
            g.name,
            util::bytes(g.memory_used),
            util::bytes(g.memory_total),
            g.uuid
        );
    }

    if let Some(state) = ft::proc::ServeState::load() {
        println!();
        let alive = if state.is_alive() { "running" } else { "stale (will be cleared)" };
        println!("recorded serve pid {} — {alive}", state.pid);
        println!("  model        {}", state.model);
        println!("  log          {}", state.log_path.display());
    }

    Ok(())
}

/// Log ft-man's own diagnostics to a file. Nothing goes to the terminal: stdout and
/// stderr belong to the TUI once it starts.
fn init_logging() -> Result<()> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let dir = config::log_dir();
    std::fs::create_dir_all(&dir).ok();
    let appender = tracing_appender::rolling::never(&dir, "ft-man.log");
    let filter = EnvFilter::try_from_env("FT_MAN_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(appender).with_ansi(false))
        .with(filter)
        .try_init()
        .ok();
    Ok(())
}

async fn run(cli: Cli, config: Config, ft: Result<ft::Freetoken, String>) -> Result<()> {
    let profiles = Profiles::load().unwrap_or_default();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    let (ft_ok, ft_err) = match ft {
        Ok(f) => (Some(f), None),
        Err(e) => (None, Some(e)),
    };
    let mut app = App::new(config, profiles, ft_ok, ft_err.clone(), tx.clone())?;

    if let Some(name) = &cli.tab {
        if let Some(t) = Tab::ALL.iter().find(|t| t.title().eq_ignore_ascii_case(name)) {
            app.tab = *t;
        }
    }
    if let Some(e) = ft_err {
        app.error(e);
    }
    app.request_scan();

    spawn_telemetry(&app, tx.clone());
    spawn_hardware(tx.clone());

    let mut terminal = ratatui::try_init().context("initializing the terminal")?;
    let result = event_loop(&mut terminal, &mut app, &mut rx).await;
    ratatui::try_restore().ok();

    // Leave the engine running: it is a long-lived service, the state file lets a later
    // run re-attach, and killing a loaded 200 GiB model because a TUI closed would be
    // the wrong default. Only a stop the user asked for stops it.
    app.engine.shutdown_blocking_if_requested();

    if let Err(e) = app.profiles.save() {
        eprintln!("warning: could not save profiles: {e:#}");
    }
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<Message>,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(app.config.ui.tick_ms));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    terminal.draw(|f| ui::draw::draw(f, app))?;

    loop {
        let mut dirty = false;

        tokio::select! {
            // Terminal input has priority: a keystroke should never queue behind a
            // burst of telemetry.
            biased;

            Some(event) = events.next() => {
                match event.context("reading terminal input")? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        ui::input::handle_key(app, key);
                        dirty = true;
                    }
                    Event::Resize(_, _) => dirty = true,
                    Event::Mouse(_) | Event::Paste(_) | Event::FocusGained
                    | Event::FocusLost | Event::Key(_) => {}
                }
            }

            Some(msg) = rx.recv() => {
                app.handle(msg);
                dirty = true;
                // Drain anything else already queued so a burst costs one redraw.
                while let Ok(next) = rx.try_recv() {
                    app.handle(next);
                }
            }

            _ = ticker.tick() => {
                app.tick();
                dirty = true;
            }
        }

        if app.should_quit {
            return Ok(());
        }
        if dirty {
            terminal.draw(|f| ui::draw::draw(f, app))?;
        }
    }
}

/// Poll the server's control plane on its own cadence.
///
/// `/health` is cheap and always answered, so it drives the loop; `/v1/stats` and
/// `/v1/cache/status` are only meaningful once the engine is serving, and skipping them
/// while it loads keeps a loading engine from being polled pointlessly for minutes.
fn spawn_telemetry(app: &App, tx: mpsc::UnboundedSender<Message>) {
    let mut client = app.client.clone();
    let mut endpoint = app.endpoint_tx.subscribe();
    let timeout = Duration::from_millis(app.config.server.timeout_ms);
    let period = Duration::from_millis(app.config.server.poll_ms.max(200));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut cursor = 0u64;
        loop {
            ticker.tick().await;
            // Follow a port change in the serve configuration rather than polling an
            // address nothing is listening on.
            if endpoint.has_changed().unwrap_or(false) {
                let url = endpoint.borrow_and_update().clone();
                if let Ok(next) = ft::Client::new(&url, timeout) {
                    client = next;
                    cursor = 0;
                }
            }
            let mut t = Telemetry { at: Some(std::time::Instant::now()), ..Default::default() };
            match client.health().await {
                Ok(health) => {
                    let ready = health.is_ready();
                    t.health = Some(health);
                    if ready {
                        t.stats = client.stats().await.ok();
                        t.cache = client.cache_status().await.ok();
                        if let Ok(page) = client.requests(cursor, 200).await {
                            if !page.entries.is_empty() || page.next_cursor != cursor {
                                cursor = page.next_cursor;
                                let _ = tx.send(Message::Requests {
                                    entries: page.entries,
                                    next_cursor: cursor,
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    // A connection refused while nothing is running is the normal state,
                    // not an error worth a toast — the Dashboard shows it as "not
                    // running" and that is enough.
                    t.error = Some(format!("{e:#}"));
                    cursor = 0;
                }
            }
            if tx.send(Message::Telemetry(Box::new(t))).is_err() {
                break;
            }
        }
    });
}

/// Sample GPUs and host memory on a blocking thread — NVML and `/proc` reads are
/// synchronous, and doing them on the runtime would stall other tasks.
fn spawn_hardware(tx: mpsc::UnboundedSender<Message>) {
    std::thread::spawn(move || {
        let mut probe = probe::Probe::new();
        loop {
            let gpus = probe.gpus();
            let host = probe.host();
            if tx.send(Message::Hardware { gpus, host }).is_err() {
                break;
            }
            std::thread::sleep(probe::SAMPLE_INTERVAL);
        }
    });
}

/// Restore the terminal even when a panic unwinds past the normal exit path, so a crash
/// never leaves the user in a raw-mode shell with no echo.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ratatui::try_restore();
        let _ = io::Write::flush(&mut io::stderr());
        default(info);
    }));
}
