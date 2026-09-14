//! paddock — a terminal UI for managing FreeToken.
//!
//! Download checkpoints from Hugging Face, convert them to FreeToken's FTW format, tune
//! every `ft serve` knob, launch and supervise the engine, retune its cache pools live,
//! and watch throughput, requests and logs — all from one screen on the machine the
//! engine runs on.

mod actions;
mod cache_pools;
mod compat;
mod config;
mod ft;
mod hub;
mod knobs;
mod models;
mod plan;
mod probe;
mod reuse;
mod runtime;
mod sampling;
mod templates;
mod ui;
mod util;
mod variants;
mod web;

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::config::{Config, Profiles};
use crate::ui::app::{App, Message, Tab};

#[derive(Parser, Debug)]
#[command(
    name = "paddock",
    version,
    about = "Manage FreeToken from a terminal or a browser",
    long_about = "paddock is a control panel for a FreeToken install: browse and download \
                  checkpoints, convert them to FTW, configure and supervise `ft serve`, resize \
                  cache pools on a live engine, and watch throughput, requests and logs.\n\n\
                  Run it bare for the terminal UI, or `paddock web` for the same thing in a \
                  browser."
)]
struct Cli {
    /// Server host paddock polls for telemetry.
    #[arg(long, value_name = "HOST", global = true)]
    host: Option<String>,

    /// Server port paddock polls for telemetry.
    #[arg(long, value_name = "PORT", global = true)]
    port: Option<u16>,

    /// Path to the `ft` executable, if it is not on PATH.
    #[arg(long, value_name = "PATH", global = true)]
    ft_binary: Option<std::path::PathBuf>,

    /// A virtualenv holding a FreeToken install.
    #[arg(long, value_name = "DIR", global = true)]
    venv: Option<std::path::PathBuf>,

    /// Extra directory to scan for checkpoints; repeatable.
    #[arg(long = "models", value_name = "DIR", global = true)]
    model_roots: Vec<std::path::PathBuf>,

    /// Color theme: auto, dark, light, or mono.
    #[arg(long, value_name = "NAME", global = true)]
    theme: Option<String>,

    /// Open on a specific tab.
    #[arg(long, value_name = "TAB", global = true)]
    tab: Option<String>,

    /// Write a default config file and exit.
    #[arg(long, global = true)]
    init_config: bool,

    /// Print the resolved configuration and the FreeToken install, then exit.
    #[arg(long, global = true)]
    doctor: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Serve the web interface instead of drawing a terminal UI.
    Web {
        /// Address to bind. Overrides `[web] listen`.
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,

        /// Bearer token every /api request must carry. Overrides `[web] token`.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },
}

fn main() -> Result<()> {
    install_panic_hook();
    let cli = Cli::parse();

    // Before anything reads config or state: an install made under the old name is moved
    // into place, so the rename does not read as a factory reset.
    config::migrate_legacy_dirs();

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

    let web = matches!(cli.command, Some(Command::Web { .. }));
    init_logging(web)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    match cli.command {
        Some(Command::Web { ref listen, ref token }) => {
            let listen = listen.clone().unwrap_or_else(|| config.web.listen.clone());
            let token = token.clone().or_else(|| config.web.token.clone());
            runtime.block_on(web::run(config, ft, listen, token))
        }
        None => runtime.block_on(run(cli, config, ft)),
    }
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

/// `--doctor`: everything paddock resolved, so a misconfigured install can be diagnosed
/// without entering the UI.
fn doctor(config: &Config, ft: Result<ft::Freetoken, String>) -> Result<()> {
    println!("paddock {}", env!("CARGO_PKG_VERSION"));
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
    match hub::locate_cli(config, ft.as_ref().ok().map(|f| f.program.as_path())) {
        Ok(path) => println!("hf CLI        {}", path.display()),
        Err(e) => println!(
            "hf CLI        NOT FOUND
  {e}"
        ),
    }
    println!();

    println!("model roots");
    let roots = config.library.effective_roots();
    let cache = config.library.hub_cache();
    for root in &roots {
        let exists = if root.is_dir() { "" } else { "  (missing)" };
        // Worth naming: it is added implicitly, so a user who never configured it should
        // still be told this is where their `hf`-downloaded weights are being read from.
        let note = if *root == cache { "  (Hugging Face cache)" } else { "" };
        println!("  {}{note}{exists}", root.display());
    }
    println!("  FTW builds  {}", config.library.ftw_dir().display());
    let found = models::scan(&roots, &config.library.ftw_dir());
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
            "  token        NOT FOUND (checked HF_TOKEN, HUGGING_FACE_HUB_TOKEN, hub.token \
                 in the config, and {})",
            config::hf_token_path().display()
        ),
    }

    let probe = probe::Probe::new();
    println!("GPU source     {}", probe.gpu_source);
    if let Some(e) = &probe.nvml_error {
        println!("  NVML         {e}");
    }
    let gpus = probe.gpus();
    for g in &gpus {
        println!(
            "  [{}] {}  {} / {}  {}",
            g.index,
            g.name,
            util::bytes(g.memory_used),
            util::bytes(g.memory_total),
            g.uuid
        );
    }

    // Without a bandwidth profile `--moe-strategy auto` can only ever resolve to offload
    // and `--moe-hybrid-max-fetch auto` falls back to a fixed cap of 1 — a speed ceiling
    // with no symptom, so it is worth naming here rather than leaving to be discovered.
    println!();
    match plan::bench_profile_status(gpus.first().map(|g| g.uuid.as_str())) {
        Some(path) => println!("bench profile  {}", path.display()),
        None => println!(
            "bench profile  NOT FOUND\n  \
             `ft bench bw --dtype all` measures CPU vs PCIe bandwidth for this card. \
             Until it has run,\n  --moe-strategy auto cannot select hybrid and \
             --moe-hybrid-max-fetch auto uses a fixed cap of 1."
        ),
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

/// Log paddock's own diagnostics.
///
/// The TUI writes to a file and nowhere else: stdout and stderr belong to the terminal
/// once it starts. The web daemon adds stderr at `info`, because it is a service and
/// whatever supervises it — journald, usually — is where its operator will look first.
fn init_logging(web: bool) -> Result<()> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let dir = config::log_dir();
    std::fs::create_dir_all(&dir).ok();
    let appender = tracing_appender::rolling::never(&dir, "paddock.log");
    let default = if web { "info" } else { "warn" };
    let filter = EnvFilter::try_from_env("PADDOCK_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(appender).with_ansi(false))
        .with(web.then(|| fmt::layer().with_writer(std::io::stderr)))
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
    // `ft` is None whenever locating FreeToken failed, which is the very case the
    // `ft_err` arm below reports — so this has to be a probe of an Option, not an
    // unwrap of one.
    let ft_version = app.ft.as_ref().and_then(crate::ft::locate::probe_version);
    app.set_ft_version(ft_version);

    if let Some(name) = &cli.tab {
        if let Some(t) = Tab::ALL.iter().find(|t| t.title().eq_ignore_ascii_case(name)) {
            app.tab = *t;
        }
    }
    if let Some(e) = ft_err {
        app.error(e);
    }
    app.request_scan();

    runtime::spawn_all(&app, tx.clone());

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
