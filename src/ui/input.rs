//! Keyboard handling and the actions keys trigger.
//!
//! Dispatch is layered: overlays first (a modal owns the keyboard completely), then
//! text-entry modes, then the active tab. Anything that spawns a process, deletes a
//! directory, or stops the engine goes through a confirmation unless the user has turned
//! those off.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::ft::proc::{spawn_job, JobKind, JobSpec};
use crate::hub::{start_download, Hub};
use crate::knobs::{knobs_in, Kind, Knob};
use crate::models::Format;
use crate::ui::app::{
    rebuild_from_pending, App, HubFocus, Message, Pool, Tab, Telemetry, TemplatePane,
};
use crate::ui::views;
use crate::ui::widgets::{Confirm, ConfirmAction, ToastKind};

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Terminals that report key releases would otherwise fire every binding twice.
    if key.kind == KeyEventKind::Release {
        return;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        app.should_quit = true;
        return;
    }

    if app.confirm.is_some() {
        confirm_key(app, key);
        return;
    }
    if app.show_help {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::F(1)
        ) {
            app.show_help = false;
        }
        return;
    }
    // The plan overlay is read-then-decide: it holds the keyboard so a stray key cannot
    // half-apply it, and only A commits.
    if app.serve_view.plan.is_some() {
        match key.code {
            KeyCode::Char('A') => apply_plan(app),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('a') => app.serve_view.plan = None,
            _ => {}
        }
        return;
    }

    // A text field has the keyboard until it is dismissed.
    if let Some(handled) = text_entry(app, key) {
        if handled {
            return;
        }
    }

    if global_key(app, key) {
        return;
    }

    match app.tab {
        Tab::Dashboard => dashboard_key(app, key),
        Tab::Models => models_key(app, key),
        Tab::Hub => hub_key(app, key),
        Tab::Templates => templates_key(app, key),
        Tab::Serve => serve_key(app, key),
        Tab::Cache => cache_key(app, key),
        Tab::Jobs => jobs_key(app, key),
        Tab::Requests => requests_key(app, key),
        Tab::Logs => logs_key(app, key),
    }
}

// ---------------------------------------------------------------- overlays

fn confirm_key(app: &mut App, key: KeyEvent) {
    let Some(confirm) = app.confirm.as_mut() else { return };
    match key.code {
        KeyCode::Left | KeyCode::Char('h') => confirm.selected = confirm.selected.saturating_sub(1),
        KeyCode::Right | KeyCode::Char('l') => {
            confirm.selected = (confirm.selected + 1).min(confirm.options.len() - 1)
        }
        KeyCode::Tab => confirm.selected = (confirm.selected + 1) % confirm.options.len(),
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            confirm.selected = 1;
            let action = confirm.action.clone();
            app.confirm = None;
            run_action(app, action);
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.confirm = None,
        KeyCode::Enter => {
            let accepted = confirm.accepted();
            let action = confirm.action.clone();
            app.confirm = None;
            if accepted {
                run_action(app, action);
            }
        }
        _ => {}
    }
}

/// Carry out a confirmed action.
fn run_action(app: &mut App, action: ConfirmAction) {
    match action {
        ConfirmAction::Quit => app.should_quit = true,
        ConfirmAction::StopEngine { force } => {
            app.engine.stop(force);
            app.info(if force { "force-stopping the engine" } else { "stopping the engine" });
        }
        ConfirmAction::DeleteModel(path) => match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                app.success(format!("deleted {}", path.display()));
                app.request_scan();
            }
            Err(e) => app.error(format!("could not delete {}: {e}", path.display())),
        },
        ConfirmAction::CancelJob(id) => {
            if let Some(j) = app.jobs.iter_mut().find(|j| j.id == id) {
                j.cancel();
            }
        }
        ConfirmAction::CancelDownload(id) => {
            if let Some(d) = app.downloads.iter_mut().find(|d| d.id == id) {
                d.cancel();
                app.warn("download canceled");
            }
        }
        ConfirmAction::DeleteProfile(name) => {
            if app.profiles.remove(&name) {
                if app.profiles.last_used.as_deref() == Some(name.as_str()) {
                    app.profiles.last_used = None;
                }
                save_profiles(app);
                app.success(format!("deleted profile '{name}'"));
            }
        }
        ConfirmAction::InstallHfCli => {
            app.hf_installing = true;
            app.info("installing the hf CLI…");
            let tx = app.tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(Message::HfInstalled(crate::hub::install_cli().await));
            });
        }
        ConfirmAction::ApplyCacheRebuild => apply_cache_rebuild(app),
        ConfirmAction::ApplyTemplate { template, model } => write_template(app, &template, &model),
        ConfirmAction::RevertTemplate(model) => revert_template(app, &model),
        ConfirmAction::ConvertAnyway(source) => start_conversion(app, &source),
        ConfirmAction::ReconvertModel(source) => {
            let out = ftw_out(app, &source);
            if let Err(e) = std::fs::remove_dir_all(&out) {
                app.error(format!("could not remove {}: {e}", out.display()));
                return;
            }
            app.info(format!("removed the incomplete {}", out.display()));
            if let Some(i) = app.models.iter().position(|m| m.path == source) {
                app.models_view.sel.index = i;
            }
            begin_conversion(app, &source);
            app.request_scan();
        }
        ConfirmAction::DeleteTemplate(name) => match crate::templates::remove(&name) {
            Ok(()) => {
                app.reload_templates();
                app.success(format!("deleted template '{name}'"));
            }
            Err(e) => app.error(format!("could not delete it: {e:#}")),
        },
    }
}

fn ask(app: &mut App, confirm: Confirm) {
    if app.config.ui.confirm_destructive {
        app.confirm = Some(confirm);
    } else {
        run_action(app, confirm.action);
    }
}

// ---------------------------------------------------------------- text entry

/// Route a key into whichever text field is active. `None` means no field is active;
/// `Some(true)` means the key was consumed.
fn text_entry(app: &mut App, key: KeyEvent) -> Option<bool> {
    let field = active_field(app)?;

    match key.code {
        KeyCode::Esc => {
            close_field(app, false);
            return Some(true);
        }
        KeyCode::Enter => {
            close_field(app, true);
            return Some(true);
        }
        _ => {}
    }

    let input = match field {
        Field::ModelFilter => &mut app.models_view.filter,
        Field::HubQuery => &mut app.hub_view.query,
        Field::ServeValue => &mut app.serve_view.editor,
        Field::ProfileName => &mut app.serve_view.profile_name,
        Field::LogFilter => &mut app.logs_view.filter,
        Field::TemplateRepo => &mut app.templates_view.repo,
    };

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('w') => input.delete_word(),
            KeyCode::Char('u') => input.clear(),
            KeyCode::Char('a') => input.home(),
            KeyCode::Char('e') => input.end(),
            KeyCode::Char('k') => {
                while input.cursor < input.value.chars().count() {
                    input.delete();
                }
            }
            _ => {}
        }
        return Some(true);
    }

    match key.code {
        KeyCode::Char(c) => input.insert(c),
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete(),
        KeyCode::Left => input.left(),
        KeyCode::Right => input.right(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        _ => return Some(false),
    }

    // The model filter is live, so the cursor must not point past the shortened list.
    if field == Field::ModelFilter {
        let n = app.filtered_models().len();
        app.models_view.sel.clamp(n);
    }
    Some(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    ModelFilter,
    HubQuery,
    ServeValue,
    ProfileName,
    LogFilter,
    TemplateRepo,
}

fn active_field(app: &App) -> Option<Field> {
    if app.models_view.filtering {
        return Some(Field::ModelFilter);
    }
    if app.hub_view.editing {
        return Some(Field::HubQuery);
    }
    if app.serve_view.editing {
        return Some(Field::ServeValue);
    }
    if app.serve_view.naming {
        return Some(Field::ProfileName);
    }
    if app.logs_view.filtering {
        return Some(Field::LogFilter);
    }
    if app.templates_view.editing_repo {
        return Some(Field::TemplateRepo);
    }
    None
}

fn close_field(app: &mut App, commit: bool) {
    match active_field(app) {
        Some(Field::ModelFilter) => {
            app.models_view.filtering = false;
            if !commit {
                app.models_view.filter.clear();
            }
            app.models_view.sel.clamp(app.filtered_models().len());
        }
        Some(Field::HubQuery) => {
            app.hub_view.editing = false;
            if commit && !app.hub_view.query.is_empty() {
                start_search(app);
            }
        }
        Some(Field::ServeValue) => {
            app.serve_view.editing = false;
            if commit {
                commit_knob_edit(app);
            }
        }
        Some(Field::ProfileName) => {
            app.serve_view.naming = false;
            if commit {
                save_profile(app);
            }
        }
        Some(Field::LogFilter) => {
            app.logs_view.filtering = false;
            if !commit {
                app.logs_view.filter.clear();
            }
        }
        Some(Field::TemplateRepo) => {
            app.templates_view.editing_repo = false;
            if commit {
                list_template_repo(app);
            }
        }
        None => {}
    }
}

// ---------------------------------------------------------------- global

fn global_key(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('?') | KeyCode::F(1) => {
            app.show_help = true;
            true
        }
        KeyCode::Char('q') => {
            if app.engine.is_live() && app.engine.state != crate::ft::EngineState::Adopted {
                ask(
                    app,
                    Confirm::new(
                        "Quit ft-man",
                        vec![
                            "The engine ft-man started is still running.".into(),
                            String::new(),
                            "Quitting leaves it running and detached; it keeps serving, and a \
                             later ft-man run will re-attach to it."
                                .into(),
                        ],
                        ConfirmAction::Quit,
                        false,
                    ),
                );
            } else {
                app.should_quit = true;
            }
            true
        }
        KeyCode::Tab if !tab_is_local(app) => {
            app.tab = app.tab.next();
            true
        }
        KeyCode::BackTab => {
            app.tab = app.tab.prev();
            true
        }
        KeyCode::Char(c @ '1'..='9') => {
            if let Some(t) = Tab::from_digit(c.to_digit(10).unwrap()) {
                app.tab = t;
            }
            true
        }
        _ => false,
    }
}

/// Tabs where `Tab` moves focus within the view instead of switching views.
fn tab_is_local(app: &App) -> bool {
    matches!(app.tab, Tab::Hub | Tab::Templates | Tab::Serve | Tab::Jobs)
}

// ---------------------------------------------------------------- dashboard

fn dashboard_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('e') => start_engine(app),
        KeyCode::Char('s') => request_stop(app, false),
        KeyCode::Char('S') => request_stop(app, true),
        KeyCode::Char('r') => app.request_scan(),
        KeyCode::Char('t') => smoke_test(app),
        _ => {}
    }
}

fn request_stop(app: &mut App, force: bool) {
    if !app.engine.is_live() {
        app.warn("no engine is running");
        return;
    }
    let adopted = app.engine.state == crate::ft::EngineState::Adopted;
    let model = app.current_model().unwrap_or_else(|| "the model".into());
    ask(
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
    );
}

fn smoke_test(app: &mut App) {
    if !app.server_reachable() {
        app.warn("the server is not answering");
        return;
    }
    let client = app.client.clone();
    let tx = app.tx.clone();
    app.info("running a /generate smoke test…");
    tokio::spawn(async move {
        let res =
            client.generate("The capital of France is", 16).await.map_err(|e| format!("{e:#}"));
        let _ = tx.send(Message::SmokeTest(res));
    });
}

// ---------------------------------------------------------------- models

fn models_key(app: &mut App, key: KeyEvent) {
    let len = app.filtered_models().len();
    match key.code {
        KeyCode::Char('/') => {
            app.models_view.filtering = true;
        }
        KeyCode::Up | KeyCode::Char('k') => app.models_view.sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.models_view.sel.down(len),
        KeyCode::PageUp => app.models_view.sel.page_up(10),
        KeyCode::PageDown => app.models_view.sel.page_down(len, 10),
        KeyCode::Home => app.models_view.sel.first(),
        KeyCode::End => app.models_view.sel.last(len),
        KeyCode::Char('r') => app.request_scan(),
        KeyCode::Enter => use_selected_model(app, false),
        KeyCode::Char('s') => use_selected_model(app, true),
        KeyCode::Char('c') => convert_selected(app),
        KeyCode::Char('D') => delete_selected_model(app),
        _ => {}
    }
}

/// Load the highlighted model into the Serve configuration, optionally starting it.
fn use_selected_model(app: &mut App, and_serve: bool) {
    let Some(model) = app.selected_model() else { return };
    if model.is_partial() {
        app.warn(format!(
            "{} is an incomplete conversion and cannot be served; delete it with D",
            model.name
        ));
        return;
    }
    // An FTW build loads meaningfully faster, so prefer it when one exists.
    let (path, note) = match &model.converted_to {
        Some(ftw) => (ftw.clone(), Some(format!("using the FTW build at {}", ftw.display()))),
        None => (model.path.clone(), None),
    };
    let name = model.name.clone();
    // Explicit, never inferred. See `Model::served_name`.
    let served = model.served_name();
    app.serve.set("model", path.display().to_string());
    app.serve.set("served_model_name", served);
    if let Some(n) = note {
        app.info(n);
    }
    if and_serve {
        start_engine(app);
    } else {
        app.tab = Tab::Serve;
        app.success(format!("{name} loaded into the Serve configuration"));
    }
}

fn convert_selected(app: &mut App) {
    let Some(model) = app.selected_model() else { return };
    if model.format != Format::Hf {
        app.warn(format!(
            "{} is already in {} format; conversion only applies to Hugging Face checkpoints",
            model.name,
            model.format.label()
        ));
        return;
    }
    if let Some(reason) = app.gpu_busy_reason() {
        app.warn(format!("cannot convert: {reason}"));
        return;
    }
    if app.convert_checking.is_some() {
        app.warn("a checkpoint check is already running");
        return;
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
        if leftovers {
            let size = crate::models::dir_size(&out);
            ask(
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
            );
        } else {
            app.warn(format!(
                "{} already exists; delete it from the Models tab to reconvert",
                out.display()
            ));
        }
        return;
    }

    begin_conversion(app, &source);
}

/// Ask FreeToken what it makes of the checkpoint, then convert.
///
/// The check is cheap and the job is not: a conversion that cannot read the experts
/// still spends minutes writing most of the model to disk before it finds out.
fn begin_conversion(app: &mut App, source: &std::path::Path) {
    if !app.config.convert.preflight {
        start_conversion(app, source);
        return;
    }
    let Some(ft) = app.ft.clone() else {
        app.error("the FreeToken CLI was not found");
        return;
    };
    let moe_backend = convert_moe_backend(app);
    let Some(argv) = crate::ft::preflight::convert_command(&ft, source, moe_backend) else {
        // No interpreter to check with is not a reason to refuse the conversion.
        start_conversion(app, source);
        return;
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
}

/// Act on a conversion preflight: start silently when it is clean, explain and ask when
/// it is not.
pub fn on_convert_preflight(
    app: &mut App,
    source: std::path::PathBuf,
    outcome: crate::ft::Preflight,
) {
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
    match app.serve.get("moe_backend") {
        Some("fused") => "triton",
        _ => "offload",
    }
}

/// Where the FTW build of `source` goes, resolving the checkpoint's repo id out of the
/// library so one that came from the Hugging Face cache gets an org-qualified name.
fn ftw_out(app: &App, source: &std::path::Path) -> std::path::PathBuf {
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
fn start_conversion(app: &mut App, source: &std::path::Path) {
    let Some(ft) = app.ft.clone() else {
        app.error("the FreeToken CLI was not found");
        return;
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
            app.jobs.push(job);
            app.jobs_view.sel.last(views::jobs::rows(app).len());
            app.tab = Tab::Jobs;
            app.info(format!("converting {name} to {}", out.display()));
        }
        Err(e) => app.error(format!("could not start the conversion: {e:#}")),
    }
}

fn delete_selected_model(app: &mut App) {
    let Some(model) = app.selected_model() else { return };
    let path = model.path.clone();
    let name = model.name.clone();
    let size = crate::models::dir_size(&path);
    ask(
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
    );
}

// ---------------------------------------------------------------- hub

fn hub_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('/') => app.hub_view.editing = true,
        KeyCode::Tab => {
            if !app.hub_view.files.is_empty() {
                app.hub_view.focus = match app.hub_view.focus {
                    HubFocus::Results if app.hub_view.layout.is_multi() => HubFocus::Variants,
                    HubFocus::Results => HubFocus::Files,
                    HubFocus::Variants => HubFocus::Files,
                    HubFocus::Files => HubFocus::Results,
                };
            }
        }
        KeyCode::Char('i') if app.hf_cli.is_none() && !app.hf_installing => offer_hf_install(app),
        KeyCode::Esc if app.hub_view.focus != HubFocus::Results => {
            app.hub_view.focus = HubFocus::Results
        }
        _ if app.hub_view.focus == HubFocus::Variants => hub_variants_key(app, key),
        _ if app.hub_view.focus == HubFocus::Files => hub_files_key(app, key),
        _ => hub_results_key(app, key),
    }
}

fn hub_results_key(app: &mut App, key: KeyEvent) {
    let len = app.hub_view.results.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.hub_view.sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.hub_view.sel.down(len),
        KeyCode::PageUp => app.hub_view.sel.page_up(5),
        KeyCode::PageDown => app.hub_view.sel.page_down(len, 5),
        KeyCode::Home => app.hub_view.sel.first(),
        KeyCode::End => app.hub_view.sel.last(len),
        KeyCode::Enter => load_repo_files(app),
        KeyCode::Char('d') => begin_download(app),
        _ => {}
    }
}

/// Offer Hugging Face's own installer for the missing `hf` CLI.
fn offer_hf_install(app: &mut App) {
    ask(
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
    );
}

/// Choosing a quantization — the one interaction this tab really exists for.
fn hub_variants_key(app: &mut App, key: KeyEvent) {
    let len = app.hub_view.layout.weights().count();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.hub_view.variant_sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.hub_view.variant_sel.down(len),
        KeyCode::PageUp => app.hub_view.variant_sel.page_up(10),
        KeyCode::PageDown => app.hub_view.variant_sel.page_down(len, 10),
        KeyCode::Home => app.hub_view.variant_sel.first(),
        KeyCode::End => app.hub_view.variant_sel.last(len),
        KeyCode::Enter | KeyCode::Char(' ') => choose_variant(app),
        KeyCode::Char('d') => {
            // `d` on a highlighted row means that row. Nobody presses download expecting
            // the quantization under the cursor to be the one left out.
            if app.hub_view.variant.is_none() {
                choose_variant(app);
            }
            begin_download(app);
        }
        _ => {}
    }
}

/// Apply the highlighted quantization to the file selection.
fn choose_variant(app: &mut App) {
    let Some(label) =
        app.hub_view.layout.weights().nth(app.hub_view.variant_sel.index).map(|v| v.label.clone())
    else {
        return;
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
}

fn hub_files_key(app: &mut App, key: KeyEvent) {
    let len = app.hub_view.files.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.hub_view.file_sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.hub_view.file_sel.down(len),
        KeyCode::PageUp => app.hub_view.file_sel.page_up(10),
        KeyCode::PageDown => app.hub_view.file_sel.page_down(len, 10),
        KeyCode::Home => app.hub_view.file_sel.first(),
        KeyCode::End => app.hub_view.file_sel.last(len),
        KeyCode::Char(' ') => {
            if let Some(f) = app.hub_view.files.get_mut(app.hub_view.file_sel.index) {
                f.wanted = !f.wanted;
                // The selection no longer is a quantization, so nothing downstream may go
                // on claiming it is one.
                app.hub_view.custom_selection = true;
            }
        }
        KeyCode::Char('a') => {
            app.hub_view.files.iter_mut().for_each(|f| f.wanted = true);
            app.hub_view.custom_selection = true;
        }
        KeyCode::Char('n') => {
            app.hub_view.files.iter_mut().for_each(|f| f.wanted = false);
            app.hub_view.custom_selection = true;
        }
        KeyCode::Char('d') => begin_download(app),
        _ => {}
    }
}

#[cfg(test)]
pub fn hub_client_for_tests(app: &App) -> Result<Hub, String> {
    hub_client(app)
}

fn hub_client(app: &App) -> Result<Hub, String> {
    Hub::new(&app.config.hub.endpoint, app.hub_token.as_ref().map(|t| t.value.clone()))
        .map_err(|e| format!("{e:#}"))
}

fn start_search(app: &mut App) {
    let query = app.hub_view.query.value.trim().to_string();
    if query.is_empty() {
        return;
    }
    let hub = match hub_client(app) {
        Ok(h) => h,
        Err(e) => {
            app.error(e);
            return;
        }
    };
    app.hub_view.searching = true;
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let res = hub.search(&query, 50).await.map_err(|e| format!("{e:#}"));
        let _ = tx.send(Message::HubSearch(res));
    });
}

fn load_repo_files(app: &mut App) {
    let Some(repo) = app.hub_view.results.get(app.hub_view.sel.index) else { return };
    let repo_id = repo.id.clone();
    let gated = repo.is_gated();
    let hub = match hub_client(app) {
        Ok(h) => h,
        Err(e) => {
            app.error(e);
            return;
        }
    };
    if gated && !hub.has_token() {
        app.warn(format!(
            "{repo_id} is gated — set HF_TOKEN or hub.token and accept its terms on the Hub first"
        ));
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

fn begin_download(app: &mut App) {
    let Some(info) = app.hub_view.info.as_ref() else {
        app.warn("select a repo and press Enter to list its files first");
        return;
    };
    let selected: Vec<_> = app.hub_view.files.iter().filter(|f| f.wanted).cloned().collect();
    if selected.is_empty() {
        app.warn("no files selected");
        return;
    }
    let repo = info.id.clone();
    if app.downloads.iter().any(|d| d.is_running() && d.repo == repo) {
        app.warn(format!("{repo} is already downloading"));
        return;
    }

    let hub = match hub_client(app) {
        Ok(h) => h,
        Err(e) => {
            app.error(e);
            return;
        }
    };
    let Some(cli) = app.hf_cli.clone() else {
        app.error("the hf CLI is not installed — press i on the Hub tab to install it");
        return;
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
}

// ---------------------------------------------------------------- templates

fn templates_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('r') => app.templates_view.editing_repo = true,
        KeyCode::Tab => {
            app.templates_view.pane = match app.templates_view.pane {
                TemplatePane::Store => TemplatePane::Remote,
                TemplatePane::Remote => TemplatePane::Store,
            };
        }
        KeyCode::Char('a') => apply_template(app),
        KeyCode::Char('u') => request_revert_template(app),
        KeyCode::Char('v') => verify_template(app),
        _ if app.templates_view.pane == TemplatePane::Remote => remote_templates_key(app, key),
        _ => stored_templates_key(app, key),
    }
}

fn stored_templates_key(app: &mut App, key: KeyEvent) {
    let len = app.templates_view.stored.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.templates_view.sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.templates_view.sel.down(len),
        KeyCode::PageUp => app.templates_view.sel.page_up(5),
        KeyCode::PageDown => app.templates_view.sel.page_down(len, 5),
        KeyCode::Home => app.templates_view.sel.first(),
        KeyCode::End => app.templates_view.sel.last(len),
        KeyCode::Char('D') => {
            let Some(t) = app.selected_template() else { return };
            let name = t.name.clone();
            ask(
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
            );
            return;
        }
        _ => return,
    }
    // The preview follows the cursor.
    app.refresh_template_preview();
}

fn remote_templates_key(app: &mut App, key: KeyEvent) {
    let len = app.templates_view.remote.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.templates_view.remote_sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.templates_view.remote_sel.down(len),
        KeyCode::PageUp => app.templates_view.remote_sel.page_up(10),
        KeyCode::PageDown => app.templates_view.remote_sel.page_down(len, 10),
        KeyCode::Home => app.templates_view.remote_sel.first(),
        KeyCode::End => app.templates_view.remote_sel.last(len),
        KeyCode::Enter | KeyCode::Char('f') => fetch_template(app),
        _ => {}
    }
}

/// List the `.jinja` files in the repo named in the repo field.
fn list_template_repo(app: &mut App) {
    let repo = app.templates_view.repo.value.trim().to_string();
    if repo.is_empty() {
        app.warn("enter a Hugging Face repo id first");
        return;
    }
    let hub = match hub_client(app) {
        Ok(h) => h,
        Err(e) => {
            app.error(e);
            return;
        }
    };
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
}

/// Download the highlighted repo file into the local store.
fn fetch_template(app: &mut App) {
    let Some(file) = app.templates_view.remote.get(app.templates_view.remote_sel.index).cloned()
    else {
        return;
    };
    let (Some(repo), Some(revision)) =
        (app.templates_view.remote_repo.clone(), app.templates_view.remote_revision.clone())
    else {
        return;
    };
    let hub = match hub_client(app) {
        Ok(h) => h,
        Err(e) => {
            app.error(e);
            return;
        }
    };
    let name = crate::templates::name_for(&repo, &file.path);
    app.templates_view.loading = true;
    app.info(format!("fetching {}", file.path));
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let res = async {
            let jinja =
                hub.fetch_text(&repo, &revision, &file.path).await.map_err(|e| format!("{e:#}"))?;
            let meta = crate::templates::TemplateMeta {
                source: Some(repo.clone()),
                revision: Some(revision.clone()),
                repo_path: Some(file.path.clone()),
                ..Default::default()
            };
            crate::templates::save(&name, &jinja, meta)
                .map(|t| t.name)
                .map_err(|e| format!("{e:#}"))
        }
        .await;
        let _ = tx.send(Message::TemplateFetched(res));
    });
}

/// Ask before writing a template into a checkpoint directory.
fn apply_template(app: &mut App) {
    let Some(template) = app.selected_template() else {
        app.warn("no template selected");
        return;
    };
    let name = template.name.clone();
    let version = template.meta.version.clone();

    let Some(model) = app.selected_model() else {
        app.warn("no model selected — pick one on the Models tab first");
        return;
    };
    let model_name = model.name.clone();
    let model_path = model.path.clone();
    let targets = crate::templates::targets(model);
    if targets.is_empty() {
        app.error(format!("{model_name} has no directory to write a template into"));
        return;
    }
    let status = app.template_status(model);

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

    ask(
        app,
        Confirm::new(
            "Apply chat template",
            body,
            ConfirmAction::ApplyTemplate { template: name, model: model_path },
            false,
        ),
    );
}

/// Write the template, optionally after a real render check.
fn write_template(app: &mut App, template_name: &str, model_path: &std::path::Path) {
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

fn request_revert_template(app: &mut App) {
    let Some(model) = app.selected_model() else {
        app.warn("no model selected");
        return;
    };
    let status = app.template_status(model);
    if !status.is_overridden() {
        app.warn(format!("{} is not using an ft-man template override", model.name));
        return;
    }
    let name = model.name.clone();
    let path = model.path.clone();
    let targets = crate::templates::targets(model);
    let mut body = vec![
        format!("Restore {name}'s own chat template?"),
        String::new(),
        "This reverses the override in:".into(),
    ];
    for t in &targets {
        body.push(format!("    {}", t.display()));
    }
    ask(
        app,
        Confirm::new("Restore built-in template", body, ConfirmAction::RevertTemplate(path), false),
    );
}

fn revert_template(app: &mut App, model_path: &std::path::Path) {
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

/// Render the selected template against the selected model's real tokenizer.
fn verify_template(app: &mut App) {
    let Some(template) = app.selected_template() else {
        app.warn("no template selected");
        return;
    };
    let (name, path) = (template.name.clone(), template.path.clone());
    let Some(model) = app.selected_model() else {
        app.warn("no model selected — the check needs a tokenizer to render against");
        return;
    };
    let model_path = model.path.clone();
    run_preflight(app, &name, &model_path, &path);
}

/// Spawn the template render check. It needs FreeToken's Python (for transformers), so
/// it is a no-op with a clear message when that is not available.
fn run_preflight(app: &mut App, name: &str, model_dir: &std::path::Path, jinja: &std::path::Path) {
    let Some(ft) = app.ft.clone() else {
        app.warn("cannot verify the template without the FreeToken CLI");
        return;
    };
    let Some(argv) = crate::ft::preflight::template_command(&ft, model_dir, jinja) else {
        app.warn("cannot verify the template: no Python found beside the FreeToken CLI");
        return;
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
}

// ---------------------------------------------------------------- serve

fn serve_key(app: &mut App, key: KeyEvent) {
    if app.serve_view.in_profiles {
        match key.code {
            KeyCode::Tab | KeyCode::Esc => {
                app.serve_view.in_profiles = false;
                return;
            }
            _ => {
                profiles_key(app, key);
                return;
            }
        }
    }

    let items: Vec<&Knob> = knobs_in(app.serve_view.group).collect();
    let len = items.len();
    match key.code {
        KeyCode::Tab => app.serve_view.in_profiles = true,
        KeyCode::Up | KeyCode::Char('k') => app.serve_view.sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.serve_view.sel.down(len),
        KeyCode::Left | KeyCode::Char('h') => {
            app.serve_view.group = prev_group(app.serve_view.group);
            app.serve_view.sel.first();
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.serve_view.group = next_group(app.serve_view.group);
            app.serve_view.sel.first();
        }
        KeyCode::Home => app.serve_view.sel.first(),
        KeyCode::End => app.serve_view.sel.last(len),
        KeyCode::Enter => begin_knob_edit(app),
        KeyCode::Char(' ') => cycle_knob(app, 1),
        KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
            if let Some(k) = items.get(app.serve_view.sel.index) {
                let key_name = k.key;
                let label = k.label;
                if app.serve.is_set(key_name) {
                    app.serve.unset(key_name);
                    app.info(format!("{label} reset to its default"));
                }
            }
        }
        KeyCode::Char('p') => app.serve_view.show_preview = !app.serve_view.show_preview,
        KeyCode::Char('S') => {
            app.serve_view.profile_name.set(suggested_profile_name(app));
            app.serve_view.naming = true;
        }
        KeyCode::Char('P') => load_profile(app),
        KeyCode::Char('a') => build_plan(app),
        KeyCode::Char('g') => start_engine(app),
        _ => {}
    }
}

/// Work out what this hardware would prefer, and show it before changing anything.
///
/// Planning is pure and instant — every input is already in memory — so this needs no
/// job, no spinner and no confirmation. Applying it does need a deliberate second key,
/// which is why the overlay holds the keyboard until one arrives.
fn build_plan(app: &mut App) {
    match crate::ui::views::plan::build(app) {
        Ok(plan) => {
            if plan.is_empty() && plan.unpriced.is_none() {
                app.success("nothing to change — this configuration is already optimal here");
                return;
            }
            app.serve_view.plan = Some(plan);
        }
        Err(why) => app.warn(format!("cannot plan: {why}")),
    }
}

/// Fold the plan's edits into the serve configuration.
fn apply_plan(app: &mut App) {
    let Some(plan) = app.serve_view.plan.take() else { return };
    match plan.apply(&mut app.serve) {
        0 => app.info("the configuration already matched the plan"),
        n => app.success(format!(
            "applied {n} change{} — press g to serve with it",
            if n == 1 { "" } else { "s" }
        )),
    }
}

fn profiles_key(app: &mut App, key: KeyEvent) {
    let len = app.profiles.items.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.serve_view.profile_sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.serve_view.profile_sel.down(len),
        KeyCode::Home => app.serve_view.profile_sel.first(),
        KeyCode::End => app.serve_view.profile_sel.last(len),
        KeyCode::Enter | KeyCode::Char('P') => load_profile(app),
        KeyCode::Char('D') => {
            let Some(p) = app.profiles.items.get(app.serve_view.profile_sel.index) else { return };
            let name = p.name.clone();
            ask(
                app,
                Confirm::new(
                    "Delete profile",
                    vec![format!("Delete the saved profile '{name}'?")],
                    ConfirmAction::DeleteProfile(name),
                    true,
                ),
            );
        }
        _ => {}
    }
}

fn prev_group(g: crate::knobs::Group) -> crate::knobs::Group {
    let all = crate::knobs::Group::ALL;
    let i = all.iter().position(|x| *x == g).unwrap_or(0);
    all[(i + all.len() - 1) % all.len()]
}

fn next_group(g: crate::knobs::Group) -> crate::knobs::Group {
    let all = crate::knobs::Group::ALL;
    let i = all.iter().position(|x| *x == g).unwrap_or(0);
    all[(i + 1) % all.len()]
}

fn selected_knob(app: &App) -> Option<&'static Knob> {
    knobs_in(app.serve_view.group).nth(app.serve_view.sel.index)
}

fn begin_knob_edit(app: &mut App) {
    let Some(k) = selected_knob(app) else { return };
    match k.kind {
        // A flag has nothing to type, so Enter just toggles it.
        Kind::Flag => {
            app.serve.toggle_flag(k.key);
            let state = if app.serve.flag(k.key) { "on" } else { "off" };
            app.info(format!("{} {state}", k.label));
        }
        Kind::Choice(_) => cycle_knob(app, 1),
        _ => {
            app.serve_view.editor =
                crate::ui::widgets::TextInput::new(app.serve.get(k.key).unwrap_or_default());
            app.serve_view.editing = true;
        }
    }
}

fn cycle_knob(app: &mut App, delta: isize) {
    let Some(k) = selected_knob(app) else { return };
    match k.kind {
        Kind::Flag => {
            app.serve.toggle_flag(k.key);
        }
        Kind::Choice(options) => {
            if options.is_empty() {
                return;
            }
            // Cycling walks options and then wraps through "unset", so there is always a
            // way back to the default without reaching for another key.
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
                Some(i) => app.serve.set(k.key, options[i]),
                None => app.serve.unset(k.key),
            }
        }
        _ => begin_knob_edit(app),
    }
}

fn commit_knob_edit(app: &mut App) {
    let Some(k) = selected_knob(app) else { return };
    let value = app.serve_view.editor.value.trim().to_string();
    if value.is_empty() {
        app.serve.unset(k.key);
        return;
    }
    if let Some(msg) = crate::knobs::validate_value(k, &value) {
        app.error(format!("{}: {msg}", k.flag));
        return;
    }
    app.serve.set(k.key, value);
}

fn suggested_profile_name(app: &App) -> String {
    app.serve
        .get("model")
        .map(|m| {
            std::path::Path::new(m)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| m.to_string())
        })
        .unwrap_or_else(|| "profile".into())
}

fn save_profile(app: &mut App) {
    let name = app.serve_view.profile_name.value.trim().to_string();
    if name.is_empty() {
        app.warn("a profile needs a name");
        return;
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
}

fn load_profile(app: &mut App) {
    let Some(p) = app.profiles.items.get(app.serve_view.profile_sel.index) else {
        app.warn("no profile selected");
        return;
    };
    app.serve = p.serve.clone();
    let name = p.name.clone();
    app.profiles.last_used = Some(name.clone());
    save_profiles(app);
    app.success(format!("loaded profile '{name}'"));
}

fn save_profiles(app: &mut App) {
    if let Err(e) = app.profiles.save() {
        app.error(format!("could not save profiles: {e:#}"));
    }
}

// ---------------------------------------------------------------- engine launch

pub fn start_engine(app: &mut App) {
    if app.engine.is_live() {
        app.warn("an engine is already running; stop it first");
        return;
    }
    if let Some(job) = app.jobs.iter().find(|j| j.is_running()) {
        app.warn(format!(
            "a {} job is using the GPU; wait for it or cancel it first",
            job.kind.label()
        ));
        return;
    }
    let Some(ft) = app.ft.clone() else {
        app.error(app.ft_error.clone().unwrap_or_else(|| "the FreeToken CLI was not found".into()));
        return;
    };

    let errors = app.serve.validate();
    if !errors.is_empty() {
        let (key, msg) = &errors[0];
        let flag = crate::knobs::knob(key).map(|k| k.flag).unwrap_or(key);
        app.error(format!("{flag}: {msg}"));
        app.tab = Tab::Serve;
        return;
    }

    let model = app.serve.get("model").unwrap_or_default().to_string();
    let port = app.serve.get("port").and_then(|p| p.parse().ok()).unwrap_or(app.config.server.port);
    let args = app.serve.to_args();

    match app.engine.start(&ft, args, &app.config.freetoken.env, model.clone(), port) {
        Ok(path) => {
            app.telemetry = Telemetry::default();
            app.series = crate::ui::app::Series::default();
            app.requests_view.entries.clear();
            app.requests_view.cursor = 0;
            app.tab = Tab::Logs;
            app.logs_view.follow = true;
            app.logs_view.scroll = 0;
            app.info(format!("starting {model}; logging to {}", path.display()));
        }
        Err(e) => app.error(format!("could not start the engine: {e:#}")),
    }
}

// ---------------------------------------------------------------- cache

fn cache_key(app: &mut App, key: KeyEvent) {
    let Some(geo) = app.telemetry.cache.as_ref().map(|c| c.geometry.clone()) else { return };
    let pools: Vec<Pool> =
        Pool::ALL.iter().copied().filter(|p| views::cache::pool_present(&geo, *p)).collect();
    if pools.is_empty() {
        return;
    }
    app.cache_view.sel.clamp(pools.len());
    let pool = pools[app.cache_view.sel.index];
    let big = key.modifiers.contains(KeyModifiers::SHIFT);
    let step = if big { 0.10 } else { 0.01 };

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.cache_view.sel.up(pools.len()),
        KeyCode::Down | KeyCode::Char('j') => app.cache_view.sel.down(pools.len()),
        KeyCode::Left | KeyCode::Char('h') => views::cache::adjust(app, pool, -step),
        KeyCode::Right | KeyCode::Char('l') => views::cache::adjust(app, pool, step),
        KeyCode::Char('r') => app.cache_view.set_pending(pool, None),
        KeyCode::Char('R') => app.cache_view.clear_pending(),
        KeyCode::Char('a') | KeyCode::Enter => {
            if !app.cache_view.has_pending() {
                app.warn("nothing to apply");
                return;
            }
            let active = app.telemetry.stats.as_ref().map(|s| s.requests.active).unwrap_or(0);
            let mut body =
                vec!["Resize the cache pools on the running engine?".to_string(), String::new()];
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
            ask(app, Confirm::new("Rebuild cache", body, ConfirmAction::ApplyCacheRebuild, false));
        }
        _ => {}
    }
}

fn apply_cache_rebuild(app: &mut App) {
    let req = rebuild_from_pending(&app.cache_view);
    if req.is_empty() {
        return;
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
}

// ---------------------------------------------------------------- jobs

fn jobs_key(app: &mut App, key: KeyEvent) {
    let items = views::jobs::rows(app);
    let len = items.len();
    match key.code {
        KeyCode::Tab => app.jobs_view.in_output = !app.jobs_view.in_output,
        KeyCode::Char('b') => run_bench(app),
        _ if app.jobs_view.in_output => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                app.jobs_view.output_scroll = app.jobs_view.output_scroll.saturating_add(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.jobs_view.output_scroll = app.jobs_view.output_scroll.saturating_sub(1)
            }
            KeyCode::PageUp => {
                app.jobs_view.output_scroll = app.jobs_view.output_scroll.saturating_add(10)
            }
            KeyCode::PageDown => {
                app.jobs_view.output_scroll = app.jobs_view.output_scroll.saturating_sub(10)
            }
            KeyCode::End | KeyCode::Char('G') => app.jobs_view.output_scroll = 0,
            KeyCode::Esc => app.jobs_view.in_output = false,
            _ => {}
        },
        KeyCode::Up | KeyCode::Char('k') => {
            app.jobs_view.sel.up(len);
            app.jobs_view.output_scroll = 0;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.jobs_view.sel.down(len);
            app.jobs_view.output_scroll = 0;
        }
        KeyCode::Home => app.jobs_view.sel.first(),
        KeyCode::End => app.jobs_view.sel.last(len),
        KeyCode::Char('x') => cancel_selected(app),
        KeyCode::Char('X') => {
            let before = app.jobs.len() + app.downloads.len();
            app.jobs.retain(|j| j.is_running());
            app.downloads.retain(|d| d.is_running());
            let removed = before - (app.jobs.len() + app.downloads.len());
            app.jobs_view.sel.clamp(views::jobs::rows(app).len());
            if removed > 0 {
                app.info(format!("cleared {removed} finished entr(ies)"));
            }
        }
        _ => {}
    }
}

fn cancel_selected(app: &mut App) {
    let items = views::jobs::rows(app);
    let Some(row) = items.get(app.jobs_view.sel.index).copied() else { return };
    match row {
        views::jobs::Row::Job(i) => {
            let job = &app.jobs[i];
            if !job.is_running() {
                app.warn("that job has already finished");
                return;
            }
            let (id, title) = (job.id, job.title.clone());
            ask(
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
            );
        }
        views::jobs::Row::Download(i) => {
            let d = &app.downloads[i];
            if !d.is_running() {
                app.warn("that download has already finished");
                return;
            }
            let (id, repo) = (d.id, d.repo.clone());
            ask(
                app,
                Confirm::new(
                    "Cancel download",
                    vec![
                        format!("Cancel the download of {repo}?"),
                        String::new(),
                        "Completed files are kept and a partial file resumes where it stopped."
                            .into(),
                    ],
                    ConfirmAction::CancelDownload(id),
                    false,
                ),
            );
        }
    }
}

fn run_bench(app: &mut App) {
    if let Some(reason) = app.gpu_busy_reason() {
        app.warn(format!("cannot benchmark: {reason}"));
        return;
    }
    let Some(ft) = app.ft.clone() else {
        app.error("the FreeToken CLI was not found");
        return;
    };
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
            app.jobs.push(job);
            app.jobs_view.sel.last(views::jobs::rows(app).len());
            app.info("benchmarking CPU and PCIe bandwidth; this takes a few minutes");
        }
        Err(e) => app.error(format!("could not start the benchmark: {e:#}")),
    }
}

// ---------------------------------------------------------------- requests

fn requests_key(app: &mut App, key: KeyEvent) {
    let len = app.requests_view.entries.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.requests_view.sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.requests_view.sel.down(len),
        KeyCode::PageUp => app.requests_view.sel.page_up(10),
        KeyCode::PageDown => app.requests_view.sel.page_down(len, 10),
        KeyCode::Home => app.requests_view.sel.first(),
        KeyCode::End | KeyCode::Char('G') => app.requests_view.sel.last(len),
        KeyCode::Enter => app.requests_view.show_details = !app.requests_view.show_details,
        KeyCode::Char('f') => views::requests::follow_tail(app),
        KeyCode::Char('p') => {
            app.requests_view.paused = !app.requests_view.paused;
            let state = if app.requests_view.paused { "paused" } else { "resumed" };
            app.info(format!("request polling {state}"));
        }
        KeyCode::Char('c') => {
            app.requests_view.entries.clear();
            app.requests_view.sel.first();
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- logs

fn logs_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('/') => app.logs_view.filtering = true,
        KeyCode::Up | KeyCode::Char('k') => {
            app.logs_view.follow = false;
            app.logs_view.scroll += 1;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.logs_view.scroll = app.logs_view.scroll.saturating_sub(1);
            if app.logs_view.scroll == 0 {
                app.logs_view.follow = true;
            }
        }
        KeyCode::PageUp => {
            app.logs_view.follow = false;
            app.logs_view.scroll += 20;
        }
        KeyCode::PageDown => {
            app.logs_view.scroll = app.logs_view.scroll.saturating_sub(20);
            if app.logs_view.scroll == 0 {
                app.logs_view.follow = true;
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            app.logs_view.scroll = 0;
            app.logs_view.follow = true;
        }
        KeyCode::Home => {
            app.logs_view.follow = false;
            app.logs_view.scroll = usize::MAX / 2;
        }
        KeyCode::Char('f') => {
            app.logs_view.follow = !app.logs_view.follow;
            if app.logs_view.follow {
                app.logs_view.scroll = 0;
            }
        }
        KeyCode::Char('e') => app.logs_view.errors_only = !app.logs_view.errors_only,
        KeyCode::Char('w') => app.logs_view.wrap = !app.logs_view.wrap,
        KeyCode::Char('c') => {
            app.engine.log.clear();
            app.logs_view.scroll = 0;
        }
        _ => {}
    }
}
