//! Security headers middleware (#443).
//!
//! Adds `Strict-Transport-Security`, `Content-Security-Policy`,
//! `X-Content-Type-Options`, `Referrer-Policy`, and `X-Frame-Options` to
//! every response. These are mandated by the issue's security requirements
//! and verified end-to-end by the integration tests.

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue, header},
    middleware::Next,
    response::Response,
};

/// HSTS value from the issue spec.
pub const HSTS_VALUE: &str = "max-age=31536000; includeSubDomains; preload";

/// CSP value from the issue spec.
pub const CSP_VALUE: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'";

/// Tower middleware that injects security headers on every response.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static(HSTS_VALUE),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP_VALUE),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    resp
}
