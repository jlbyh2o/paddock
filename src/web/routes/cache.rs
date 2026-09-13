//! Resizing the pools on a live engine.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::actions;
use crate::ui::app::Pool;
use crate::web::state::{reply, ApiResult, Body, Reply, Shared};

#[derive(Deserialize)]
pub struct Empty {}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PoolId {
    Moe,
    Kv,
    Mamba,
    Swa,
}

impl From<PoolId> for Pool {
    fn from(p: PoolId) -> Self {
        match p {
            PoolId::Moe => Pool::Moe,
            PoolId::Kv => Pool::Kv,
            PoolId::Mamba => Pool::Mamba,
            PoolId::Swa => Pool::Swa,
        }
    }
}

#[derive(Deserialize)]
pub struct PendingRequest {
    pool: PoolId,
    /// Null clears the pending edit, which is the `r` key.
    #[serde(default)]
    value: Option<u64>,
}

pub async fn pending(
    State(state): State<Shared>,
    Body(req): Body<PendingRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let staged = state.write(|app| actions::set_cache_pending(app, req.pool.into(), req.value))?;
    Ok(Json(serde_json::json!({ "status": "ok", "pending": staged })))
}

#[derive(Deserialize)]
pub struct AdjustRequest {
    pool: PoolId,
    percent: f64,
}

pub async fn adjust(
    State(state): State<Shared>,
    Body(req): Body<AdjustRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let staged = state.write(|app| actions::adjust_cache(app, req.pool.into(), req.percent))?;
    Ok(Json(serde_json::json!({ "status": "ok", "pending": staged })))
}

pub async fn reset_all(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    state.write(actions::reset_cache);
    Json(serde_json::json!({ "status": "ok" }))
}

pub async fn apply(State(state): State<Shared>, Body(_): Body<Empty>) -> ApiResult<Reply> {
    state.write(|app| reply(actions::apply_cache(app)))
}
