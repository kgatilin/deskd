//! Vendored static assets (#444).
//!
//! Serves htmx and the htmx SSE extension from the binary itself via
//! `include_str!`. Vendoring (rather than CDN) keeps the strict CSP from
//! #443 (`script-src 'self'`) intact — no third-party origins required.
//!
//! Routes:
//! - `GET /static/htmx.min.js`   — htmx 2.0.3 minified
//! - `GET /static/htmx-sse.js`   — htmx-ext-sse 2.2.2 (the SSE extension)

use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

const HTMX_JS: &str = include_str!("../static/htmx.min.js");
const HTMX_SSE_JS: &str = include_str!("../static/htmx-sse.js");

const JS_CONTENT_TYPE: &str = "application/javascript; charset=utf-8";
/// Cache for an hour — we'll bump these by editing the file and rebuilding,
/// no fingerprinting needed for a single-user dashboard.
const JS_CACHE_CONTROL: &str = "public, max-age=3600";

pub async fn htmx_js() -> Response {
    js_response(HTMX_JS)
}

pub async fn htmx_sse_js() -> Response {
    js_response(HTMX_SSE_JS)
}

fn js_response(body: &'static str) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    let h = resp.headers_mut();
    if let Ok(v) = JS_CONTENT_TYPE.parse() {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = JS_CACHE_CONTROL.parse() {
        h.insert(header::CACHE_CONTROL, v);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn htmx_js_served_with_js_content_type() {
        let resp = htmx_js().await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("application/javascript"));
    }

    #[tokio::test]
    async fn sse_extension_starts_with_iife() {
        // Sanity-check that we vendored the right file, not an HTML 404 page.
        assert!(HTMX_SSE_JS.contains("Server Sent Events Extension"));
    }

    #[tokio::test]
    async fn htmx_main_starts_with_var_declaration() {
        assert!(HTMX_JS.starts_with("var htmx="));
    }
}
