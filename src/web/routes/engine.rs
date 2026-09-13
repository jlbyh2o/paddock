//! Starting, stopping and smoke-testing the engine.

use axum::extract::State;
use serde::Deserialize;

use crate::actions;
use crate::web::state::{reply, ApiResult, Body, Reply, Shared};

#[derive(Deserialize)]
pub struct Empty {}

pub async fn start(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    state.write(|app| reply(actions::start_engine(app)))
}

#[derive(Deserialize)]
pub struct StopRequest {
    #[serde(default)]
    force: bool,
}

pub async fn stop(State(state): State<Shared>, Body(req): Body<StopRequest>) -> ApiResult<Reply> {
    state.write(|app| reply(actions::request_stop(app, req.force)))
}

pub async fn smoke_test(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    state.write(|app| reply(actions::smoke_test(app)))
}
