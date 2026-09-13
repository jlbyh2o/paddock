//! Every operation the two front ends share.
//!
//! A key in the TUI and a request to the web server must do the same thing, so the thing
//! itself lives here once, as a function of `&mut App` plus an explicit target: a model
//! *path*, a job *id*, a profile *name*, a knob *key and value*. `ui::input` resolves
//! "the selected row" to a target and calls in; the web layer resolves a request body to
//! the same target and calls the same function.
//!
//! Refusals are reported twice over, deliberately. Every path that declines to act still
//! pushes the toast the TUI has always shown, and also returns a [`Refusal`] carrying the
//! HTTP status the web API documents for it — so the terminal keeps behaving exactly as
//! it did while a browser learns why nothing happened.

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
}

pub type Outcome = Result<Done, Refusal>;

/// Decline, saying so in the terminal and on the wire at once.
fn refuse(app: &mut App, status: u16, kind: ToastKind, message: impl Into<String>) -> Refusal {
    let message = message.into();
    app.toast(message.clone(), kind);
    Refusal { status, message }
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
        ConfirmAction::StopEngine { force } => {
            app.engine.stop(force);
            app.info(if force { "force-stopping the engine" } else { "stopping the engine" });
            Done::Ok
        }
        ConfirmAction::DeleteModel(path) => {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {
                    app.success(format!("deleted {}", path.display()));
                    app.request_scan();
                }
                Err(e) => app.error(format!("could not delete {}: {e}", path.display())),
            }
            Done::Ok
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
        ConfirmAction::ConvertAnyway(source) => start_conversion(app, &source),
        ConfirmAction::ReconvertModel(source) => {
            let out = ftw_out(app, &source);
            if let Err(e) = std::fs::remove_dir_all(&out) {
                app.error(format!("could not remove {}: {e}", out.display()));
                return Done::Ok;
            }
            app.info(format!("removed the incomplete {}", out.display()));
            if let Some(i) = app.models.iter().position(|m| m.path == source) {
                app.models_view.sel.index = i;
            }
            let done = begin_conversion(app, &source);
            app.request_scan();
            done
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
        return Err(Refusal { status: 409, message: "no confirmation is pending".into() });
    };
    if !accept {
        return Ok(Done::Ok);
    }
    Ok(run_action(app, confirm.action))
}

// ---------------------------------------------------------------- engine

pub fn start_engine(app: &mut App) -> Outcome {
    if app.engine.is_live() {
        return Err(warn_off(app, 409, "an engine is already running; stop it first"));
    }
    if let Some(job) = app.jobs.iter().find(|j| j.is_running()) {
        let kind = job.kind.label();
        return Err(warn_off(
            app,
            409,
            format!("a {kind} job is using the GPU; wait for it or cancel it first"),
        ));
    }
    let ft = freetoken(app)?;

    let errors = app.serve.validate();
    if !errors.is_empty() {
        let (key, msg) = &errors[0];
        let flag = crate::knobs::knob(key).map(|k| k.flag).unwrap_or(key);
        let message = format!("{flag}: {msg}");
        app.tab = Tab::Serve;
        return Err(error_off(app, 409, message));
    }

    let model = app.serve.get("model").unwrap_or_default().to_string();
    let port = app.serve.get("port").and_then(|p| p.parse().ok()).unwrap_or(app.config.server.port);
    let args = app.serve.to_args();

    match app.engine.start(&ft, args, &app.config.freetoken.env, model.clone(), port) {
        Ok(path) => {
            app.telemetry = Telemetry::default();
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
                    "This engine was started by an earlier ft-man run and re-attached to. \
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
    app.serve.set("model", target.display().to_string());
    app.serve.set("served_model_name", served);
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
        let size = crate::models::dir_size(&out);
        return Ok(ask(
            app,
            Confirm::new(
                "Retry conversion",
                vec![
                    format!("{} has leftovers from a conversion that failed.", name),
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
            ),
        ));
    }

    Ok(begin_conversion(app, &source))
}

pub fn delete_model(app: &mut App, path: &Path) -> Outcome {
    let Some(model) = app.models.iter().find(|m| m.path == path) else {
        return Err(not_found_model(app, path));
    };
    let path = model.path.clone();
    let name = model.name.clone();
    let size = crate::models::dir_size(&path);
    Ok(ask(
        app,
        Confirm::new(
            "Delete checkpoint",
            vec![
                format!("Permanently delete {name}?"),
                String::new(),
                path.display().to_string(),
                format!("This frees {} and cannot be undone.", crate::util::bytes(size)),
            ],
            ConfirmAction::DeleteModel(path),
            true,
        ),
    ))
}

/// A model named by a path that is no longer in the library. No toast: the TUI resolves
/// its target from the list it just drew and cannot reach this, so the only reader is a
/// browser acting on a stale list.
fn not_found_model(_app: &mut App, _path: &Path) -> Refusal {
    Refusal { status: 404, message: "that model is no longer in the library".into() }
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
        return Err(Refusal { status: 400, message: "a search needs a query".into() });
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
    let archs = app.supported_archs.clone();
    let hw = crate::compat::Hardware {
        vram_bytes: app.gpus.first().map(|g| g.memory_total).unwrap_or(0),
        host_ram_bytes: app.host.memory_total,
        free_disk_bytes: crate::hub::disk_free(&app.hub_view.target.value).unwrap_or(0),
    };
    app.hub_view.checking_compat = true;
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let res = async {
            let raw = hub
                .fetch_text(&repo, &revision, "config.json")
                .await
                .map_err(|e| format!("{e:#}"))?;
            let config: serde_json::Value =
                serde_json::from_str(&raw).map_err(|e| format!("config.json is not JSON: {e}"))?;
            // Size comes from the listing the caller already has; passing 0 keeps the
            // report to what the config alone can say.
            Ok::<_, String>(crate::compat::evaluate(&config, 0, archs.as_deref(), hw))
        }
        .await;
        let _ = tx.send(Message::Compatibility(Box::new(res)));
    });
}

/// Apply a quantization to the file selection.
pub fn choose_variant(app: &mut App, label: &str) -> Outcome {
    let Some(label) =
        app.hub_view.layout.weights().find(|v| v.label == label).map(|v| v.label.clone())
    else {
        return Err(Refusal {
            status: 409,
            message: format!("this repo has no '{label}' quantization"),
        });
    };
    let wanted = app.hub_view.layout.files_for(&label);
    for f in &mut app.hub_view.files {
        f.wanted = wanted.contains(&f.path);
    }
    let total: u64 = app.hub_view.files.iter().filter(|f| f.wanted).map(|f| f.size).sum();
    let count = app.hub_view.files.iter().filter(|f| f.wanted).count();
    app.hub_view.variant = Some(label.clone());
    app.hub_view.custom_selection = false;
    app.info(format!("{label}: {count} file(s), {}", crate::util::bytes(total)));
    Ok(Done::Ok)
}

/// Toggle one file by path. `wanted` of `None` flips it, which is what `Space` does.
pub fn toggle_file(app: &mut App, path: &str, wanted: Option<bool>) -> Result<bool, Refusal> {
    let Some(f) = app.hub_view.files.iter_mut().find(|f| f.path == path) else {
        return Err(Refusal {
            status: 404,
            message: format!("no file named {path} in this repo's listing"),
        });
    };
    f.wanted = wanted.unwrap_or(!f.wanted);
    let now = f.wanted;
    // The selection no longer is a quantization, so nothing downstream may go on
    // claiming it is one.
    app.hub_view.custom_selection = true;
    Ok(now)
}

/// Select every file, or none. Returns how many are now wanted.
pub fn select_files(app: &mut App, all: bool) -> usize {
    app.hub_view.files.iter_mut().for_each(|f| f.wanted = all);
    app.hub_view.custom_selection = true;
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
                        return Err(Refusal {
                            status: 400,
                            message: format!("{p} is not in this repo's listing"),
                        })
                    }
                }
            }
            out
        }
        (_, Some(label)) => {
            let wanted = app.hub_view.layout.files_for(label);
            if wanted.is_empty() {
                return Err(Refusal {
                    status: 409,
                    message: format!("this repo has no '{label}' quantization"),
                });
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
                "ft-man delegates Hub downloads to `hf`, and it is not installed.".into(),
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
        return Err(warn_off(app, 400, "enter a Hugging Face repo id first"));
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
        return Err(Refusal { status: 400, message: "a fetch needs a repo id".into() });
    }
    let revision =
        match revision.map(str::to_string).or_else(|| app.templates_view.remote_revision.clone()) {
            Some(r) => r,
            None => {
                return Err(Refusal {
                    status: 400,
                    message: "no revision is known for that repo; list it first".into(),
                })
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
        return Err(Refusal { status: 404, message: format!("no template named '{template}'") });
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
    let status = crate::templates::status(&model_path);

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
            "There is already a chat_template.jinja here that ft-man did not write; it              will be backed up, not lost."
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
    if !crate::templates::status(&path).is_overridden() {
        return Err(warn_off(app, 409, format!("{name} is not using an ft-man template override")));
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
    if reverted == 0 {
        app.error("found no override to restore");
        return;
    }
    app.success(format!("restored the built-in template in {reverted} director(ies)"));
    if app.engine.is_live() {
        app.warn("restart the engine for the change to take effect");
    }
}

/// Render a stored template against a model's real tokenizer.
pub fn verify_template(app: &mut App, template: &str, model_path: &Path) -> Outcome {
    let Some(stored) = app.templates_view.stored.iter().find(|t| t.name == template) else {
        return Err(Refusal { status: 404, message: format!("no template named '{template}'") });
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
        return Err(Refusal { status: 404, message: format!("no template named '{name}'") });
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
    crate::knobs::knob(key)
        .ok_or_else(|| Refusal { status: 404, message: format!("no knob named '{key}'") })
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
        let flag = k.flag;
        return Err(error_off(app, 409, format!("{flag}: {msg}")));
    }
    // `ServeConfig::set` clears whatever the new value excludes; report which, since the
    // browser's other fields have just been emptied under it.
    let cleared: Vec<String> = k
        .exclusive_with
        .iter()
        .filter(|other| **other != k.key && app.serve.is_set(other))
        .map(|other| (*other).to_string())
        .collect();
    app.serve.set(k.key, value);
    Ok(KnobSet { set: true, cleared })
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
        return Err(Refusal { status: 409, message: format!("{} is not a flag", k.flag) });
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
        _ => Err(Refusal { status: 409, message: format!("{} is not a choice knob", k.flag) }),
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
        return Err(Refusal { status: 409, message: "no plan is held".into() });
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
        return Err(warn_off(app, 400, "a profile needs a name"));
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
        return Err(Refusal { status: 404, message: format!("no profile named '{name}'") });
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
        return Err(Refusal { status: 404, message: format!("no profile named '{name}'") });
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
        None => Err(Refusal {
            status: 503,
            message: "cache geometry is only available while the engine is serving".into(),
        }),
    }
}

fn present(geo: &crate::ft::types::CacheGeometry, pool: Pool) -> Result<(), Refusal> {
    if views::cache::pool_present(geo, pool) {
        Ok(())
    } else {
        Err(Refusal {
            status: 404,
            message: format!("this model exposes no {} pool", pool.label()),
        })
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
    let staged = value.map(|v| {
        let max = views::cache::pool_max(&geo, pool);
        let min = geo
            .limit(views::cache::limit_key(pool), "min")
            .map(|m| m / views::cache::tokens_per_unit(&geo, pool).max(1))
            .unwrap_or(1)
            .max(1);
        v.clamp(min.min(max.max(1)), max.max(1))
    });
    // "Back to where it started" is not an edit, which is how `views::cache::adjust`
    // already treats it.
    let staged = staged.filter(|v| *v != views::cache::pool_current(&geo, pool));
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
        Pool::ALL.iter().copied().filter(|p| views::cache::pool_present(&geo, *p)).collect();
    let active = app.telemetry.stats.as_ref().map(|s| s.requests.active).unwrap_or(0);
    let mut body = vec!["Resize the cache pools on the running engine?".to_string(), String::new()];
    for p in &pools {
        if let Some(v) = app.cache_view.pending_for(*p) {
            body.push(format!(
                "  {}: {} → {} {}",
                p.label(),
                crate::util::count(views::cache::pool_current(&geo, *p)),
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
        return Err(Refusal { status: 404, message: format!("no job with id {id}") });
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
        return Err(Refusal { status: 404, message: format!("no download with id {id}") });
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
