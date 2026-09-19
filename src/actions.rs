//! Every operation the two front ends share.
//!
//! A key in the TUI and a request to the web server must do the same thing, so the thing
//! itself lives here once, as a function of `&mut App` plus an explicit target: a model
//! *path*, a job *id*, a profile *name*, a knob *key and value*. `ui::input` resolves
//! "the selected row" to a target and calls in; the web layer resolves a request body to
//! the same target and calls the same function.
//!
//! Refusals are reported twice over, deliberately. A refusal about the *state* of the
//! machine — an engine already running, a GPU already busy — pushes the toast the TUI has
//! always shown and also returns a [`Refusal`] carrying the HTTP status the web API
//! documents for it, so the terminal keeps behaving exactly as it did while a browser
//! learns why nothing happened.
//!
//! A refusal about the *request* does not toast. A rejected knob value, an empty profile
//! name, a path that is no longer in the library: each belongs against the field or the row
//! that produced it, where the reader is already looking, and a floating toast beside it is
//! the same problem rendered twice. The terminal has no inline slot, so `ui::input` raises
//! those toasts on its own side of the shared action.
//!
//! Every `Refusal` therefore carries [`Refusal::toasted`], and the web error envelope
//! reports it, so a client knows whether a toast is also on its way. See docs/web-api.md
//! sections 1.2 and 4.8.

use std::path::{Path, PathBuf};

use crate::ft::proc::{spawn_job, JobKind, JobSpec};
use crate::hub::{start_download, Hub};
use crate::knobs::{Kind, Knob};
use crate::models::Format;
use crate::ui::app::{rebuild_from_pending, App, HubFocus, Message, Pool, Tab, Telemetry};
use crate::ui::views;
use crate::ui::widgets::{Confirm, ConfirmAction, ToastKind};

// ---------------------------------------------------------------- outcomes

/// What an action did. The TUI ignores this; the web layer renders it as the reply body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// Finished.
    Ok,
    /// A task was spawned; the result arrives as a snapshot change and a toast.
    Started { job_id: Option<u64>, log_path: Option<PathBuf> },
    /// `app.confirm` is now set and nothing has happened yet.
    ConfirmPending,
}

impl Done {
    pub fn started() -> Self {
        Done::Started { job_id: None, log_path: None }
    }
}

/// Why an action declined to act, with the status the web API documents for that case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    pub message: String,
    /// Whether this refusal also went onto `app.toasts`, and so will arrive in the next
    /// snapshot. False for the field-shaped ones the caller renders inline.
    pub toasted: bool,
}

impl Refusal {
    /// A refusal the reader will see where they are already looking: under the field they
    /// submitted, or against the row they clicked. No toast.
    pub fn quiet(status: u16, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), toasted: false }
    }
}

pub type Outcome = Result<Done, Refusal>;

/// Decline, saying so in the terminal and on the wire at once.
fn refuse(app: &mut App, status: u16, kind: ToastKind, message: impl Into<String>) -> Refusal {
    let message = message.into();
    app.toast(message.clone(), kind);
    Refusal { status, message, toasted: true }
}

fn warn_off(app: &mut App, status: u16, message: impl Into<String>) -> Refusal {
    refuse(app, status, ToastKind::Warn, message)
}

fn error_off(app: &mut App, status: u16, message: impl Into<String>) -> Refusal {
    refuse(app, status, ToastKind::Error, message)
}

/// The FreeToken CLI, or the 503 that explains its absence.
fn freetoken(app: &mut App) -> Result<crate::ft::Freetoken, Refusal> {
    match app.ft.clone() {
        Some(ft) => Ok(ft),
        None => {
            let why =
                app.ft_error.clone().unwrap_or_else(|| "the FreeToken CLI was not found".into());
            Err(error_off(app, 503, why))
        }
    }
}

// ---------------------------------------------------------------- confirmations

/// Carry out a confirmed action.
pub fn run_action(app: &mut App, action: ConfirmAction) -> Done {
    match action {
        ConfirmAction::Quit => {
            app.should_quit = true;
            Done::Ok
        }
        ConfirmAction::UpdateFreetoken => start_update(app),
        ConfirmAction::StopEngine { force } => {
            app.engine.stop(force);
            app.info(if force { "force-stopping the engine" } else { "stopping the engine" });
            Done::Ok
        }
        ConfirmAction::DeleteModel(path) => {
            // Tens of thousands of `unlink`s against a 200 GiB checkpoint. Done inline it
            // held the TUI's event loop — and the web daemon's single `App` mutex, so every
            // connected browser — for as long as the filesystem took.
            app.info(format!("deleting {}…", path.display()));
            let tx = app.tx.clone();
            tokio::task::spawn_blocking(move || {
                let result = std::fs::remove_dir_all(&path).map_err(|e| e.to_string());
                let _ = tx.send(Message::ModelDeleted(path, result));
            });
            Done::started()
        }
        ConfirmAction::DeleteHfCacheModel { path, repo } => {
            // The hub cache belongs to `huggingface_hub`: deleting a snapshot directory
            // only removes symlinks into `blobs/`, leaving the actual weight data behind.
            // `hf cache delete` is the reference implementation that knows how to take a
            // repo apart — removing dangling blobs and cleaning up refs.
            let Some(cli) = app.hf_cli.clone() else {
                app.error("the hf CLI is not installed — install it from the Hub tab first");
                return Done::Ok;
            };
            app.info(format!("deleting {} from the HF cache…", path.display()));
            let tx = app.tx.clone();
            let repo_clone = repo.clone();
            tokio::task::spawn_blocking(move || {
                let result = std::process::Command::new(&cli)
                    .arg("cache")
                    .arg("delete")
                    .arg(&repo_clone)
                    .output()
                    .map_err(|e| format!("could not run {}: {e}", cli.display()))
                    .and_then(|out| {
                        if out.status.success() {
                            Ok(())
                        } else {
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            let stdout = String::from_utf8_lossy(&out.stdout);
                            let msg = if stderr.trim().is_empty() {
                                stdout.trim().to_string()
                            } else {
                                stderr.trim().to_string()
                            };
                            Err(if msg.is_empty() {
                                format!("hf cache delete exited with status {}", out.status)
                            } else {
                                msg
                            })
                        }
                    });
                let _ = tx.send(Message::HfCacheDeleted(path, result));
            });
            Done::started()
        }
        ConfirmAction::CancelJob(id) => {
            if let Some(j) = app.jobs.iter_mut().find(|j| j.id == id) {
                j.cancel();
            }
            Done::Ok
        }
        ConfirmAction::CancelDownload(id) => {
            if let Some(d) = app.downloads.iter_mut().find(|d| d.id == id) {
                d.cancel();
                app.warn("download canceled");
            }
            Done::Ok
        }
        ConfirmAction::DeleteProfile(name) => {
            if app.profiles.remove(&name) {
                if app.profiles.last_used.as_deref() == Some(name.as_str()) {
                    app.profiles.last_used = None;
                }
                save_profiles(app);
                app.success(format!("deleted profile '{name}'"));
            }
            Done::Ok
        }
        ConfirmAction::InstallHfCli => {
            app.hf_installing = true;
            app.info("installing the hf CLI…");
            let tx = app.tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(Message::HfInstalled(crate::hub::install_cli().await));
            });
            Done::started()
        }
        ConfirmAction::ApplyCacheRebuild => apply_cache_rebuild(app),
        ConfirmAction::ApplyTemplate { template, model } => {
            write_template(app, &template, &model);
            Done::Ok
        }
        ConfirmAction::RevertTemplate(model) => {
            revert_template(app, &model);
            Done::Ok
        }
        ConfirmAction::ApplySampling { model, sampling } => {
            write_sampling(app, &model, &sampling);
            Done::Ok
        }
        ConfirmAction::RevertSampling(model) => {
            revert_sampling(app, &model);
            Done::Ok
        }
        ConfirmAction::ConvertAnyway(source) => start_conversion(app, &source),
        ConfirmAction::ReconvertModel(source) => {
            let out = ftw_out(app, &source);
            app.info(format!("removing the incomplete {}…", out.display()));
            let tx = app.tx.clone();
            tokio::task::spawn_blocking(move || {
                let result = std::fs::remove_dir_all(&out).map_err(|e| e.to_string());
                let _ = tx.send(Message::LeftoversRemoved(source, result));
            });
            Done::started()
        }
        ConfirmAction::DeleteTemplate(name) => {
            match crate::templates::remove(&name) {
                Ok(()) => {
                    app.reload_templates();
                    app.success(format!("deleted template '{name}'"));
                }
                Err(e) => app.error(format!("could not delete it: {e:#}")),
            }
            Done::Ok
        }
    }
}

/// Raise a confirmation, or act at once when the user has turned confirmations off.
pub fn ask(app: &mut App, confirm: Confirm) -> Done {
    if app.config.ui.confirm_destructive {
        app.confirm = Some(confirm);
        Done::ConfirmPending
    } else {
        run_action(app, confirm.action)
    }
}

/// Answer a pending confirmation by identity-free accept/dismiss, as `POST /api/confirm`
/// and the modal's `y`/`n` both do.
pub fn answer_confirm(app: &mut App, accept: bool) -> Outcome {
    let Some(confirm) = app.confirm.take() else {
        return Err(warn_off(app, 409, "no confirmation is pending"));
    };
    if !accept {
        return Ok(Done::Ok);
    }
    Ok(run_action(app, confirm.action))
}

// ---------------------------------------------------------------- engine

/// Why a start would be refused, with everything the two callers need to report it.
struct StartRefusal {
    status: u16,
    kind: ToastKind,
    message: String,
    /// Whether the refusal is something to fix on the Serve tab.
    focus_serve: bool,
}

/// The single predicate behind `POST /api/engine/start` and `engine.start_blocked`.
///
/// One function, because the snapshot advertises in advance what the route will do, and a
/// button that says "start" against a daemon that would answer 409 is worse than no button.
/// It reads state only — the cross-process check it depends on is refreshed by
/// [`crate::ft::Engine::poll`] once a second, and forced by the route itself.
fn start_refusal(app: &App) -> Option<StartRefusal> {
    let warn = |message: String| {
        Some(StartRefusal { status: 409, kind: ToastKind::Warn, message, focus_serve: false })
    };
    if app.engine.is_live() {
        return warn("an engine is already running; stop it first".into());
    }
    // Another paddock on this machine — a terminal beside the daemon, or a second daemon —
    // may have started one since the last tick. The state file is the handoff; starting a
    // second engine on the same GPU and port is how both end up broken.
    if let Some(state) = app.engine.foreign() {
        return warn(format!(
            "an engine started elsewhere is already running (pid {}, port {}); paddock has              attached to it",
            state.pid, state.port
        ));
    }
    if let Some(job) = app.jobs.iter().find(|j| j.is_running()) {
        let kind = job.kind.label();
        return warn(format!("a {kind} job is using the GPU; wait for it or cancel it first"));
    }
    if app.ft.is_none() {
        return Some(StartRefusal {
            status: 503,
            kind: ToastKind::Error,
            message: app
                .ft_error
                .clone()
                .unwrap_or_else(|| "the FreeToken CLI was not found".into()),
            focus_serve: false,
        });
    }
    let errors = app.serve.validate();
    if let Some((key, msg)) = errors.first() {
        let flag = crate::knobs::knob(key).map(|k| k.flag).unwrap_or(key);
        return Some(StartRefusal {
            status: 409,
            kind: ToastKind::Error,
            message: format!("{flag}: {msg}"),
            focus_serve: true,
        });
    }
    None
}

/// Why `POST /api/engine/start` would be refused right now; `None` when it would try.
pub fn start_blocked(app: &App) -> Option<String> {
    start_refusal(app).map(|r| r.message)
}

pub fn start_engine(app: &mut App) -> Outcome {
    // Re-read the state file before deciding, rather than trusting the last tick: a second
    // engine started in the last second is exactly the race this closes. Adopting it here
    // also means the refusal is true by the time it is read.
    app.engine.refresh_foreign();
    if let Some(r) = start_refusal(app) {
        if r.focus_serve {
            app.tab = Tab::Serve;
        }
        return Err(refuse(app, r.status, r.kind, r.message));
    }
    let ft = freetoken(app)?;

    let model = app.serve.get("model").unwrap_or_default().to_string();
    let port = app.serve.get("port").and_then(|p| p.parse().ok()).unwrap_or(app.config.server.port);
    let args = app.serve.to_args();

    match app.engine.start(&ft, args, &app.config.freetoken.env, model.clone(), port) {
        Ok(path) => {
            app.telemetry = Telemetry::default();
            app.wake_poll();
            app.series = crate::ui::app::Series::default();
            app.requests_view.clear();
            app.requests_view.cursor = 0;
            app.tab = Tab::Logs;
            app.logs_view.follow = true;
            app.logs_view.scroll = 0;
            app.info(format!("starting {model}; logging to {}", path.display()));
            Ok(Done::Started { job_id: None, log_path: Some(path) })
        }
        Err(e) => Err(error_off(app, 500, format!("could not start the engine: {e:#}"))),
    }
}

pub fn request_stop(app: &mut App, force: bool) -> Outcome {
    if !app.engine.is_live() {
        return Err(warn_off(app, 409, "no engine is running"));
    }
    let adopted = app.engine.state == crate::ft::EngineState::Adopted;
    let model = app.current_model().unwrap_or_else(|| "the model".into());
    Ok(ask(
        app,
        Confirm::new(
            if force { "Force-stop the engine" } else { "Stop the engine" },
            vec![
                format!("Stop the engine serving {model}?"),
                String::new(),
                if force {
                    "SIGKILL does not let the engine drain in-flight requests or release VRAM \
                     cleanly. Use it only when a normal stop has already failed."
                        .into()
                } else if adopted {
                    "This engine was started by an earlier paddock run and re-attached to. \
                     In-flight requests are aborted and the weights are unloaded."
                        .to_string()
                } else {
                    "In-flight requests are aborted and the weights are unloaded.".to_string()
                },
            ],
            ConfirmAction::StopEngine { force },
            force,
        ),
    ))
}

pub fn smoke_test(app: &mut App) -> Outcome {
    if !app.server_reachable() {
        return Err(warn_off(app, 409, "the server is not answering"));
    }
    let client = app.client.clone();
    let tx = app.tx.clone();
    app.info("running a /generate smoke test…");
    tokio::spawn(async move {
        let res =
            client.generate("The capital of France is", 16).await.map_err(|e| format!("{e:#}"));
        let _ = tx.send(Message::SmokeTest(res));
    });
    Ok(Done::started())
}

// ---------------------------------------------------------------- models

pub fn rescan(app: &mut App) -> Outcome {
    app.request_scan();
    Ok(Done::started())
}

/// Load a checkpoint into the Serve configuration, optionally starting it.
pub fn use_model(app: &mut App, path: &Path, and_serve: bool) -> Outcome {
    let Some(model) = app.models.iter().find(|m| m.path == path) else {
        return Err(not_found_model(app, path));
    };
    if model.is_partial() {
        let name = model.name.clone();
        return Err(warn_off(
            app,
            409,
            format!("{name} is an incomplete conversion and cannot be served; delete it with D"),
        ));
    }
    // An FTW build loads meaningfully faster, so prefer it when one exists.
    let (target, note) = match &model.converted_to {
        Some(ftw) => (ftw.clone(), Some(format!("using the FTW build at {}", ftw.display()))),
        None => (model.path.clone(), None),
    };
    let name = model.name.clone();
    // Explicit, never inferred. See `Model::served_name`.
    let served = model.served_name();
    // Only when paddock is the one who put it there. A name typed by hand, or loaded from a
    // profile, is a decision about the API this engine publishes — clients send it in
    // request bodies — and picking a different model must not silently rewrite it. The
    // giveaway is that the current value is the previous model's derived name.
    let derive = match app.serve.get("served_model_name").map(str::trim).filter(|n| !n.is_empty()) {
        None => true,
        Some(current) => {
            let current = current.to_string();
            app.models.iter().any(|m| m.served_name() == current)
        }
    };
    app.serve.set("model", target.display().to_string());
    if derive {
        app.serve.set("served_model_name", served);
    }
    if let Some(n) = note {
        app.info(n);
    }
    if and_serve {
        start_engine(app)
    } else {
        app.tab = Tab::Serve;
        app.success(format!("{name} loaded into the Serve configuration"));
        Ok(Done::Ok)
    }
}

pub fn convert_model(app: &mut App, path: &Path) -> Outcome {
    let Some(model) = app.models.iter().find(|m| m.path == path) else {
        return Err(not_found_model(app, path));
    };
    if model.format != Format::Hf {
        let (name, label) = (model.name.clone(), model.format.label());
        return Err(warn_off(
            app,
            409,
            format!(
                "{name} is already in {label} format; conversion only applies to Hugging Face \
                 checkpoints"
            ),
        ));
    }
    if let Some(reason) = app.gpu_busy_reason() {
        return Err(warn_off(app, 409, format!("cannot convert: {reason}")));
    }
    if app.convert_checking.is_some() {
        return Err(warn_off(app, 409, "a checkpoint check is already running"));
    }

    let source = model.path.clone();
    let name = model.name.clone();
    let out = crate::models::ftw_output_path(
        &source,
        model.repo.as_deref(),
        model.variant.as_deref(),
        &app.config.library.ftw_dir(),
    );

    if out.exists() {
        // A finished build is a real artifact; the leftovers of a failed run are not, and
        // refusing to touch either left a retry with nowhere to go and tens of gigabytes
        // stranded in a directory the Models list did not even show.
        let leftovers = crate::models::inspect(&out).is_some_and(|m| m.is_partial());
        if !leftovers {
            return Err(warn_off(
                app,
                409,
                format!(
                    "{} already exists; delete it from the Models tab to reconvert",
                    out.display()
                ),
            ));
        }
        // Same reason as `delete_model`: sizing a half-written FTW build is a full tree
        // walk, and the confirmation cannot be worded without the number.
        let tx = app.tx.clone();
        tokio::task::spawn_blocking(move || {
            let size = crate::models::dir_size(&out);
            let confirm = Confirm::new(
                "Retry conversion",
                vec![
                    format!("{name} has leftovers from a conversion that failed."),
                    String::new(),
                    out.display().to_string(),
                    format!(
                        "Delete those {} and convert again? They hold no index and \
                         cannot be served.",
                        crate::util::bytes(size)
                    ),
                ],
                ConfirmAction::ReconvertModel(source),
                true,
            );
            let _ = tx.send(Message::AskConfirm(Box::new(confirm)));
        });
        return Ok(Done::started());
    }

    Ok(begin_conversion(app, &source))
}

pub fn delete_model(app: &mut App, path: &Path) -> Outcome {
    let Some(model) = app.models.iter().find(|m| m.path == path) else {
        return Err(not_found_model(app, path));
    };
    let path = model.path.clone();
    let name = model.name.clone();
    // The hub cache belongs to `huggingface_hub`: a snapshot directory is symlinks into
    // `blobs/`, and deleting it frees the links, leaves the blobs, and breaks `refs/`.
    // `hf cache delete` is the only thing that knows how to take a repo apart properly.
    if crate::templates::is_hub_cache_path(&path) {
        let repo = model.repo.clone().unwrap_or_else(|| name.clone());
        // For HF cache models, the confirmation is simpler — no dir_size walk needed.
        let confirm = Confirm::new(
            "Delete from HF cache",
            vec![
                format!("Permanently delete {name} from the Hugging Face cache?"),
                String::new(),
                path.display().to_string(),
                "This runs `hf cache delete` to remove the snapshot and any dangling blobs. \
                 Cannot be undone."
                    .into(),
            ],
            ConfirmAction::DeleteHfCacheModel { path: path.clone(), repo },
            true,
        );
        let tx = app.tx.clone();
        let _ = tx.send(Message::AskConfirm(Box::new(confirm)));
        return Ok(Done::started());
    }
    // `dir_size` walks the whole tree. On the daemon that walk happens under the one mutex
    // every browser shares, so it goes to the blocking pool and the confirmation is raised
    // when the number is in hand.
    app.info(format!("measuring {}…", path.display()));
    let tx = app.tx.clone();
    tokio::task::spawn_blocking(move || {
        let size = crate::models::dir_size(&path);
        let confirm = Confirm::new(
            "Delete checkpoint",
            vec![
                format!("Permanently delete {name}?"),
                String::new(),
                path.display().to_string(),
                format!("This frees {} and cannot be undone.", crate::util::bytes(size)),
            ],
            ConfirmAction::DeleteModel(path),
            true,
        );
        let _ = tx.send(Message::AskConfirm(Box::new(confirm)));
    });
    Ok(Done::started())
}

/// A model named by a path that is no longer in the library. No toast: the TUI resolves
/// its target from the list it just drew and cannot reach this, so the only reader is a
/// browser acting on a stale list.
fn not_found_model(_app: &mut App, _path: &Path) -> Refusal {
    Refusal::quiet(404, "that model is no longer in the library")
}

/// Continue a retried conversion once its leftovers have actually been removed.
///
/// Split out because the removal now runs on the blocking pool: `Message::LeftoversRemoved`
/// is what resumes the sequence, and it has nowhere else to call into.
pub fn start_after_leftovers(app: &mut App, source: &Path) {
    begin_conversion(app, source);
}

/// Ask FreeToken what it makes of the checkpoint, then convert.
///
/// The check is cheap and the job is not: a conversion that cannot read the experts
/// still spends minutes writing most of the model to disk before it finds out.
fn begin_conversion(app: &mut App, source: &Path) -> Done {
    if !app.config.convert.preflight {
        return start_conversion(app, source);
    }
    let Some(ft) = app.ft.clone() else {
        app.error("the FreeToken CLI was not found");
        return Done::Ok;
    };
    let moe_backend = convert_moe_backend(app);
    let Some(argv) = crate::ft::preflight::convert_command(&ft, source, moe_backend) else {
        // No interpreter to check with is not a reason to refuse the conversion.
        return start_conversion(app, source);
    };

    app.convert_checking = Some(source.to_path_buf());
    app.info("checking that FreeToken can read this checkpoint…");

    let source = source.to_path_buf();
    let env = app.config.freetoken.env.clone();
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let outcome = crate::ft::preflight::run(argv, &env).await;
        let _ = tx.send(Message::ConvertPreflight(source, outcome));
    });
    Done::started()
}

/// Act on a conversion preflight: start silently when it is clean, explain and ask when
/// it is not.
pub fn on_convert_preflight(app: &mut App, source: PathBuf, outcome: crate::ft::Preflight) {
    use crate::ft::Preflight;
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.display().to_string());

    match outcome {
        Preflight::Ok(detail) => {
            app.info(detail);
            start_conversion(app, &source);
        }
        Preflight::Warn(detail) | Preflight::Fail(detail) => {
            ask(
                app,
                Confirm::new(
                    "Convert anyway?",
                    vec![
                        format!("FreeToken may not be able to convert {name}."),
                        String::new(),
                        detail,
                        String::new(),
                        "Converting anyway will run for several minutes and write most of \
                         the model to disk before it can fail."
                            .into(),
                    ],
                    ConfirmAction::ConvertAnyway(source),
                    false,
                ),
            );
        }
    }
}

/// `offload` packs experts into banks, which is what every offload-family backend wants;
/// `triton` keeps them dense for resident serving. Matching the Serve configuration's MoE
/// backend keeps the output usable by the configuration that asked for it.
fn convert_moe_backend(app: &App) -> &'static str {
    match app.serve.get("moe_strategy") {
        Some("fused") => "triton",
        _ => "offload",
    }
}

/// Where the FTW build of `source` goes, resolving the checkpoint's repo id out of the
/// library so one that came from the Hugging Face cache gets an org-qualified name.
pub fn ftw_out(app: &App, source: &Path) -> PathBuf {
    let found = app.models.iter().find(|m| m.path == source);
    let repo = found.and_then(|m| m.repo.clone());
    let variant = found.and_then(|m| m.variant.clone());
    crate::models::ftw_output_path(
        source,
        repo.as_deref(),
        variant.as_deref(),
        &app.config.library.ftw_dir(),
    )
}

/// Spawn `ft checkpoint` for a checkpoint that has been cleared to convert.
fn start_conversion(app: &mut App, source: &Path) -> Done {
    let Some(ft) = app.ft.clone() else {
        app.error("the FreeToken CLI was not found");
        return Done::Ok;
    };
    let out = ftw_out(app, source);
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.display().to_string());

    let moe_backend = convert_moe_backend(app);

    let mut args = vec![
        "--model".to_string(),
        source.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
        "--moe-backend".to_string(),
        moe_backend.to_string(),
    ];
    // The expert banks are physically packed for the kernel that will read them, and the
    // FTW records which one in `quant_format`, so a conversion that ignores the serve
    // configuration's --quant-backend bakes in a layout the serve will not ask for.
    if let Some(quant) = app.serve.get("quant_backend") {
        args.push("--quant-backend".into());
        args.push(quant.to_string());
    }
    if let Some(gpu) = app.serve.get("gpu") {
        args.push("--gpu".into());
        args.push(gpu.to_string());
    }

    match spawn_job(
        &ft,
        JobSpec {
            kind: JobKind::Convert,
            subcommand: &["checkpoint"],
            args,
            env: &app.config.freetoken.env,
            title: format!("{name} → FTW"),
            log_capacity: app.config.ui.log_capacity,
        },
        app.job_tx.clone(),
    ) {
        Ok(job) => {
            let id = job.id;
            app.jobs.push(job);
            app.jobs_view.sel.last(views::jobs::rows(app).len());
            app.tab = Tab::Jobs;
            app.info(format!("converting {name} to {}", out.display()));
            Done::Started { job_id: Some(id), log_path: None }
        }
        Err(e) => {
            app.error(format!("could not start the conversion: {e:#}"));
            Done::Ok
        }
    }
}

// ---------------------------------------------------------------- hub

pub fn hub_client(app: &App) -> Result<Hub, String> {
    Hub::new(&app.config.hub.endpoint, app.hub_token.as_ref().map(|t| t.value.clone()))
        .map_err(|e| format!("{e:#}"))
}

fn hub_or_refuse(app: &mut App) -> Result<Hub, Refusal> {
    match hub_client(app) {
        Ok(h) => Ok(h),
        Err(e) => Err(error_off(app, 503, e)),
    }
}

pub fn search(app: &mut App, query: &str) -> Outcome {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Err(Refusal::quiet(400, "a search needs a query"));
    }
    app.hub_view.query.set(query.clone());
    let hub = hub_or_refuse(app)?;
    app.hub_view.searching = true;
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let res = hub.search(&query, 50).await.map_err(|e| format!("{e:#}"));
        let _ = tx.send(Message::HubSearch(res));
    });
    Ok(Done::started())
}

/// Update the FreeToken this machine runs: pull the checkout, reinstall it into its venv.
///
/// The same two commands an operator would type, with nothing inherited from a shell that
/// may not be there — a systemd unit's PATH is not a login shell's, and this runs under
/// both. Every program is resolved to a path first and refused by name if missing, rather
/// than failing halfway through with `uv: not found` and a half-updated tree.
///
/// Asks before doing it. This rewrites the files the engine loads from.
pub fn update_freetoken(app: &mut App) -> Outcome {
    let plan = update_plan(app)?;
    Ok(ask(
        app,
        crate::ui::widgets::Confirm::new(
            "Update FreeToken",
            vec![
                format!("{}", plan.dir.display()),
                String::new(),
                format!("git pull --ff-only  ({} commit(s) behind origin)", plan.behind),
                "uv pip install -e \".[accel]\"".to_string(),
                String::new(),
                "the engine must be started again afterwards".into(),
            ],
            ConfirmAction::UpdateFreetoken,
            false,
        ),
    ))
}

/// What an update would run, and every reason it would not.
struct UpdatePlan {
    dir: std::path::PathBuf,
    venv: std::path::PathBuf,
    git: std::path::PathBuf,
    uv: std::path::PathBuf,
    behind: usize,
}

fn update_plan(app: &mut App) -> Result<UpdatePlan, Refusal> {
    let Some(dir) = crate::ft::checkout::locate(&app.config.freetoken, app.ft.as_ref()) else {
        return Err(warn_off(app, 503, "no FreeToken checkout to update"));
    };
    // An editable install is the files on disk: pulling under a running engine swaps the
    // modules it imports lazily and the kernels it has mapped, which is a crash with a
    // confusing cause rather than an update.
    if app.engine.is_live() {
        return Err(warn_off(
            app,
            409,
            "stop the engine first; an update rewrites the files it is running from",
        ));
    }
    let checkout = app.ft_checkout.clone();
    if checkout.as_ref().is_some_and(|c| c.dirty) {
        return Err(warn_off(
            app,
            409,
            "the checkout has local changes; commit or discard them first",
        ));
    }
    let behind = checkout.as_ref().map(|c| c.origin_behind).unwrap_or(0);
    if behind == 0 {
        return Err(warn_off(app, 409, "already at origin; nothing to update"));
    }
    // What is true about the world first, what is missing from the setup second: being told
    // the venv is unconfigured is no use to someone who has nothing to pull anyway.
    let Some(venv) = app.config.freetoken.venv.clone() else {
        return Err(warn_off(
            app,
            503,
            "freetoken.venv is not set; paddock does not know which venv to install into",
        ));
    };
    let Some(git) = which("git") else {
        return Err(warn_off(app, 503, "git is not on PATH"));
    };
    // No pip fallback: a uv-created venv does not ship one, so the absence of uv is the end
    // of the road and should say so rather than be discovered mid-run.
    let Some(uv) = which("uv").or_else(|| {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
        [home.join(".local/bin/uv"), home.join(".cargo/bin/uv")].into_iter().find(|p| p.is_file())
    }) else {
        return Err(warn_off(app, 503, "uv was not found on PATH or in ~/.local/bin; FreeToken's venv has no pip to fall back on"));
    };
    Ok(UpdatePlan { dir, venv, git, uv, behind })
}

/// The first executable of that name on PATH.
fn which(program: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).map(|dir| dir.join(program)).find(|p| p.is_file())
    })
}

fn start_update(app: &mut App) -> Done {
    let plan = match update_plan(app) {
        Ok(p) => p,
        // The preconditions are re-checked on the way in: a confirmation can sit on screen
        // while the engine is started in another window.
        Err(_) => return Done::Ok,
    };
    // One shell, because the second command must not run if the first fails, but every
    // program is passed in by path: nothing here is looked up in an inherited PATH.
    const SCRIPT: &str =
        "set -e; \"$1\" pull --ff-only; VIRTUAL_ENV=\"$2\" \"$3\" pip install -e \".[accel]\"";
    let run = crate::ft::proc::Run {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            SCRIPT.into(),
            "paddock-update".into(),
            plan.git.display().to_string(),
            plan.venv.display().to_string(),
            plan.uv.display().to_string(),
        ],
        cwd: Some(plan.dir.clone()),
        display: format!(
            "git pull --ff-only && uv pip install -e \".[accel]\"  # in {}",
            plan.dir.display()
        ),
        kind: JobKind::Update,
        title: format!("update FreeToken ({} commits)", plan.behind),
        log_capacity: app.config.ui.log_capacity,
    };
    match crate::ft::proc::spawn_run(run, &app.config.freetoken.env, app.job_tx.clone()) {
        Ok(job) => {
            let id = job.id;
            app.jobs.push(job);
            app.jobs_view.sel.last(views::jobs::rows(app).len());
            app.tab = Tab::Jobs;
            // The summary is the engine's account of the commits this checkout is behind
            // origin. The update consumes exactly those, so drop it as the pull begins
            // rather than leave a summary of commits that are no longer behind.
            app.origin_summary = None;
            app.info("updating FreeToken");
            Done::Started { job_id: Some(id), log_path: None }
        }
        Err(e) => {
            app.error(format!("could not start the update: {e:#}"));
            Done::Ok
        }
    }
}

/// Ask the running engine what origin changed.
///
/// The one place paddock uses the model it supervises for something other than proving the
/// server answers. The material is the commit log, the diffstat and as much of the patch
/// as the budget allows; the log and the stat always fit, and only the patch is cut,
/// because they are the parts that describe a change rather than spell it out.
///
/// Every refusal here is a different missing precondition, and each says which: there is
/// no checkout to read, it is already current, or nothing is loaded to ask.
pub fn summarize_origin(app: &mut App) -> Outcome {
    let Some(dir) = crate::ft::checkout::locate(&app.config.freetoken, app.ft.as_ref()) else {
        return Err(warn_off(app, 503, "no FreeToken checkout to compare against"));
    };
    if !app.server_reachable() {
        return Err(warn_off(app, 409, "no engine is answering; start one to ask it"));
    }
    let Some(model) = app.current_model() else {
        return Err(warn_off(app, 409, "no model is loaded to ask"));
    };
    let Some(changes) = crate::ft::checkout::origin_changes(&dir, PATCH_BUDGET) else {
        return Err(warn_off(app, 409, "this checkout is already at origin"));
    };

    let prompt = format!(
        "Below are the commits {range} that the local FreeToken checkout is missing.\n\n\
         COMMIT LOG (oldest first)\n{log}\n\n\
         DIFFSTAT\n{stat}\n\n\
         PATCH{cut}\n{patch}\n",
        range = changes.range,
        log = changes.log,
        stat = changes.stat,
        cut =
            if changes.truncated { " (truncated — the diffstat above is complete)" } else { "" },
        patch = changes.patch,
    );
    app.origin_summary = Some(crate::ui::app::OriginSummary {
        range: changes.range,
        commits: changes.commits,
        model: model.clone(),
        pending: true,
        truncated: changes.truncated,
        text: None,
        error: None,
    });

    let client = app.client.clone();
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let res = client
            .chat(
                &model,
                SUMMARY_SYSTEM,
                &prompt,
                SUMMARY_MAX_TOKENS,
                Some("low"),
                std::time::Duration::from_secs(600),
            )
            .await
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(Message::OriginSummary(Box::new(res)));
    });
    Ok(Done::started())
}

/// Output budget for a summary, thinking included.
///
/// A reasoning model spends this before it answers, and the graded ladder the checkpoint
/// advertises has no "off" gear on every family — so the request asks for the low gear and
/// still leaves room for a model that thinks anyway. Six bullet points need a few hundred
/// tokens; the rest of this is headroom for the reasoning that precedes them, because
/// running out mid-thought produces no answer at all rather than a short one.
const SUMMARY_MAX_TOKENS: u32 = 4096;

/// Bytes of patch text sent with a summary request. Large enough for an ordinary origin
/// week, small enough that prefill is seconds rather than minutes on a loaded engine.
const PATCH_BUDGET: usize = 64 * 1024;

/// What the engine is asked to be. Deliberately narrow: the reader is an operator deciding
/// whether to pull, not a reviewer, and the failure mode of a model shown a diff is
/// confident invention about code it cannot see.
const SUMMARY_SYSTEM: &str = "You summarize changes to FreeToken, a local LLM inference \
    engine, for the operator of a machine running it. Answer in at most six bullet points, \
    plainest first. Say what changed and what it means for someone running the engine — new \
    or renamed CLI flags, changed defaults, new model support, anything that alters how a \
    server should be started or what it will accept. If the patch was truncated, say which \
    parts of your answer rest on the diffstat alone. Do not speculate about code you were \
    not shown, and do not repeat the commit subjects back as a list.";

/// List a repo's files and judge whether FreeToken could run it.
pub fn open_repo(app: &mut App, repo_id: &str, revision: Option<&str>) -> Outcome {
    let repo_id = repo_id.to_string();
    let gated = app.hub_view.results.iter().find(|r| r.id == repo_id).is_some_and(|r| r.is_gated());
    let hub = hub_or_refuse(app)?;
    if gated && !hub.has_token() {
        app.warn(format!(
            "{repo_id} is gated — set HF_TOKEN or hub.token and accept its terms on the Hub first"
        ));
    }
    if let Some(rev) = revision {
        app.hub_view.revision = rev.to_string();
    }
    let revision = app.hub_view.revision.clone();
    app.hub_view.loading_info = true;
    app.hub_view.focus = HubFocus::Files;
    app.hub_view.compat = None;
    app.hub_view.compat_error = None;
    let tx = app.tx.clone();
    let repo_for_info = repo_id.clone();
    let rev_for_info = revision.clone();
    let hub_for_info = hub.clone();
    tokio::spawn(async move {
        let res =
            hub_for_info.info(&repo_for_info, &rev_for_info).await.map_err(|e| format!("{e:#}"));
        let _ = tx.send(Message::HubInfo(Box::new(res)));
    });

    check_compatibility(app, hub, repo_id, revision);
    Ok(Done::started())
}

/// Fetch just the repo's `config.json` and judge whether FreeToken could run it.
///
/// One small request against a download measured in tens of gigabytes: the whole point
/// is to answer "can this even work here?" before committing to the transfer.
fn check_compatibility(app: &mut App, hub: Hub, repo: String, revision: String) {
    app.hub_view.checking_compat = true;
    app.hub_view.compat_config = None;
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let res = async {
            let raw = hub
                .fetch_text(&repo, &revision, "config.json")
                .await
                .map_err(|e| format!("{e:#}"))?;
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|e| format!("config.json is not JSON: {e}"))
        }
        .await;
        // The config, not a verdict. Judging it also needs the file listing and
        // FreeToken's registry, and this request races both of them, so the arithmetic
        // happens in `App::reprice_compat` where all three can be seen at once.
        let _ = tx.send(Message::Compatibility(Box::new(res)));
    });
}

/// Apply a quantization to the file selection.
pub fn choose_variant(app: &mut App, label: &str) -> Outcome {
    let Some(label) =
        app.hub_view.layout.weights().find(|v| v.label == label).map(|v| v.label.clone())
    else {
        return Err(warn_off(app, 409, format!("this repo has no '{label}' quantization")));
    };
    let wanted = app.hub_view.layout.files_for(&label);
    for f in &mut app.hub_view.files {
        f.wanted = wanted.contains(&f.path);
    }
    let (total, count) = app.hub_view.selected();
    app.hub_view.variant = Some(label.clone());
    app.hub_view.custom_selection = false;
    // A quantization is a weight size, and a weight size is how much of the card is left
    // for the cache. The context the verdict promises moves with this choice.
    app.reprice_compat();
    app.info(format!("{label}: {count} file(s), {}", crate::util::bytes(total)));
    Ok(Done::Ok)
}

/// Toggle one file by path. `wanted` of `None` flips it, which is what `Space` does.
pub fn toggle_file(app: &mut App, path: &str, wanted: Option<bool>) -> Result<bool, Refusal> {
    let Some(f) = app.hub_view.files.iter_mut().find(|f| f.path == path) else {
        return Err(Refusal::quiet(404, format!("no file named {path} in this repo's listing")));
    };
    f.wanted = wanted.unwrap_or(!f.wanted);
    let now = f.wanted;
    // The selection no longer is a quantization, so nothing downstream may go on
    // claiming it is one.
    app.hub_view.custom_selection = true;
    app.reprice_compat();
    Ok(now)
}

/// Select every file, or none. Returns how many are now wanted.
pub fn select_files(app: &mut App, all: bool) -> usize {
    app.hub_view.files.iter_mut().for_each(|f| f.wanted = all);
    app.hub_view.custom_selection = true;
    app.reprice_compat();
    app.hub_view.files.iter().filter(|f| f.wanted).count()
}

/// Start a repo download.
///
/// Resolution order matches the API: explicit `files`, else a `variant` expanded through
/// the layout, else whatever is currently wanted — which is the TUI's own behavior.
pub fn download(app: &mut App, variant: Option<&str>, files: Option<&[String]>) -> Outcome {
    let Some(info) = app.hub_view.info.as_ref() else {
        return Err(warn_off(app, 409, "select a repo and press Enter to list its files first"));
    };
    let repo = info.id.clone();

    let selected: Vec<crate::hub::RepoFile> = match (files, variant) {
        (Some(paths), _) if !paths.is_empty() => {
            let mut out = Vec::with_capacity(paths.len());
            for p in paths {
                match app.hub_view.files.iter().find(|f| f.path == *p) {
                    Some(f) => out.push(f.clone()),
                    None => {
                        return Err(Refusal::quiet(
                            400,
                            format!("{p} is not in this repo's listing"),
                        ))
                    }
                }
            }
            out
        }
        (_, Some(label)) => {
            let wanted = app.hub_view.layout.files_for(label);
            if wanted.is_empty() {
                return Err(warn_off(app, 409, format!("this repo has no '{label}' quantization")));
            }
            app.hub_view.files.iter().filter(|f| wanted.contains(&f.path)).cloned().collect()
        }
        _ => app.hub_view.files.iter().filter(|f| f.wanted).cloned().collect(),
    };

    if selected.is_empty() {
        return Err(warn_off(app, 409, "no files selected"));
    }
    if app.downloads.iter().any(|d| d.is_running() && d.repo == repo) {
        return Err(warn_off(app, 409, format!("{repo} is already downloading")));
    }

    let hub = hub_or_refuse(app)?;
    let Some(cli) = app.hf_cli.clone() else {
        return Err(error_off(
            app,
            503,
            "the hf CLI is not installed — press i on the Hub tab to install it",
        ));
    };
    let cache_dir = app.config.library.hub_cache();
    let token = app.config.hub.resolve_token().map(|t| t.value);
    let revision = app.hub_view.revision.clone();
    let concurrency = app.config.hub.concurrency;
    let files = selected;
    let tx = app.tx.clone();
    let dl_tx = app.download_tx.clone();
    let total: u64 = files.iter().map(|f| f.size).sum();

    app.info(format!(
        "downloading {} file(s), {} from {repo}",
        files.len(),
        crate::util::bytes(total)
    ));
    app.tab = Tab::Jobs;

    // Spawning the child and taking the first cache sample both touch the filesystem, so
    // this runs off the UI thread and reports back rather than blocking a keystroke.
    tokio::spawn(async move {
        match start_download(
            hub,
            cli,
            cache_dir,
            token,
            repo.clone(),
            revision,
            files,
            concurrency,
            dl_tx,
        )
        .await
        {
            Ok(dl) => {
                let _ = tx.send(Message::RegisterDownload(Box::new(dl)));
            }
            Err(e) => {
                let _ = tx.send(Message::Toast(crate::ui::widgets::Toast::new(
                    format!("could not start the download: {e:#}"),
                    ToastKind::Error,
                )));
            }
        }
    });
    Ok(Done::started())
}

/// Offer Hugging Face's own installer for the missing `hf` CLI.
pub fn offer_hf_install(app: &mut App) -> Outcome {
    if app.hf_cli.is_some() {
        return Err(warn_off(app, 409, "the hf CLI is already installed"));
    }
    if app.hf_installing {
        return Err(warn_off(app, 409, "the hf CLI is already being installed"));
    }
    Ok(ask(
        app,
        Confirm::new(
            "Install the Hugging Face CLI",
            vec![
                "paddock delegates Hub downloads to `hf`, and it is not installed.".into(),
                String::new(),
                "This runs Hugging Face's own installer, the method their CLI guide lists \
                 as recommended:"
                    .into(),
                String::new(),
                format!("  {}", crate::hub::INSTALL_COMMAND),
                String::new(),
                "It downloads and executes a script from hf.co and installs into \
                 ~/.local/bin. `--exclude-skill` keeps it from also writing agent skills \
                 into your home directory."
                    .into(),
            ],
            ConfirmAction::InstallHfCli,
            // Not destructive: it adds a tool, it does not remove or overwrite anything of
            // the user's. Defaulting to Cancel is still the right posture for a network
            // install, and Confirm::new already does that.
            false,
        ),
    ))
}

// ---------------------------------------------------------------- templates

/// List the `.jinja` files in a repo.
pub fn list_template_repo(app: &mut App, repo: &str) -> Outcome {
    let repo = repo.trim().to_string();
    if repo.is_empty() {
        // Field-shaped: the repo box is right there. `ui::input` toasts it, because a
        // terminal has nowhere else to put it.
        return Err(Refusal::quiet(400, "enter a Hugging Face repo id first"));
    }
    app.templates_view.repo.set(repo.clone());
    let hub = hub_or_refuse(app)?;
    app.templates_view.loading = true;
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let res = hub
            .info(&repo, "main")
            .await
            .map(|info| crate::ui::app::TemplateListing {
                files: crate::hub::jinja_files(&info.siblings),
                revision: info.sha.unwrap_or_else(|| "main".into()),
                repo: info.id,
            })
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(Message::TemplateRepo(Box::new(res)));
    });
    Ok(Done::started())
}

/// Download one repo file into the local store.
pub fn fetch_template(app: &mut App, repo: &str, revision: Option<&str>, path: &str) -> Outcome {
    let repo = repo.trim().to_string();
    if repo.is_empty() {
        return Err(Refusal::quiet(400, "a fetch needs a repo id"));
    }
    let revision = match revision
        .map(str::to_string)
        .or_else(|| app.templates_view.remote_revision.clone())
    {
        Some(r) => r,
        None => {
            return Err(Refusal::quiet(400, "no revision is known for that repo; list it first"))
        }
    };
    let path = path.to_string();
    let hub = hub_or_refuse(app)?;
    let name = crate::templates::name_for(&repo, &path);
    app.templates_view.loading = true;
    app.info(format!("fetching {path}"));
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let res = async {
            let jinja =
                hub.fetch_text(&repo, &revision, &path).await.map_err(|e| format!("{e:#}"))?;
            let meta = crate::templates::TemplateMeta {
                source: Some(repo.clone()),
                revision: Some(revision.clone()),
                repo_path: Some(path.clone()),
                ..Default::default()
            };
            crate::templates::save(&name, &jinja, meta)
                .map(|t| t.name)
                .map_err(|e| format!("{e:#}"))
        }
        .await;
        let _ = tx.send(Message::TemplateFetched(res));
    });
    Ok(Done::started())
}

/// Ask before writing a template into a checkpoint directory.
pub fn apply_template(app: &mut App, template: &str, model_path: &Path) -> Outcome {
    let Some(stored) = app.templates_view.stored.iter().find(|t| t.name == template) else {
        return Err(Refusal::quiet(404, format!("no template named '{template}'")));
    };
    let name = stored.name.clone();
    let version = stored.meta.version.clone();

    let Some(model) = app.models.iter().find(|m| m.path == model_path) else {
        return Err(not_found_model(app, model_path));
    };
    let model_name = model.name.clone();
    let model_path = model.path.clone();
    let targets = crate::templates::targets(model);
    if targets.is_empty() {
        return Err(error_off(
            app,
            409,
            format!("{model_name} has no directory to write a template into"),
        ));
    }
    let status = model.template_status.clone();

    let mut body = vec![
        match &version {
            Some(v) => format!("Apply '{name}' ({v}) to {model_name}?"),
            None => format!("Apply '{name}' to {model_name}?"),
        },
        String::new(),
        "FreeToken reads the chat template from the checkpoint, so this writes          chat_template.jinja into:"
            .into(),
    ];
    for t in &targets {
        body.push(format!("    {}", t.display()));
    }
    body.push(String::new());
    body.push(match &status {
        crate::templates::Status::BuiltIn => {
            "The checkpoint's own template is preserved and can be restored with u.".into()
        }
        crate::templates::Status::Foreign => {
            "There is already a chat_template.jinja here that paddock did not write; it              will be backed up, not lost."
                .to_string()
        }
        crate::templates::Status::Overridden(a) => {
            format!("This replaces the override '{}'. The checkpoint's original stays backed up.", a.name)
        }
    });
    // A cache directory is shared with every other tool reading it, so the reader needs to
    // know that this reaches further than the model in front of them.
    if targets.iter().any(|t| crate::templates::is_hub_cache_path(t)) {
        body.push(String::new());
        body.push(
            "That is inside the Hugging Face cache, which other tools on this machine read \
             too — they will see this template as well. A later `hf download` of the repo \
             may replace it. The checkpoint's own template is backed up either way, and u \
             restores it."
                .into(),
        );
    }
    if app.engine.is_live() {
        body.push(String::new());
        body.push(
            "The engine is running and read its template at load time, so restart it for              this to take effect."
                .into(),
        );
    }

    Ok(ask(
        app,
        Confirm::new(
            "Apply chat template",
            body,
            ConfirmAction::ApplyTemplate { template: name, model: model_path },
            false,
        ),
    ))
}

/// Write the template, optionally after a real render check.
fn write_template(app: &mut App, template_name: &str, model_path: &Path) {
    let Some(template) = crate::templates::get(template_name) else {
        app.error(format!("template '{template_name}' is no longer in the store"));
        return;
    };
    let jinja = match template.read() {
        Ok(j) => j,
        Err(e) => {
            app.error(format!("could not read the template: {e:#}"));
            return;
        }
    };
    let Some(model) = app.models.iter().find(|m| m.path == model_path).cloned() else {
        app.error("that model is no longer in the library");
        return;
    };

    let mut written = Vec::new();
    for dir in crate::templates::targets(&model) {
        match crate::templates::apply(&dir, &template, &jinja) {
            Ok(()) => written.push(dir),
            Err(e) => {
                app.error(format!("could not apply to {}: {e:#}", dir.display()));
                return;
            }
        }
    }
    // Writing nothing is a failure, not a quiet success. Reporting "applied to 0
    // directories" and then indexing the empty list is how this last went wrong.
    let Some(first) = written.first().cloned() else {
        app.error(format!(
            "nothing to apply '{}' to — {} has no writable directory",
            template.name, model.name
        ));
        return;
    };
    app.refresh_template_status(&model.path);
    app.success(format!(
        "applied '{}' to {} director{}",
        template.name,
        written.len(),
        if written.len() == 1 { "y" } else { "ies" }
    ));
    if app.engine.is_live() {
        app.warn("restart the engine for the new template to take effect");
    }
    // Verify against the real tokenizer now that it is in place.
    if app.config.templates.preflight {
        run_preflight(app, &template.name, &first, &template.path);
    }
}

pub fn request_revert_template(app: &mut App, model_path: &Path) -> Outcome {
    let Some(model) = app.models.iter().find(|m| m.path == model_path) else {
        return Err(not_found_model(app, model_path));
    };
    let name = model.name.clone();
    let path = model.path.clone();
    let targets = crate::templates::targets(model);
    if !model.template_status.is_overridden() {
        return Err(warn_off(
            app,
            409,
            format!("{name} is not using an paddock template override"),
        ));
    }
    let mut body = vec![
        format!("Restore {name}'s own chat template?"),
        String::new(),
        "This reverses the override in:".into(),
    ];
    for t in &targets {
        body.push(format!("    {}", t.display()));
    }
    Ok(ask(
        app,
        Confirm::new("Restore built-in template", body, ConfirmAction::RevertTemplate(path), false),
    ))
}

fn revert_template(app: &mut App, model_path: &Path) {
    let Some(model) = app.models.iter().find(|m| m.path == model_path).cloned() else {
        app.error("that model is no longer in the library");
        return;
    };
    let mut reverted = 0usize;
    for dir in crate::templates::targets(&model) {
        // The FTW build may never have had one applied; that is not an error.
        if !crate::templates::status(&dir).is_overridden() {
            continue;
        }
        match crate::templates::revert(&dir) {
            Ok(()) => reverted += 1,
            Err(e) => {
                app.error(format!("could not revert {}: {e:#}", dir.display()));
                return;
            }
        }
    }
    app.templates_view.preflight = None;
    app.refresh_template_status(&model.path);
    if reverted == 0 {
        app.error("found no override to restore");
        return;
    }
    app.success(format!("restored the built-in template in {reverted} director(ies)"));
    if app.engine.is_live() {
        app.warn("restart the engine for the change to take effect");
    }
}

// ---------------------------------------------------------------- sampling

/// Ask before merging sampling defaults into a checkpoint's `generation_config.json`.
pub fn request_apply_sampling(
    app: &mut App,
    model_path: &Path,
    sampling: crate::sampling::Sampling,
) -> Outcome {
    let Some(model) = app.models.iter().find(|m| m.path == model_path) else {
        return Err(not_found_model(app, model_path));
    };
    let model_name = model.name.clone();
    let path = model.path.clone();
    let format = model.format;
    let status = model.sampling_status.clone();
    let targets = crate::templates::targets(model);

    // GGUF is refused rather than warned about: the file would be written and then never
    // read, which looks exactly like a working override until someone checks the engine.
    if let Some(why) = crate::sampling::unsupported(format) {
        return Err(warn_off(app, 409, format!("{model_name}: {why}")));
    }
    if let Some(problem) = sampling.validate() {
        return Err(warn_off(app, 400, problem));
    }
    if targets.is_empty() {
        return Err(error_off(
            app,
            409,
            format!("{model_name} has no directory to write a generation config into"),
        ));
    }

    let mut body = vec![
        format!("Serve {model_name} with {}?", sampling.summary()),
        String::new(),
        "FreeToken reads its sampling defaults from the checkpoint, so this merges them \
         into generation_config.json in:"
            .into(),
    ];
    for t in &targets {
        body.push(format!("    {}", t.display()));
    }
    body.push(String::new());
    body.push(match &status {
        crate::sampling::Status::Checkpoint => {
            "Everything else in that file — the stop token ids especially — is kept, and \
             the original is backed up so u can restore it."
                .into()
        }
        crate::sampling::Status::Overridden(a) => format!(
            "This replaces the override '{}'. The checkpoint's original stays backed up.",
            a.sampling.summary()
        ),
    });
    for warning in sampling.warnings() {
        body.push(String::new());
        body.push(warning);
    }
    // A cache directory is shared with every other tool reading it, so the reader needs to
    // know that this reaches further than the model in front of them.
    if targets.iter().any(|t| crate::templates::is_hub_cache_path(t)) {
        body.push(String::new());
        body.push(
            "That is inside the Hugging Face cache, which other tools on this machine read \
             too — they will see these defaults as well. A later `hf download` of the repo \
             may replace them. The checkpoint's own config is backed up either way, and u \
             restores it."
                .into(),
        );
    }
    if app.engine.is_live() {
        body.push(String::new());
        body.push(
            "The engine read its sampling defaults at load time, so restart it for this to \
             take effect."
                .into(),
        );
    }

    Ok(ask(
        app,
        Confirm::new(
            "Set sampling defaults",
            body,
            ConfirmAction::ApplySampling { model: path, sampling },
            false,
        ),
    ))
}

fn write_sampling(app: &mut App, model_path: &Path, sampling: &crate::sampling::Sampling) {
    let Some(model) = app.models.iter().find(|m| m.path == model_path).cloned() else {
        app.error("that model is no longer in the library");
        return;
    };
    let mut written = 0usize;
    for dir in crate::templates::targets(&model) {
        match crate::sampling::apply(&dir, sampling) {
            Ok(()) => written += 1,
            Err(e) => {
                app.error(format!("could not apply to {}: {e:#}", dir.display()));
                return;
            }
        }
    }
    // Writing nothing is a failure, not a quiet success.
    if written == 0 {
        app.error(format!(
            "nothing to write sampling defaults to — {} has no writable directory",
            model.name
        ));
        return;
    }
    app.refresh_sampling_status(&model.path);
    app.success(format!("set {} for {}", sampling.summary(), model.name));
    if app.engine.is_live() {
        app.warn("restart the engine for the new sampling defaults to take effect");
    }
}

pub fn request_revert_sampling(app: &mut App, model_path: &Path) -> Outcome {
    let Some(model) = app.models.iter().find(|m| m.path == model_path) else {
        return Err(not_found_model(app, model_path));
    };
    let name = model.name.clone();
    let path = model.path.clone();
    let targets = crate::templates::targets(model);
    if !model.sampling_status.is_overridden() {
        return Err(warn_off(app, 409, format!("{name} is not using paddock sampling defaults")));
    }
    let mut body = vec![
        format!("Restore {name}'s own sampling defaults?"),
        String::new(),
        "This reverses the override in:".into(),
    ];
    for t in &targets {
        body.push(format!("    {}", t.display()));
    }
    Ok(ask(
        app,
        Confirm::new(
            "Restore checkpoint sampling",
            body,
            ConfirmAction::RevertSampling(path),
            false,
        ),
    ))
}

fn revert_sampling(app: &mut App, model_path: &Path) {
    let Some(model) = app.models.iter().find(|m| m.path == model_path).cloned() else {
        app.error("that model is no longer in the library");
        return;
    };
    let mut reverted = 0usize;
    for dir in crate::templates::targets(&model) {
        // The FTW build may never have had one applied; that is not an error.
        if !crate::sampling::status(&dir).is_overridden() {
            continue;
        }
        match crate::sampling::revert(&dir) {
            Ok(()) => reverted += 1,
            Err(e) => {
                app.error(format!("could not revert {}: {e:#}", dir.display()));
                return;
            }
        }
    }
    app.refresh_sampling_status(&model.path);
    if reverted == 0 {
        app.error("found no override to restore");
        return;
    }
    app.success(format!("restored the checkpoint's sampling in {reverted} director(ies)"));
    if app.engine.is_live() {
        app.warn("restart the engine for the change to take effect");
    }
}

/// Render a stored template against a model's real tokenizer.
pub fn verify_template(app: &mut App, template: &str, model_path: &Path) -> Outcome {
    let Some(stored) = app.templates_view.stored.iter().find(|t| t.name == template) else {
        return Err(Refusal::quiet(404, format!("no template named '{template}'")));
    };
    let (name, jinja) = (stored.name.clone(), stored.path.clone());
    let Some(model) = app.models.iter().find(|m| m.path == model_path) else {
        return Err(not_found_model(app, model_path));
    };
    let model_dir = model.path.clone();
    run_preflight_checked(app, &name, &model_dir, &jinja)
}

/// Spawn the template render check. It needs FreeToken's Python (for transformers), so
/// it is a no-op with a clear message when that is not available.
fn run_preflight(app: &mut App, name: &str, model_dir: &Path, jinja: &Path) {
    let _ = run_preflight_checked(app, name, model_dir, jinja);
}

fn run_preflight_checked(app: &mut App, name: &str, model_dir: &Path, jinja: &Path) -> Outcome {
    let Some(ft) = app.ft.clone() else {
        return Err(warn_off(app, 503, "cannot verify the template without the FreeToken CLI"));
    };
    let Some(argv) = crate::ft::preflight::template_command(&ft, model_dir, jinja) else {
        return Err(warn_off(
            app,
            503,
            "cannot verify the template: no Python found beside the FreeToken CLI",
        ));
    };

    app.templates_view.checking = true;
    app.templates_view.preflight = None;
    app.info(format!("checking that '{name}' renders…"));

    let name = name.to_string();
    let env = app.config.freetoken.env.clone();
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let outcome = crate::ft::preflight::run(argv, &env).await;
        let _ = tx.send(Message::TemplatePreflight(name, outcome));
    });
    Ok(Done::started())
}

pub fn delete_template(app: &mut App, name: &str) -> Outcome {
    let Some(stored) = app.templates_view.stored.iter().find(|t| t.name == name) else {
        return Err(Refusal::quiet(404, format!("no template named '{name}'")));
    };
    let name = stored.name.clone();
    Ok(ask(
        app,
        Confirm::new(
            "Delete template",
            vec![
                format!("Remove '{name}' from the template store?"),
                String::new(),
                "Checkpoints it was already applied to keep using it; this only \
                 removes the stored copy."
                    .into(),
            ],
            ConfirmAction::DeleteTemplate(name),
            true,
        ),
    ))
}

// ---------------------------------------------------------------- serve knobs

/// What setting a knob did: whether it is now set, and which mutually exclusive knobs
/// were cleared to make room for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnobSet {
    pub set: bool,
    pub cleared: Vec<String>,
}

fn knob_or_refuse(key: &str) -> Result<&'static Knob, Refusal> {
    crate::knobs::knob(key).ok_or_else(|| Refusal::quiet(404, format!("no knob named '{key}'")))
}

/// Set a knob, or unset it when `value` is `None` or empty.
pub fn set_knob(app: &mut App, key: &str, value: Option<&str>) -> Result<KnobSet, Refusal> {
    let k = knob_or_refuse(key)?;
    let value = value.map(str::trim).unwrap_or("");
    if value.is_empty() {
        unset_knob(app, k, true);
        return Ok(KnobSet::default());
    }
    if let Some(msg) = crate::knobs::validate_value(k, value) {
        // No toast. A rejected value belongs against the field that produced it, and the
        // web UI has an inline slot for it; the terminal has none, so `commit_knob_edit`
        // raises the toast there. See docs/web-api.md section 4.8.
        return Err(Refusal::quiet(409, format!("{}: {msg}", k.flag)));
    }
    // `ServeConfig::set` clears whatever the new value excludes; report which, since the
    // browser's other fields have just been emptied under it.
    let candidates: Vec<&'static str> =
        k.exclusive_with.iter().copied().filter(|other| *other != k.key).collect();
    let before: Vec<&'static str> =
        candidates.iter().copied().filter(|other| app.serve.is_set(other)).collect();
    app.serve.set(k.key, value);
    // Read back rather than predicted. `set` has one case that does not store: a `Flag`
    // given "false" unsets the knob and clears nothing, so reporting `set: true` there was
    // a lie the browser then rendered as a checked box.
    let set = app.serve.is_set(k.key);
    let cleared: Vec<String> =
        before.into_iter().filter(|other| !app.serve.is_set(other)).map(str::to_string).collect();
    Ok(KnobSet { set, cleared })
}

/// Clear a knob. `announce` is what separates `x` on the Serve tab, which says so, from
/// committing an empty editor, which never has.
pub fn unset_knob(app: &mut App, k: &'static Knob, announce: bool) {
    if !app.serve.is_set(k.key) {
        return;
    }
    app.serve.unset(k.key);
    if announce {
        app.info(format!("{} reset to its default", k.label));
    }
}

/// Toggle a flag knob, or set it outright. Says what it did, as `Enter` on the Serve tab
/// always has.
pub fn toggle_flag(app: &mut App, key: &str, on: Option<bool>) -> Result<bool, Refusal> {
    let k = knob_or_refuse(key)?;
    if !matches!(k.kind, Kind::Flag) {
        return Err(warn_off(app, 409, format!("{} is not a flag", k.flag)));
    }
    let wanted = on.unwrap_or(!app.serve.flag(k.key));
    if wanted {
        app.serve.set(k.key, "true");
    } else {
        app.serve.unset(k.key);
    }
    let state = if wanted { "on" } else { "off" };
    app.info(format!("{} {state}", k.label));
    Ok(wanted)
}

/// Walk a choice knob's options, wrapping through unset so there is always a way back to
/// the default. A flag knob is toggled instead, which is what the `Space` key does.
pub fn cycle_knob(app: &mut App, key: &str, delta: isize) -> Result<Option<String>, Refusal> {
    let k = knob_or_refuse(key)?;
    match k.kind {
        Kind::Flag => {
            app.serve.toggle_flag(k.key);
            Ok(app.serve.flag(k.key).then(|| "true".to_string()))
        }
        Kind::Choice(options) => {
            if options.is_empty() {
                return Ok(app.serve.get(k.key).map(str::to_string));
            }
            let current = app.serve.get(k.key).and_then(|v| options.iter().position(|o| *o == v));
            let next = match current {
                None if delta > 0 => Some(0),
                None => Some(options.len() - 1),
                Some(i) => {
                    let n = i as isize + delta;
                    if n < 0 || n >= options.len() as isize {
                        None
                    } else {
                        Some(n as usize)
                    }
                }
            };
            match next {
                Some(i) => {
                    app.serve.set(k.key, options[i]);
                    Ok(Some(options[i].to_string()))
                }
                None => {
                    app.serve.unset(k.key);
                    Ok(None)
                }
            }
        }
        _ => Err(warn_off(app, 409, format!("{} is not a choice knob", k.flag))),
    }
}

/// Work out what this hardware would prefer, and show it before changing anything.
///
/// Planning is pure and instant — every input is already in memory — so this needs no
/// job, no spinner and no confirmation. Returns whether a plan is now held.
pub fn build_plan(app: &mut App) -> Result<bool, Refusal> {
    match views::plan::build(app) {
        Ok(plan) => {
            if plan.is_empty() && plan.unpriced.is_none() {
                app.success("nothing to change — this configuration is already optimal here");
                return Ok(false);
            }
            app.serve_view.plan = Some(plan);
            Ok(true)
        }
        Err(why) => Err(warn_off(app, 409, format!("cannot plan: {why}"))),
    }
}

/// Fold the plan's edits into the serve configuration.
pub fn apply_plan(app: &mut App) -> Result<usize, Refusal> {
    let Some(plan) = app.serve_view.plan.take() else {
        return Err(warn_off(app, 409, "no plan is held"));
    };
    let changed = plan.apply(&mut app.serve);
    match changed {
        0 => app.info("the configuration already matched the plan"),
        n => app.success(format!(
            "applied {n} change{} — press g to serve with it",
            if n == 1 { "" } else { "s" }
        )),
    }
    Ok(changed)
}

pub fn dismiss_plan(app: &mut App) {
    app.serve_view.plan = None;
}

// ---------------------------------------------------------------- profiles

/// Save the current serve configuration under `name`. Returns whether it is new.
pub fn save_profile(app: &mut App, name: &str) -> Result<bool, Refusal> {
    let name = name.trim().to_string();
    if name.is_empty() {
        // Field-shaped, like every other empty-input refusal; `ui::input` toasts it.
        return Err(Refusal::quiet(400, "a profile needs a name"));
    }
    let existed = app.profiles.get(&name).is_some();
    app.profiles.upsert(crate::ui::app::profile_from(name.clone(), &app.serve));
    app.profiles.last_used = Some(name.clone());
    save_profiles(app);
    app.success(if existed {
        format!("profile '{name}' updated")
    } else {
        format!("profile '{name}' saved")
    });
    if let Some(i) = app.profiles.items.iter().position(|p| p.name == name) {
        app.serve_view.profile_sel.index = i;
    }
    Ok(!existed)
}

pub fn load_profile(app: &mut App, name: &str) -> Outcome {
    let Some(p) = app.profiles.get(name) else {
        return Err(Refusal::quiet(404, format!("no profile named '{name}'")));
    };
    app.serve = p.serve.clone();
    let name = p.name.clone();
    app.profiles.last_used = Some(name.clone());
    save_profiles(app);
    app.success(format!("loaded profile '{name}'"));
    Ok(Done::Ok)
}

pub fn delete_profile(app: &mut App, name: &str) -> Outcome {
    let Some(p) = app.profiles.get(name) else {
        return Err(Refusal::quiet(404, format!("no profile named '{name}'")));
    };
    let name = p.name.clone();
    Ok(ask(
        app,
        Confirm::new(
            "Delete profile",
            vec![format!("Delete the saved profile '{name}'?")],
            ConfirmAction::DeleteProfile(name),
            true,
        ),
    ))
}

fn save_profiles(app: &mut App) {
    if let Err(e) = app.profiles.save() {
        app.error(format!("could not save profiles: {e:#}"));
    }
}

// ---------------------------------------------------------------- cache

/// The live geometry, or the refusal the Cache tab shows in its place.
fn geometry(app: &mut App) -> Result<crate::ft::types::CacheGeometry, Refusal> {
    match app.telemetry.cache.as_ref().map(|c| c.geometry.clone()) {
        Some(geo) => Ok(geo),
        None => {
            Err(warn_off(app, 503, "cache geometry is only available while the engine is serving"))
        }
    }
}

fn present(geo: &crate::ft::types::CacheGeometry, pool: Pool) -> Result<(), Refusal> {
    if crate::cache_pools::present(geo, pool) {
        Ok(())
    } else {
        Err(Refusal::quiet(404, format!("this model exposes no {} pool", pool.label())))
    }
}

/// Stage a pool size. `None` clears the edit, which is the `r` key.
pub fn set_cache_pending(
    app: &mut App,
    pool: Pool,
    value: Option<u64>,
) -> Result<Option<u64>, Refusal> {
    let geo = geometry(app)?;
    present(&geo, pool)?;
    // The same bounds the Cache view draws and the snapshot sends: one conversion from
    // FreeToken's published limits into the unit `/v1/cache/rebuild` accepts.
    let bounds = crate::cache_pools::geometry(&geo, pool);
    let staged = value.map(|v| bounds.clamp(v));
    // "Back to where it started" is not an edit, which is how `views::cache::adjust`
    // already treats it.
    let staged = staged.filter(|v| *v != bounds.current);
    app.cache_view.set_pending(pool, staged);
    Ok(staged)
}

/// Nudge a pool by a fraction of its maximum — the arrow keys, ±1% or ±10%.
pub fn adjust_cache(app: &mut App, pool: Pool, percent: f64) -> Result<Option<u64>, Refusal> {
    let geo = geometry(app)?;
    present(&geo, pool)?;
    views::cache::adjust(app, pool, percent);
    Ok(app.cache_view.pending_for(pool))
}

pub fn reset_cache(app: &mut App) {
    app.cache_view.clear_pending();
}

/// Ask before rebuilding the pools on a live engine.
pub fn apply_cache(app: &mut App) -> Outcome {
    let geo = geometry(app)?;
    if !app.cache_view.has_pending() {
        return Err(warn_off(app, 409, "nothing to apply"));
    }
    let pools: Vec<Pool> =
        Pool::ALL.iter().copied().filter(|p| crate::cache_pools::present(&geo, *p)).collect();
    let active = app.telemetry.stats.as_ref().map(|s| s.requests.active).unwrap_or(0);
    let mut body = vec!["Resize the cache pools on the running engine?".to_string(), String::new()];
    for p in &pools {
        if let Some(v) = app.cache_view.pending_for(*p) {
            body.push(format!(
                "  {}: {} → {} {}",
                p.label(),
                crate::util::count(crate::cache_pools::current(&geo, *p)),
                crate::util::count(v),
                p.unit()
            ));
        }
    }
    body.push(String::new());
    body.push(if active > 0 {
        format!(
            "{active} request(s) are in flight. The engine only rebuilds while idle, so \
             this will be rejected until they finish."
        )
    } else {
        "Weights stay loaded; only the pools are rebuilt.".to_string()
    });
    Ok(ask(app, Confirm::new("Rebuild cache", body, ConfirmAction::ApplyCacheRebuild, false)))
}

fn apply_cache_rebuild(app: &mut App) -> Done {
    let req = rebuild_from_pending(&app.cache_view);
    if req.is_empty() {
        return Done::Ok;
    }
    app.cache_view.applying = true;
    let client = app.client.clone();
    let tx = app.tx.clone();
    app.info("rebuilding cache pools…");
    tokio::spawn(async move {
        let res = client
            .cache_rebuild(&req)
            .await
            .map(|_| "cache pools rebuilt".to_string())
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(Message::CacheRebuilt(res));
    });
    Done::started()
}

// ---------------------------------------------------------------- jobs

pub fn run_bench(app: &mut App) -> Outcome {
    if let Some(reason) = app.gpu_busy_reason() {
        return Err(warn_off(app, 409, format!("cannot benchmark: {reason}")));
    }
    let ft = freetoken(app)?;
    let mut args = Vec::new();
    if let Some(gpu) = app.serve.get("gpu") {
        args.push("--gpu".to_string());
        args.push(gpu.to_string());
    }
    match spawn_job(
        &ft,
        JobSpec {
            kind: JobKind::Bench,
            subcommand: &["bench", "bw"],
            args,
            env: &app.config.freetoken.env,
            title: "CPU vs PCIe bandwidth".into(),
            log_capacity: app.config.ui.log_capacity,
        },
        app.job_tx.clone(),
    ) {
        Ok(job) => {
            let id = job.id;
            app.jobs.push(job);
            app.jobs_view.sel.last(views::jobs::rows(app).len());
            app.info("benchmarking CPU and PCIe bandwidth; this takes a few minutes");
            Ok(Done::Started { job_id: Some(id), log_path: None })
        }
        Err(e) => Err(error_off(app, 500, format!("could not start the benchmark: {e:#}"))),
    }
}

pub fn cancel_job(app: &mut App, id: u64) -> Outcome {
    let Some(job) = app.jobs.iter().find(|j| j.id == id) else {
        return Err(Refusal::quiet(404, format!("no job with id {id}")));
    };
    if !job.is_running() {
        return Err(warn_off(app, 409, "that job has already finished"));
    }
    let title = job.title.clone();
    Ok(ask(
        app,
        Confirm::new(
            "Cancel job",
            vec![
                format!("Cancel '{title}'?"),
                String::new(),
                "A partly written FTW directory is left behind and must be deleted before \
                 the conversion can be retried."
                    .into(),
            ],
            ConfirmAction::CancelJob(id),
            true,
        ),
    ))
}

pub fn cancel_download(app: &mut App, id: u64) -> Outcome {
    let Some(d) = app.downloads.iter().find(|d| d.id == id) else {
        return Err(Refusal::quiet(404, format!("no download with id {id}")));
    };
    if !d.is_running() {
        return Err(warn_off(app, 409, "that download has already finished"));
    }
    let repo = d.repo.clone();
    Ok(ask(
        app,
        Confirm::new(
            "Cancel download",
            vec![
                format!("Cancel the download of {repo}?"),
                String::new(),
                "Completed files are kept and a partial file resumes where it stopped.".into(),
            ],
            ConfirmAction::CancelDownload(id),
            false,
        ),
    ))
}

/// Drop every finished job and download. Returns how many rows went.
pub fn clear_finished(app: &mut App) -> usize {
    let before = app.jobs.len() + app.downloads.len();
    app.jobs.retain(|j| j.is_running());
    app.downloads.retain(|d| d.is_running());
    let removed = before - (app.jobs.len() + app.downloads.len());
    app.jobs_view.sel.clamp(views::jobs::rows(app).len());
    if removed > 0 {
        app.info(format!("cleared {removed} finished entr(ies)"));
    }
    removed
}

// ---------------------------------------------------------------- logs and requests

pub fn clear_logs(app: &mut App) {
    app.engine.log.clear();
    app.logs_view.scroll = 0;
}

pub fn set_requests_paused(app: &mut App, paused: bool) {
    app.requests_view.paused = paused;
    let state = if paused { "paused" } else { "resumed" };
    app.info(format!("request polling {state}"));
}

pub fn clear_requests(app: &mut App) {
    app.requests_view.clear();
    app.requests_view.sel.first();
}
