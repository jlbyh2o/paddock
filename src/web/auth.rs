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

#[derive(Clone, Debug, Default)]
pub struct Auth {
    token: Option<String>,
}

impl Auth {
    /// Configure the gate. `Err` when the token cannot be carried by the cookie the
    /// browser has to use, which is a startup failure rather than something to discover
    /// when the first login silently does not work.
    pub fn new(token: Option<String>) -> Result<Self, String> {
        // An empty token is no token: a `--token ''` that silently enabled auth against a
        // value nobody can type would lock the operator out of their own daemon.
        let token = token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        if let Some(t) = &token {
            if let Some(bad) = invalid_cookie_char(t) {
                return Err(format!(
                    "the token contains {bad}, which cannot be carried in a cookie — and the \
                     browser has no other way to authenticate an event stream. Use letters, \
                     digits and punctuation other than ';', '=', ',' and spaces"
                ));
            }
        }
        Ok(Self { token })
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
        presented(headers).iter().any(|got| constant_time_eq(got, expected))
    }
}

/// Why a token cannot be a cookie value, named so the message can say it.
///
/// RFC 6265's `cookie-value` excludes whitespace, control characters, quotes, commas,
/// semicolons and backslashes; `=` is excluded here too, because the cookie header is
/// parsed on the first one and a token containing another would be truncated.
fn invalid_cookie_char(token: &str) -> Option<&'static str> {
    token.chars().find_map(|c| match c {
        ';' => Some("';'"),
        '=' => Some("'='"),
        ',' => Some("','"),
        '"' => Some("a double quote"),
        '\\' => Some("a backslash"),
        c if c.is_whitespace() => Some("whitespace"),
        c if c.is_control() => Some("a control character"),
        _ => None,
    })
}

/// Every token a request presents, by either route.
///
/// Both, not the first: a browser that logged in once holds a cookie, and a later request
/// carrying a stale or unrelated `Authorization` header — a proxy's, an extension's — must
/// not shadow it. A non-`Bearer` scheme is ignored rather than treated as a wrong token.
fn presented(headers: &HeaderMap) -> Vec<String> {
    let mut out = Vec::new();
    for raw in headers.get_all(header::AUTHORIZATION) {
        let Ok(text) = raw.to_str() else { continue };
        if let Some(bearer) = text.strip_prefix("Bearer ") {
            out.push(bearer.trim().to_string());
        }
    }
    for raw in headers.get_all(header::COOKIE) {
        let Ok(text) = raw.to_str() else { continue };
        for pair in text.split(';') {
            // A pair with no `=` is not this cookie, and must not end the search: a
            // browser is free to send one before ours.
            let Some((name, value)) = pair.split_once('=') else { continue };
            if name.trim() == COOKIE {
                out.push(value.trim().to_string());
            }
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse::<axum::http::HeaderValue>().unwrap(),
            );
        }
        h
    }

    #[test]
    fn either_credential_authorizes_and_neither_shadows_the_other() {
        let auth = Auth::new(Some("s3cret".into())).unwrap();
        assert!(auth.authorized(&headers(&[("authorization", "Bearer s3cret")])));
        assert!(auth.authorized(&headers(&[("cookie", "a=b; ft_man_token=s3cret")])));

        // A wrong header must not hide a right cookie. This is the case a proxy that adds
        // its own Authorization used to break.
        assert!(auth.authorized(&headers(&[
            ("authorization", "Bearer wrong"),
            ("cookie", "ft_man_token=s3cret"),
        ])));
        assert!(auth.authorized(&headers(&[
            ("cookie", "ft_man_token=wrong"),
            ("authorization", "Bearer s3cret"),
        ])));

        // A non-Bearer scheme is not a token at all, so it is ignored rather than refused.
        assert!(auth.authorized(&headers(&[
            ("authorization", "Basic s3cret"),
            ("cookie", "ft_man_token=s3cret"),
        ])));
        assert!(!auth.authorized(&headers(&[("authorization", "Basic s3cret")])));
        assert!(!auth.authorized(&HeaderMap::new()));
    }

    #[test]
    fn no_token_configured_means_no_authentication() {
        let auth = Auth::new(None).unwrap();
        assert!(!auth.required());
        assert!(auth.authorized(&HeaderMap::new()));
        assert!(Auth::new(Some("   ".into())).unwrap().expected().is_none());
    }

    /// A token the cookie cannot carry is a daemon a browser can never log in to, and the
    /// failure is invisible: the header path works, so `curl` and the tests pass.
    #[test]
    fn a_token_a_cookie_cannot_carry_is_refused_at_startup() {
        for bad in
            ["has space", "semi;colon", "eq=uals", "com,ma", "quo\"te", "back\\slash", "nl\nline"]
        {
            let err = Auth::new(Some(bad.into())).expect_err("a bad token must be refused");
            assert!(err.contains("cookie"), "{bad}: {err}");
        }
        for good in ["s3cret-token", "a.b_c~d", "0123456789abcdef", "Tok3n!"] {
            assert!(Auth::new(Some(good.into())).is_ok(), "{good} is a usable token");
        }
    }
}
