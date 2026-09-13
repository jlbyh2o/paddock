//! Searching the Hub, choosing a quantization, and downloading it.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::actions;
use crate::actions::Refusal;
use crate::web::state::{reply, ApiResult, Body, Empty, Reply, Shared};

#[derive(Deserialize)]
pub struct SearchRequest {
    #[serde(default)]
    query: String,
}

pub async fn search(
    State(state): State<Shared>,
    Body(req): Body<SearchRequest>,
) -> ApiResult<Reply> {
    reply(state.act(|app| actions::search(app, &req.query)))
}

#[derive(Deserialize)]
pub struct OpenRequest {
    repo_id: String,
    revision: Option<String>,
}

pub async fn open(State(state): State<Shared>, Body(req): Body<OpenRequest>) -> ApiResult<Reply> {
    reply(state.act(|app| actions::open_repo(app, &req.repo_id, req.revision.as_deref())))
}

#[derive(Deserialize)]
pub struct VariantRequest {
    label: String,
}

pub async fn variant(
    State(state): State<Shared>,
    Body(req): Body<VariantRequest>,
) -> ApiResult<Reply> {
    reply(state.act(|app| actions::choose_variant(app, &req.label)))
}

#[derive(Deserialize)]
pub struct ToggleRequest {
    path: String,
    /// Absent means flip it, which is what `Space` does; present sets it outright, which
    /// is what a checkbox wants.
    wanted: Option<bool>,
}

pub async fn toggle_file(
    State(state): State<Shared>,
    Body(req): Body<ToggleRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let wanted = state.act(|app| actions::toggle_file(app, &req.path, req.wanted))?;
    Ok(Json(serde_json::json!({ "status": "ok", "wanted": wanted })))
}

#[derive(Deserialize)]
pub struct SelectRequest {
    mode: SelectMode,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SelectMode {
    All,
    None,
}

pub async fn select_files(
    State(state): State<Shared>,
    Body(req): Body<SelectRequest>,
) -> Json<serde_json::Value> {
    let count = state.write(|app| actions::select_files(app, req.mode == SelectMode::All));
    Json(serde_json::json!({ "status": "ok", "selected_count": count }))
}

#[derive(Deserialize)]
pub struct DownloadRequest {
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    variant: Option<String>,
    #[serde(default)]
    files: Option<Vec<String>>,
}

pub async fn download(
    State(state): State<Shared>,
    Body(req): Body<DownloadRequest>,
) -> ApiResult<Reply> {
    // `repo_id` and `revision` describe the repo already open; downloads always act on
    // the listing the server holds, so they are accepted and checked rather than obeyed.
    // Both mismatches are about the request rather than the machine, so neither toasts.
    reply(state.act(|app| {
        if let Some(wanted) = &req.repo_id {
            if app.hub_view.info.as_ref().is_some_and(|i| i.id != *wanted) {
                return Err(Refusal::quiet(
                    409,
                    format!("{wanted} is not the repo currently listed; open it first"),
                ));
            }
        }
        if let Some(rev) = &req.revision {
            if app.hub_view.revision != *rev {
                return Err(Refusal::quiet(
                    409,
                    format!("the listing is at revision {}, not {rev}", app.hub_view.revision),
                ));
            }
        }
        actions::download(app, req.variant.as_deref(), req.files.as_deref())
    }))
}

pub async fn install_cli(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    reply(state.act(actions::offer_hf_install))
}

#[derive(Deserialize)]
pub struct IdRequest {
    id: u64,
}

pub async fn cancel_download(
    State(state): State<Shared>,
    Body(req): Body<IdRequest>,
) -> ApiResult<Reply> {
    reply(state.act(|app| actions::cancel_download(app, req.id)))
}
