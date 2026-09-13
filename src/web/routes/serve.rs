//! The `ft serve` knobs, the planner, and saved profiles.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::actions;
use crate::web::state::{reply, ApiResult, Body, Reply, Shared};

#[derive(Deserialize)]
pub struct Empty {}

#[derive(Deserialize)]
pub struct KnobRequest {
    key: String,
    /// Null or empty unsets the knob, which is what `x`, `Del` and an emptied editor do.
    #[serde(default)]
    value: Option<String>,
}

pub async fn knob(
    State(state): State<Shared>,
    Body(req): Body<KnobRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let set = state.write(|app| actions::set_knob(app, &req.key, req.value.as_deref()))?;
    Ok(Json(serde_json::json!({
        "status": "ok",
        "set": set.set,
        "cleared": set.cleared,
    })))
}

#[derive(Deserialize)]
pub struct FlagRequest {
    key: String,
    /// Absent toggles, as `Enter` and `Space` do; present sets the state outright.
    on: Option<bool>,
}

pub async fn flag(
    State(state): State<Shared>,
    Body(req): Body<FlagRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let on = state.write(|app| actions::toggle_flag(app, &req.key, req.on))?;
    Ok(Json(serde_json::json!({ "status": "ok", "on": on })))
}

#[derive(Deserialize)]
pub struct CycleRequest {
    key: String,
    #[serde(default = "one")]
    delta: i32,
}

fn one() -> i32 {
    1
}

pub async fn cycle(
    State(state): State<Shared>,
    Body(req): Body<CycleRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let value = state.write(|app| actions::cycle_knob(app, &req.key, req.delta as isize))?;
    Ok(Json(serde_json::json!({ "status": "ok", "value": value })))
}

pub async fn plan(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> ApiResult<Json<serde_json::Value>> {
    let held = state.write(actions::build_plan)?;
    Ok(Json(serde_json::json!({ "status": "ok", "plan": held })))
}

pub async fn apply_plan(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> ApiResult<Json<serde_json::Value>> {
    let changed = state.write(actions::apply_plan)?;
    Ok(Json(serde_json::json!({ "status": "ok", "changed": changed })))
}

pub async fn dismiss_plan(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    state.write(actions::dismiss_plan);
    Json(serde_json::json!({ "status": "ok" }))
}

#[derive(Deserialize)]
pub struct NameRequest {
    #[serde(default)]
    name: String,
}

pub async fn save_profile(
    State(state): State<Shared>,
    Body(req): Body<NameRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let created = state.write(|app| actions::save_profile(app, &req.name))?;
    Ok(Json(serde_json::json!({ "status": "ok", "created": created })))
}

pub async fn load_profile(
    State(state): State<Shared>,
    Body(req): Body<NameRequest>,
) -> ApiResult<Reply> {
    state.write(|app| reply(actions::load_profile(app, &req.name)))
}

pub async fn delete_profile(
    State(state): State<Shared>,
    Body(req): Body<NameRequest>,
) -> ApiResult<Reply> {
    state.write(|app| reply(actions::delete_profile(app, &req.name)))
}
