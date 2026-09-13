//! Keyboard handling: which key means which action, and on which tab.
//!
//! Nothing here carries out an operation. Dispatch is layered — overlays first (a modal
//! owns the keyboard completely), then text-entry modes, then the active tab — and each
//! binding does one thing: resolve the selected row or the editor buffer to an identity,
//! and hand it to [`crate::actions`], which the web server calls with the same identity.
//! That is what keeps one implementation of "convert this checkpoint" rather than two.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::actions;
use crate::knobs::{knobs_in, Kind, Knob};
use crate::ui::app::{App, HubFocus, Pool, Tab, TemplatePane};
use crate::ui::views;
use crate::ui::widgets::{Confirm, ConfirmAction};

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
            KeyCode::Char('A') => {
                let _ = actions::apply_plan(app);
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('a') => actions::dismiss_plan(app),
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
            actions::run_action(app, action);
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.confirm = None,
        KeyCode::Enter => {
            let accepted = confirm.accepted();
            let action = confirm.action.clone();
            app.confirm = None;
            if accepted {
                actions::run_action(app, action);
            }
        }
        _ => {}
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
                let query = app.hub_view.query.value.clone();
                let _ = actions::search(app, &query);
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
                let name = app.serve_view.profile_name.value.clone();
                // A terminal has no inline slot under the field, so the refusal the shared
                // action leaves for its caller becomes a toast here.
                if let Err(refusal) = actions::save_profile(app, &name) {
                    app.warn(refusal.message);
                }
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
                let repo = app.templates_view.repo.value.clone();
                // As with the profile name: no inline slot, so the toast is raised here.
                if let Err(refusal) = actions::list_template_repo(app, &repo) {
                    app.warn(refusal.message);
                }
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
                actions::ask(
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
        KeyCode::Char('e') => {
            let _ = actions::start_engine(app);
        }
        KeyCode::Char('s') => {
            let _ = actions::request_stop(app, false);
        }
        KeyCode::Char('S') => {
            let _ = actions::request_stop(app, true);
        }
        KeyCode::Char('r') => app.request_scan(),
        KeyCode::Char('t') => {
            let _ = actions::smoke_test(app);
        }
        _ => {}
    }
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
        KeyCode::Enter => on_selected_model(app, |app, path| {
            let _ = actions::use_model(app, &path, false);
        }),
        KeyCode::Char('s') => on_selected_model(app, |app, path| {
            let _ = actions::use_model(app, &path, true);
        }),
        KeyCode::Char('c') => on_selected_model(app, |app, path| {
            let _ = actions::convert_model(app, &path);
        }),
        KeyCode::Char('D') => on_selected_model(app, |app, path| {
            let _ = actions::delete_model(app, &path);
        }),
        _ => {}
    }
}

/// Resolve the highlighted library row to the path every model action takes, and do
/// nothing at all when the list is empty.
fn on_selected_model(app: &mut App, f: impl FnOnce(&mut App, std::path::PathBuf)) {
    let Some(path) = app.selected_model().map(|m| m.path.clone()) else { return };
    f(app, path);
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
        KeyCode::Char('i') if app.hf_cli.is_none() && !app.hf_installing => {
            let _ = actions::offer_hf_install(app);
        }
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
        KeyCode::Enter => {
            let Some(repo) = app.hub_view.results.get(app.hub_view.sel.index) else { return };
            let repo_id = repo.id.clone();
            let _ = actions::open_repo(app, &repo_id, None);
        }
        KeyCode::Char('d') => {
            let _ = actions::download(app, None, None);
        }
        _ => {}
    }
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
        KeyCode::Enter | KeyCode::Char(' ') => choose_highlighted_variant(app),
        KeyCode::Char('d') => {
            // `d` on a highlighted row means that row. Nobody presses download expecting
            // the quantization under the cursor to be the one left out.
            //
            // Unless files have been ticked by hand: that selection is the answer, and
            // replacing it with a whole quantization would download something the reader
            // deliberately did not ask for.
            if app.hub_view.variant.is_none() && !app.hub_view.custom_selection {
                choose_highlighted_variant(app);
            }
            let _ = actions::download(app, None, None);
        }
        _ => {}
    }
}

fn choose_highlighted_variant(app: &mut App) {
    let Some(label) =
        app.hub_view.layout.weights().nth(app.hub_view.variant_sel.index).map(|v| v.label.clone())
    else {
        return;
    };
    let _ = actions::choose_variant(app, &label);
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
            let Some(path) =
                app.hub_view.files.get(app.hub_view.file_sel.index).map(|f| f.path.clone())
            else {
                return;
            };
            let _ = actions::toggle_file(app, &path, None);
        }
        KeyCode::Char('a') => {
            actions::select_files(app, true);
        }
        KeyCode::Char('n') => {
            actions::select_files(app, false);
        }
        KeyCode::Char('d') => {
            let _ = actions::download(app, None, None);
        }
        _ => {}
    }
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
        KeyCode::Char('a') => apply_selected_template(app),
        KeyCode::Char('u') => revert_selected_model(app),
        KeyCode::Char('v') => verify_selected_template(app),
        _ if app.templates_view.pane == TemplatePane::Remote => remote_templates_key(app, key),
        _ => stored_templates_key(app, key),
    }
}

/// The Templates tab's cross-tab dependency, resolved where it belongs: a terminal has
/// one cursor per list, so "apply this" means "to the model the Models tab is on".
fn apply_selected_template(app: &mut App) {
    let Some(name) = app.selected_template().map(|t| t.name.clone()) else {
        app.warn("no template selected");
        return;
    };
    let Some(path) = app.selected_model().map(|m| m.path.clone()) else {
        app.warn("no model selected — pick one on the Models tab first");
        return;
    };
    let _ = actions::apply_template(app, &name, &path);
}

fn revert_selected_model(app: &mut App) {
    let Some(path) = app.selected_model().map(|m| m.path.clone()) else {
        app.warn("no model selected");
        return;
    };
    let _ = actions::request_revert_template(app, &path);
}

fn verify_selected_template(app: &mut App) {
    let Some(name) = app.selected_template().map(|t| t.name.clone()) else {
        app.warn("no template selected");
        return;
    };
    let Some(path) = app.selected_model().map(|m| m.path.clone()) else {
        app.warn("no model selected — the check needs a tokenizer to render against");
        return;
    };
    let _ = actions::verify_template(app, &name, &path);
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
            let Some(name) = app.selected_template().map(|t| t.name.clone()) else { return };
            let _ = actions::delete_template(app, &name);
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
        KeyCode::Enter | KeyCode::Char('f') => fetch_highlighted_template(app),
        _ => {}
    }
}

fn fetch_highlighted_template(app: &mut App) {
    let Some(path) =
        app.templates_view.remote.get(app.templates_view.remote_sel.index).map(|f| f.path.clone())
    else {
        return;
    };
    let (Some(repo), Some(revision)) =
        (app.templates_view.remote_repo.clone(), app.templates_view.remote_revision.clone())
    else {
        return;
    };
    let _ = actions::fetch_template(app, &repo, Some(&revision), &path);
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
        KeyCode::Char(' ') => cycle_selected_knob(app, 1),
        KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace => {
            if let Some(k) = selected_knob(app) {
                actions::unset_knob(app, k, true);
            }
        }
        KeyCode::Char('p') => app.serve_view.show_preview = !app.serve_view.show_preview,
        KeyCode::Char('S') => {
            app.serve_view.profile_name.set(suggested_profile_name(app));
            app.serve_view.naming = true;
        }
        KeyCode::Char('P') => load_selected_profile(app),
        KeyCode::Char('a') => {
            let _ = actions::build_plan(app);
        }
        KeyCode::Char('g') => {
            let _ = actions::start_engine(app);
        }
        _ => {}
    }
}

fn profiles_key(app: &mut App, key: KeyEvent) {
    let len = app.profiles.items.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.serve_view.profile_sel.up(len),
        KeyCode::Down | KeyCode::Char('j') => app.serve_view.profile_sel.down(len),
        KeyCode::Home => app.serve_view.profile_sel.first(),
        KeyCode::End => app.serve_view.profile_sel.last(len),
        KeyCode::Enter | KeyCode::Char('P') => load_selected_profile(app),
        KeyCode::Char('D') => {
            let Some(name) =
                app.profiles.items.get(app.serve_view.profile_sel.index).map(|p| p.name.clone())
            else {
                return;
            };
            let _ = actions::delete_profile(app, &name);
        }
        _ => {}
    }
}

fn load_selected_profile(app: &mut App) {
    let Some(name) =
        app.profiles.items.get(app.serve_view.profile_sel.index).map(|p| p.name.clone())
    else {
        app.warn("no profile selected");
        return;
    };
    let _ = actions::load_profile(app, &name);
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
            let _ = actions::toggle_flag(app, k.key, None);
        }
        Kind::Choice(_) => cycle_selected_knob(app, 1),
        _ => {
            app.serve_view.editor =
                crate::ui::widgets::TextInput::new(app.serve.get(k.key).unwrap_or_default());
            app.serve_view.editing = true;
        }
    }
}

fn cycle_selected_knob(app: &mut App, delta: isize) {
    let Some(k) = selected_knob(app) else { return };
    match k.kind {
        Kind::Flag | Kind::Choice(_) => {
            let _ = actions::cycle_knob(app, k.key, delta);
        }
        _ => begin_knob_edit(app),
    }
}

fn commit_knob_edit(app: &mut App) {
    let Some(k) = selected_knob(app) else { return };
    let value = app.serve_view.editor.value.trim().to_string();
    if value.is_empty() {
        // Clearing the field is how a value is removed, and has never announced itself
        // the way `x` does.
        actions::unset_knob(app, k, false);
        return;
    }
    // A terminal has nowhere to put an inline error, so the refusal becomes a toast here
    // rather than inside the shared action, which the web UI renders against the field.
    if let Err(refusal) = actions::set_knob(app, k.key, Some(&value)) {
        app.error(refusal.message);
    }
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

// ---------------------------------------------------------------- cache

fn cache_key(app: &mut App, key: KeyEvent) {
    let Some(geo) = app.telemetry.cache.as_ref().map(|c| c.geometry.clone()) else { return };
    let pools: Vec<Pool> =
        Pool::ALL.iter().copied().filter(|p| crate::cache_pools::present(&geo, *p)).collect();
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
        KeyCode::Left | KeyCode::Char('h') => {
            let _ = actions::adjust_cache(app, pool, -step);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            let _ = actions::adjust_cache(app, pool, step);
        }
        KeyCode::Char('r') => app.cache_view.set_pending(pool, None),
        KeyCode::Char('R') => actions::reset_cache(app),
        KeyCode::Char('a') | KeyCode::Enter => {
            let _ = actions::apply_cache(app);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- jobs

fn jobs_key(app: &mut App, key: KeyEvent) {
    let items = views::jobs::rows(app);
    let len = items.len();
    match key.code {
        KeyCode::Tab => app.jobs_view.in_output = !app.jobs_view.in_output,
        KeyCode::Char('b') => {
            let _ = actions::run_bench(app);
        }
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
            actions::clear_finished(app);
        }
        _ => {}
    }
}

/// Jobs and downloads share one list here but not one id space, so the highlighted row
/// decides which of the two cancel actions is meant.
fn cancel_selected(app: &mut App) {
    let items = views::jobs::rows(app);
    let Some(row) = items.get(app.jobs_view.sel.index).copied() else { return };
    match row {
        views::jobs::Row::Job(i) => {
            let id = app.jobs[i].id;
            let _ = actions::cancel_job(app, id);
        }
        views::jobs::Row::Download(i) => {
            let id = app.downloads[i].id;
            let _ = actions::cancel_download(app, id);
        }
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
            let paused = !app.requests_view.paused;
            actions::set_requests_paused(app, paused);
        }
        KeyCode::Char('c') => actions::clear_requests(app),
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
        KeyCode::Char('c') => actions::clear_logs(app),
        _ => {}
    }
}
