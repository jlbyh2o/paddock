//! Resizing the pools on a live engine.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::actions;
use crate::ui::app::Pool;
use crate::web::state::{reply, ApiResult, Body, Empty, Reply, Shared};

#[derive(Deserialize)]
pub struct PendingRequest {
    /// `ui::app::Pool` itself: it already serializes as exactly the four names the wire
    /// uses, so a parallel enum here would only be a second place to keep them in step.
    pool: Pool,
    /// Null clears the pending edit, which is the `r` key.
    #[serde(default)]
    value: Option<u64>,
}

pub async fn pending(
    State(state): State<Shared>,
    Body(req): Body<PendingRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let staged = state.act(|app| actions::set_cache_pending(app, req.pool, req.value))?;
    Ok(Json(serde_json::json!({ "status": "ok", "pending": staged })))
}

#[derive(Deserialize)]
pub struct AdjustRequest {
    pool: Pool,
    percent: f64,
}

pub async fn adjust(
    State(state): State<Shared>,
    Body(req): Body<AdjustRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let staged = state.act(|app| actions::adjust_cache(app, req.pool, req.percent))?;
    Ok(Json(serde_json::json!({ "status": "ok", "pending": staged })))
}

pub async fn reset_all(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    // Clearing nothing changed nothing, so no client is woken for it.
    state.write_if(|app| {
        let had = app.cache_view.has_pending();
        actions::reset_cache(app);
        ((), had)
    });
    Json(serde_json::json!({ "status": "ok" }))
}

pub async fn apply(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    reply(state.act(actions::apply_cache))
}
