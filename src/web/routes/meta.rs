//! Authentication, the knob schema, the snapshot, and the pending confirmation.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::actions;
use crate::knobs::{Group, Knob, KNOBS};
use crate::web::auth::{constant_time_eq, COOKIE};
use crate::web::state::{reply, ApiError, ApiResult, Body, Reply, Shared};

// ---------------------------------------------------------------- auth

#[derive(Serialize)]
pub struct AuthStatus {
    auth_required: bool,
    authorized: bool,
}

/// Never gated: the login page has to be able to ask whether it is needed.
pub async fn auth(State(state): State<Shared>, headers: HeaderMap) -> Json<AuthStatus> {
    Json(AuthStatus {
        auth_required: state.auth.required(),
        authorized: state.auth.authorized(&headers),
    })
}

#[derive(Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    token: String,
}

pub async fn login(
    State(state): State<Shared>,
    headers: HeaderMap,
    Body(req): Body<LoginRequest>,
) -> Response {
    let Some(expected) = state.auth.expected() else {
        return Json(serde_json::json!({ "authorized": true })).into_response();
    };
    if !constant_time_eq(req.token.trim(), expected) {
        return ApiError::new(StatusCode::UNAUTHORIZED, "that token is not correct")
            .into_response();
    }
    // A session cookie, with no Max-Age: the token belongs to the browser tab that was
    // told it, not to the disk. `Secure` only over TLS, or the cookie is dropped outright
    // on the plain-HTTP LAN address this daemon usually answers on.
    let secure = if over_tls(&headers) { "; Secure" } else { "" };
    let cookie =
        format!("{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict{secure}", req.token.trim());
    ([(header::SET_COOKIE, cookie)], Json(serde_json::json!({ "authorized": true })))
        .into_response()
}

pub async fn logout() -> Response {
    let cookie = format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    ([(header::SET_COOKIE, cookie)], Json(serde_json::json!({ "authorized": false })))
        .into_response()
}

/// Whether the request reached us over TLS, directly or through a proxy that says so.
fn over_tls(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p.eq_ignore_ascii_case("https"))
}

// ---------------------------------------------------------------- snapshot

pub async fn snapshot(State(state): State<Shared>) -> Response {
    // The same builder the broadcaster runs, and the frame it produces is published too:
    // a client that asks for a snapshot while a stream is open should not see an older
    // document than the stream just delivered.
    match state.publish() {
        Some(frame) => {
            ([(header::CONTENT_TYPE, "application/json")], frame.json.to_string()).into_response()
        }
        None => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "could not build a snapshot")
            .into_response(),
    }
}

// ---------------------------------------------------------------- knobs

#[derive(Serialize)]
pub struct KnobSchema {
    groups: Vec<GroupOut>,
    knobs: &'static [Knob],
}

#[derive(Serialize)]
pub struct GroupOut {
    /// `knobs::Group` itself, which already serializes as the wire's own spelling.
    group: Group,
    title: &'static str,
}

/// Static for the process's lifetime, so it is served once and cached by `ETag`. Keeping
/// it out of the snapshot saves about 12 KiB of help text on every frame.
pub async fn knobs(headers: HeaderMap) -> Response {
    let etag = format!("\"{}\"", env!("CARGO_PKG_VERSION"));
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let schema = KnobSchema {
        groups: Group::ALL.iter().map(|g| GroupOut { group: *g, title: g.title() }).collect(),
        knobs: KNOBS,
    };
    ([(header::ETAG, etag)], Json(schema)).into_response()
}

// ---------------------------------------------------------------- confirm

#[derive(Deserialize)]
pub struct ConfirmRequest {
    accept: bool,
}

pub async fn confirm(
    State(state): State<Shared>,
    Body(req): Body<ConfirmRequest>,
) -> ApiResult<Reply> {
    reply(state.act(|app| actions::answer_confirm(app, req.accept)))
}
