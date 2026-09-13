//! The one `App`, the sequence counter, and the error envelope every route shares.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;

use crate::actions::{Done, Refusal};
use crate::ui::app::App;

/// Everything a handler can reach.
///
/// The mutex is `std`, not tokio's, and is never held across an `.await` — every handler
/// locks, acts on `App`, and unlocks before it returns. Actions that need to do real work
/// spawn a task and hand it a channel, which is how the TUI has always worked too.
pub struct WebState {
    app: Mutex<App>,
    seq: AtomicU64,
    /// Bumped whenever state changed, so the SSE task knows to rebuild a snapshot. A
    /// watch channel rather than a broadcast: a client wants the newest state, never a
    /// backlog of the states it missed.
    pub changed: tokio::sync::watch::Sender<u64>,
    pub auth: super::auth::Auth,
}

pub type Shared = Arc<WebState>;

impl WebState {
    pub fn new(app: App, auth: super::auth::Auth) -> Shared {
        let (changed, _) = tokio::sync::watch::channel(0);
        Arc::new(Self { app: Mutex::new(app), seq: AtomicU64::new(0), changed, auth })
    }

    /// Read `App` under the lock. A poisoned mutex is recovered rather than propagated:
    /// one panicking handler must not take the whole daemon's state with it.
    pub fn read<T>(&self, f: impl FnOnce(&App) -> T) -> T {
        let app = self.app.lock().unwrap_or_else(|e| e.into_inner());
        f(&app)
    }

    /// Mutate `App` under the lock, then wake the event stream.
    pub fn write<T>(&self, f: impl FnOnce(&mut App) -> T) -> T {
        let out = {
            let mut app = self.app.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut app)
        };
        self.touch();
        out
    }

    /// Tell the event stream that something changed.
    pub fn touch(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    /// The next snapshot's sequence number. Monotonic from 1 for the process's lifetime.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }
}

// ---------------------------------------------------------------- errors

/// Every non-2xx `/api` response: `{"error": "..."}` with a status.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
}

impl From<Refusal> for ApiError {
    fn from(r: Refusal) -> Self {
        // Every status an action produces is one this crate wrote down, so an unknown one
        // is a bug here rather than something a client should see as a 200.
        let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        Self { status, message: r.message }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

// ---------------------------------------------------------------- replies

/// The three shapes an action reply takes, per docs/web-api.md section 4.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub enum Reply {
    Ok {
        status: &'static str,
    },
    Started {
        status: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        job_id: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        log_path: Option<String>,
    },
}

impl From<Done> for Reply {
    fn from(d: Done) -> Self {
        match d {
            Done::Ok => Reply::Ok { status: "ok" },
            Done::ConfirmPending => Reply::Ok { status: "confirm_pending" },
            Done::Started { job_id, log_path } => Reply::Started {
                status: "started",
                job_id,
                log_path: log_path.map(|p| p.display().to_string()),
            },
        }
    }
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}

/// Turn an action's result into the route's response.
pub fn reply(outcome: Result<Done, Refusal>) -> ApiResult<Reply> {
    Ok(outcome?.into())
}

// ---------------------------------------------------------------- body extractor

/// A JSON request body that tolerates an absent one.
///
/// Half the routes here take no parameters, and `fetch("/api/models/rescan", {method:
/// "POST"})` sends no body at all. Treating that as `{}` costs nothing and removes a
/// whole class of 400 that says nothing useful.
pub struct Body<T>(pub T);

impl<S, T> FromRequest<S> for Body<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, _state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::to_bytes(req.into_body(), 1 << 20)
            .await
            .map_err(|e| ApiError::bad_request(format!("could not read the request body: {e}")))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ApiError::bad_request("the request body is not valid utf-8"))?;
        let text = if text.trim().is_empty() { "{}" } else { text };
        serde_json::from_str(text)
            .map(Body)
            .map_err(|e| ApiError::bad_request(format!("bad request body: {e}")))
    }
}

/// A query string that reports its failures in the error envelope.
///
/// `axum::extract::Query` rejects with plain text, which would be the one `/api` response
/// a client could not parse as `{"error": ...}`.
pub struct Q<T>(pub T);

impl<S, T> FromRequestParts<S> for Q<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or("");
        serde_urlencoded::from_str(query)
            .map(Q)
            .map_err(|e| ApiError::bad_request(format!("bad query string: {e}")))
    }
}
