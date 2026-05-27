//! Static SPA assets — `adler-server/dist/` embedded into the binary
//! via `rust-embed`.
//!
//! The bundle lives *inside* this crate so that a standalone
//! `cargo install adler-server` (or path-dep build) finds it. In the
//! workspace, `build.rs` mirrors `../adler-web/dist/` into the local
//! `dist/` whenever the sibling exists, so contributors only need to
//! run `npm run build` in `adler-web/` and `cargo build -p
//! adler-server` picks the refreshed bundle up automatically.
//!
//! Routes attached here:
//!   - `GET /` and any SPA route → `dist/index.html`
//!   - `GET /assets/*` (and any other embedded file) → matched 1:1
//!   - `GET /favicon.ico` → 204 (favicon ships inline in index.html
//!     as an SVG data URI; this stops browser noise without bundling
//!     a separate icon file).

use axum::Router;
use axum::body::Body;
use axum::extract::Path as AxumPath;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "dist/"]
struct Asset;

pub(crate) fn attach(router: Router) -> Router {
    router
        .route("/", get(index))
        .route("/favicon.ico", get(favicon))
        .route("/*path", get(static_file_or_index))
}

async fn index() -> Response {
    serve_embedded("index.html").unwrap_or_else(spa_missing_response)
}

async fn static_file_or_index(AxumPath(path): AxumPath<String>) -> Response {
    // Don't intercept API requests — they should 404 if no handler matched.
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    // Try an exact file match first; otherwise fall through to the
    // SPA's index.html so client-side routing handles unknown paths.
    if let Some(resp) = serve_embedded(&path) {
        return resp;
    }
    serve_embedded("index.html").unwrap_or_else(spa_missing_response)
}

async fn favicon() -> (StatusCode, [(header::HeaderName, &'static str); 1]) {
    (
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "public, max-age=86400")],
    )
}

fn serve_embedded(path: &str) -> Option<Response> {
    let file = Asset::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut response = Body::from(file.data.into_owned()).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    // Cache static assets aggressively; the bundle filenames are
    // content-hashed by Vite so a deploy invalidates them by URL change.
    // index.html is never long-cached — it references the hashed bundles.
    let cache_control = if path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    Some(response)
}

fn spa_missing_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        SPA_MISSING_HTML,
    )
        .into_response()
}

const SPA_MISSING_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Adler — frontend not built</title>
<style>body{font-family:ui-monospace,monospace;background:#000;color:#eee;padding:3rem 2rem;max-width:38rem;margin:auto}h1{color:#ff2d2d}code{background:#1a1a1a;padding:0.1rem 0.4rem}</style></head><body>
<h1>adler-web/dist/ is empty</h1>
<p>The SolidJS bundle wasn't built into the binary at compile time.</p>
<pre>  cd adler-web
  npm install
  npm run build
  cargo build -p adler-cli --release</pre>
<p>API endpoints are still live — see <code>GET /api/health</code>.</p>
</body></html>"#;
