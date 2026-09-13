//! The optional bearer token.
//!
//! Off by default, exactly as `ft serve` is. When a token is configured every `/api`
//! request must carry it, as `Authorization: Bearer` or as the `ft_man_token` cookie —
//! `EventSource` cannot send a header, so the cookie is the path a browser actually
//! takes and the header exists for `curl` and for tests.

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

use super::state::{ApiError, Shared};

pub const COOKIE: &str = "ft_man_token";

#[derive(Clone, Default)]
pub struct Auth {
    token: Option<String>,
}

impl Auth {
    pub fn new(token: Option<String>) -> Self {
        // An empty token is no token: a `--token ''` that silently enabled auth against a
        // value nobody can type would lock the operator out of their own daemon.
        Self { token: token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) }
    }

    pub fn required(&self) -> bool {
        self.token.is_some()
    }

    /// The configured token, for the one place that has to compare against it directly.
    pub fn expected(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Whether a request carries the right token. Always true when none is configured.
    pub fn authorized(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = &self.token else { return true };
        presented(headers).is_some_and(|got| constant_time_eq(&got, expected))
    }
}

/// The token a request presents, by either route.
fn presented(headers: &HeaderMap) -> Option<String> {
    if let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(bearer.trim().to_string());
    }
    for raw in headers.get_all(header::COOKIE) {
        let Ok(text) = raw.to_str() else { continue };
        for pair in text.split(';') {
            let (name, value) = pair.split_once('=')?;
            if name.trim() == COOKIE {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Compare without leaking how far the comparison got. A token guessed one character at
/// a time is not much of a token.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    // Lengths are compared in the clear; they are not the secret, and `ct_eq` needs
    // equal-length slices anyway.
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// Gate every `/api` route the router puts behind it. `GET /api/auth` and the static
/// assets are registered outside it, so the login page can always load and always ask.
pub async fn gate(State(state): State<Shared>, req: Request, next: Next) -> Response {
    if state.auth.authorized(req.headers()) {
        return next.run(req).await;
    }
    ApiError::new(StatusCode::UNAUTHORIZED, "this daemon requires a token").into_response()
}
