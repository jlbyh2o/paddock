//! Conversions and benchmarks. Job ids and download ids are separate counters and do
//! collide, which is why cancelling one is not the same route as cancelling the other.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::actions;
use crate::web::state::{reply, ApiResult, Body, Reply, Shared};

#[derive(Deserialize)]
pub struct Empty {}

#[derive(Deserialize)]
pub struct IdRequest {
    id: u64,
}

pub async fn bench(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    state.write(|app| reply(actions::run_bench(app)))
}

pub async fn cancel(State(state): State<Shared>, Body(req): Body<IdRequest>) -> ApiResult<Reply> {
    state.write(|app| reply(actions::cancel_job(app, req.id)))
}

pub async fn clear_finished(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    let removed = state.write(actions::clear_finished);
    Json(serde_json::json!({ "status": "ok", "removed": removed }))
}
