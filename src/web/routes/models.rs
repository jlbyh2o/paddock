//! The local library. Every target is a checkpoint path, never a list index.

use std::path::PathBuf;

use axum::extract::State;
use serde::Deserialize;

use crate::actions;
use crate::web::state::{reply, ApiResult, Body, Reply, Shared};

#[derive(Deserialize)]
pub struct Empty {}

#[derive(Deserialize)]
pub struct PathRequest {
    path: PathBuf,
}

#[derive(Deserialize)]
pub struct UseRequest {
    path: PathBuf,
    #[serde(default)]
    and_serve: bool,
}

pub async fn rescan(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    state.write(|app| reply(actions::rescan(app)))
}

pub async fn use_model(
    State(state): State<Shared>,
    Body(req): Body<UseRequest>,
) -> ApiResult<Reply> {
    state.write(|app| reply(actions::use_model(app, &req.path, req.and_serve)))
}

pub async fn convert(
    State(state): State<Shared>,
    Body(req): Body<PathRequest>,
) -> ApiResult<Reply> {
    state.write(|app| reply(actions::convert_model(app, &req.path)))
}

pub async fn delete(State(state): State<Shared>, Body(req): Body<PathRequest>) -> ApiResult<Reply> {
    state.write(|app| reply(actions::delete_model(app, &req.path)))
}
