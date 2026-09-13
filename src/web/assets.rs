//! The embedded single-page application.
//!
//! `build.rs` stages `web/dist` (or a placeholder explaining how to build it) into
//! `$OUT_DIR/web`, and it is compiled in from there, so a deployment stays one file.

use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$OUT_DIR/web"]
struct Bundle;

/// Serve an embedded file, falling back to `index.html` so client-side routes deep-link.
///
/// Anything under `/api` is routed before this and never reaches here, so a missing API
/// route returns the JSON envelope rather than a page that looks like it worked.
pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Bundle::get(path) {
        Some(file) => {
            let etag = etag(&file.metadata.sha256_hash());
            if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(&etag) {
                return StatusCode::NOT_MODIFIED.into_response();
            }
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            // `mime_guess` leaves text types bare, and a page served without a charset is
            // at the mercy of whatever the browser decides to sniff.
            let mime = match mime.type_() {
                mime_guess::mime::TEXT => format!("{mime}; charset=utf-8"),
                _ => mime.to_string(),
            };
            // A hashed filename names exactly one immutable body, so it can be cached
            // forever; everything else must be revalidated or a redeploy is invisible.
            let cache =
                if is_hashed(path) { "public, max-age=31536000, immutable" } else { "no-cache" };
            (
                [
                    (header::CONTENT_TYPE, mime.as_str()),
                    (header::ETAG, etag.as_str()),
                    (header::CACHE_CONTROL, cache),
                ],
                Body::from(file.data.into_owned()),
            )
                .into_response()
        }
        None => index(),
    }
}

/// `index.html`, for `/` and for every client-side route.
pub fn index() -> Response {
    match Bundle::get("index.html") {
        Some(file) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            Body::from(file.data.into_owned()),
        )
            .into_response(),
        // Unreachable in a built binary: build.rs always stages an index.html, even when
        // there is no frontend to stage. Saying so beats an empty 404.
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "no web bundle is embedded in this binary",
        )
            .into_response(),
    }
}

fn etag(hash: &[u8; 32]) -> String {
    let mut out = String::with_capacity(34);
    out.push('"');
    for b in &hash[..16] {
        out.push_str(&format!("{b:02x}"));
    }
    out.push('"');
    out
}

/// Vite writes `name-BQx1cP4k.js`; a build hash is what makes a URL immutable.
fn is_hashed(path: &str) -> bool {
    let Some(stem) = path.rsplit('/').next().and_then(|f| f.split('.').next()) else {
        return false;
    };
    stem.rsplit_once('-').is_some_and(|(_, hash)| {
        hash.len() >= 8 && hash.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}
