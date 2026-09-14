//! Starting, stopping and smoke-testing the engine.

use axum::extract::State;
use serde::Deserialize;

use crate::actions;
use crate::web::state::{reply, ApiResult, Body, Empty, Reply, Shared};

pub async fn start(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    reply(state.act(actions::start_engine))
}

#[derive(Deserialize)]
pub struct StopRequest {
    #[serde(default)]
    force: bool,
}

pub async fn stop(State(state): State<Shared>, Body(req): Body<StopRequest>) -> ApiResult<Reply> {
    reply(state.act(|app| actions::request_stop(app, req.force)))
}

pub async fn smoke_test(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    reply(state.act(actions::smoke_test))
}

/// Ask the loaded model what upstream changed. Returns as soon as the request is out; the
/// answer arrives in the snapshot, because a summary takes longer than a request should.
pub async fn summarize_upstream(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> ApiResult<Reply> {
    reply(state.act(actions::summarize_upstream))
}
