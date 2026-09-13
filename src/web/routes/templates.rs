//! Chat templates: browsing a repo, fetching into the store, applying to a checkpoint.
//!
//! Apply and verify each carry both identities. In the TUI they mean "the template under
//! this cursor, to the model under that one" — a cross-tab dependency that exists only
//! because a terminal has one cursor per list. Here they are request fields, so two
//! browser tabs cannot fight over which model is meant.

use std::path::PathBuf;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::actions;
use crate::web::snapshot;
use crate::web::state::{reply, ApiError, ApiResult, Body, Reply, Shared, Q};

#[derive(Deserialize)]
pub struct RepoRequest {
    #[serde(default)]
    repo: String,
}

pub async fn list_repo(
    State(state): State<Shared>,
    Body(req): Body<RepoRequest>,
) -> ApiResult<Reply> {
    reply(state.act(|app| actions::list_template_repo(app, &req.repo)))
}

#[derive(Deserialize)]
pub struct FetchRequest {
    repo: String,
    revision: Option<String>,
    path: String,
}

pub async fn fetch(State(state): State<Shared>, Body(req): Body<FetchRequest>) -> ApiResult<Reply> {
    reply(
        state
            .act(|app| actions::fetch_template(app, &req.repo, req.revision.as_deref(), &req.path)),
    )
}

#[derive(Deserialize)]
pub struct ApplyRequest {
    template: String,
    model_path: PathBuf,
}

pub async fn apply(State(state): State<Shared>, Body(req): Body<ApplyRequest>) -> ApiResult<Reply> {
    reply(state.act(|app| actions::apply_template(app, &req.template, &req.model_path)))
}

#[derive(Deserialize)]
pub struct RevertRequest {
    model_path: PathBuf,
}

pub async fn revert(
    State(state): State<Shared>,
    Body(req): Body<RevertRequest>,
) -> ApiResult<Reply> {
    reply(state.act(|app| actions::request_revert_template(app, &req.model_path)))
}

pub async fn verify(
    State(state): State<Shared>,
    Body(req): Body<ApplyRequest>,
) -> ApiResult<Reply> {
    reply(state.act(|app| actions::verify_template(app, &req.template, &req.model_path)))
}

#[derive(Deserialize)]
pub struct NameRequest {
    name: String,
}

pub async fn delete(State(state): State<Shared>, Body(req): Body<NameRequest>) -> ApiResult<Reply> {
    reply(state.act(|app| actions::delete_template(app, &req.name)))
}

#[derive(Deserialize)]
pub struct PreviewQuery {
    name: String,
}

#[derive(Serialize)]
pub struct Preview {
    name: String,
    text: String,
    truncated: bool,
}

/// The head of any stored template, not only the one `App` happens to have cached.
///
/// Reading it also refreshes that cache, so the terminal's preview pane and the browser's
/// never show two different templates for the same name.
pub async fn preview(
    State(state): State<Shared>,
    Q(q): Q<PreviewQuery>,
) -> ApiResult<Json<Preview>> {
    let stored = crate::templates::get(&q.name)
        .ok_or_else(|| ApiError::not_found(format!("no template named '{}'", q.name)))?;
    let text = stored.read().map_err(|e| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not read the template: {e:#}"),
        )
    })?;
    let (head, truncated) = snapshot::capped(&text);
    let head = head.to_string();
    state.write(|app| {
        app.templates_view.preview = Some((stored.name.clone(), text.clone()));
    });
    Ok(Json(Preview { name: stored.name, text: head, truncated }))
}
