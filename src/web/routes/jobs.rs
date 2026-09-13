//! Conversions and benchmarks. Job ids and download ids are separate counters and do
//! collide, which is why canceling one is not the same route as canceling the other.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::actions;
use crate::web::state::{reply, ApiResult, Body, Empty, Reply, Shared};

#[derive(Deserialize)]
pub struct IdRequest {
    id: u64,
}

pub async fn bench(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    reply(state.act(actions::run_bench))
}

pub async fn cancel(State(state): State<Shared>, Body(req): Body<IdRequest>) -> ApiResult<Reply> {
    reply(state.act(|app| actions::cancel_job(app, req.id)))
}

pub async fn clear_finished(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    // Clearing nothing changed nothing, so no client is woken for it.
    let removed = state.write_if(|app| {
        let removed = actions::clear_finished(app);
        (removed, removed > 0)
    });
    Json(serde_json::json!({ "status": "ok", "removed": removed }))
}
