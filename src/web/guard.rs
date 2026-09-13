//! What a state-changing request has to prove before it is allowed to change anything.
//!
//! The daemon answers on a LAN address with a cookie session, which is exactly the shape
//! a cross-site request forgery needs: any page in the operator's browser can `fetch` this
//! origin, the browser attaches the cookie, and `POST /api/models/delete` runs. Two cheap
//! checks close it without a token round trip.
//!
//! * **Origin.** A browser sets `Origin` on every cross-origin request and cannot be
//!   talked out of it. When it is present and its host is not this daemon's own `Host`,
//!   the request came from another site and is refused. A missing `Origin` is allowed:
//!   that is `curl`, and a tool with no origin has no cookie jar to be borrowed either.
//! * **Content type.** A form post — the one cross-site request that needs no CORS
//!   preflight at all — cannot set `application/json`. Requiring it means every
//!   state-changing route is behind a preflight the browser will refuse to make.

use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::state::ApiError;

/// Reject a cross-origin or form-shaped request before it reaches a handler.
pub async fn guard(req: Request, next: Next) -> Response {
    // `GET` and `HEAD` change nothing, and the browser's own rules already keep another
    // origin from reading the response.
    if matches!(req.method(), &Method::GET | &Method::HEAD | &Method::OPTIONS) {
        return next.run(req).await;
    }

    let headers = req.headers();
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if let Some(origin) = origin {
        if !same_origin(origin, host) {
            return ApiError::new(
                StatusCode::FORBIDDEN,
                format!("{origin} is not this daemon's own origin"),
            )
            .into_response();
        }
    }

    if !is_json(headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok())) {
        return ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "this endpoint takes application/json",
        )
        .into_response();
    }

    next.run(req).await
}

/// Whether `Origin` names the same host:port the request was addressed to.
///
/// Compared on authority rather than on the whole URL: the scheme is not something the
/// daemon can know behind a TLS-terminating proxy, and `Host` carries no scheme to compare
/// it against. A request with an `Origin` but no `Host` is refused — every HTTP/1.1 request
/// has one, and an HTTP/2 request's `:authority` is mapped into it by hyper.
fn same_origin(origin: &str, host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    // `null` is what a sandboxed iframe or a `file://` page sends, and it matches nothing.
    let Some(authority) = origin.split_once("://").map(|(_, rest)| rest) else { return false };
    // Neither side may carry a path; an `Origin` never does, and a `Host` never should.
    let authority = authority.split('/').next().unwrap_or(authority);
    authority.eq_ignore_ascii_case(host)
}

/// Whether the body is declared as JSON, ignoring any `; charset=utf-8` the client adds.
///
/// An absent `Content-Type` counts: a `fetch` with no body sends none, and half these
/// routes take no parameters. A form's three types (`application/x-www-form-urlencoded`,
/// `multipart/form-data`, `text/plain`) are the ones this rejects, and they are exactly
/// the ones a cross-site form can produce.
fn is_json(value: Option<&str>) -> bool {
    let Some(value) = value else { return true };
    let base = value.split(';').next().unwrap_or("").trim();
    base.is_empty() || base.eq_ignore_ascii_case("application/json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_origin_matches_only_its_own_authority() {
        assert!(same_origin("http://box.lan:7979", Some("box.lan:7979")));
        assert!(same_origin("https://box.lan:7979", Some("box.lan:7979")));
        assert!(same_origin("http://BOX.lan:7979", Some("box.lan:7979")));
        assert!(!same_origin("http://evil.example", Some("box.lan:7979")));
        // A different port is a different origin, which is what the browser thinks too.
        assert!(!same_origin("http://box.lan:8080", Some("box.lan:7979")));
        assert!(!same_origin("null", Some("box.lan:7979")));
        assert!(!same_origin("http://box.lan:7979", None));
    }

    #[test]
    fn only_json_or_nothing_is_accepted_as_a_body_type() {
        assert!(is_json(None));
        assert!(is_json(Some("application/json")));
        assert!(is_json(Some("application/json; charset=utf-8")));
        assert!(is_json(Some("APPLICATION/JSON")));
        // The three a cross-site form can produce with no preflight.
        assert!(!is_json(Some("application/x-www-form-urlencoded")));
        assert!(!is_json(Some("multipart/form-data; boundary=x")));
        assert!(!is_json(Some("text/plain")));
    }
}
