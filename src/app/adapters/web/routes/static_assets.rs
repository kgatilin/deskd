//! Vendored static assets (#444).
//!
//! Serves htmx, the htmx SSE extension, and the dashboard stylesheet from
//! the binary itself via `include_str!`. Vendoring (rather than CDN) keeps
//! the strict CSP from #443 (`script-src 'self'; style-src 'self'`) intact
//! — no third-party origins required, and no inline `<style>` element.
//!
//! Routes:
//! - `GET /static/htmx.min.js`   — htmx 2.0.3 minified
//! - `GET /static/htmx-sse.js`   — htmx-ext-sse 2.2.2 (the SSE extension)
//! - `GET /static/dashboard.css` — dashboard stylesheet
//! - `GET /static/pico.min.css`  — vendored Pico.css v2 (#497)
//! - `GET /static/panel.js`      — panel v2 delegated row click (#516)

use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

const HTMX_JS: &str = include_str!("../static/htmx.min.js");
const HTMX_SSE_JS: &str = include_str!("../static/htmx-sse.js");
/// Dashboard stylesheet. Public so the templates module can sanity-check
/// the bundled CSS without re-reading it from disk.
pub const DASHBOARD_CSS: &str = include_str!("../static/dashboard.css");
/// Pico.css v2 — vendored, classless, ~30 KB. Served from `/static/pico.min.css`
/// for the Panel v2 templates (#497).
pub const PICO_CSS: &str = include_str!("../static/pico.min.css");
/// Panel v2 delegated row-click handler (#516). Served from `/static/panel.js`
/// so the strict CSP (`script-src 'self'`) holds — replaces the inline
/// row-click attribute previously emitted on agent rows.
pub const PANEL_JS: &str = include_str!("../static/panel.js");

const JS_CONTENT_TYPE: &str = "application/javascript; charset=utf-8";
const CSS_CONTENT_TYPE: &str = "text/css; charset=utf-8";
/// Cache for an hour — we'll bump these by editing the file and rebuilding,
/// no fingerprinting needed for a single-user dashboard.
const STATIC_CACHE_CONTROL: &str = "public, max-age=3600";

pub async fn htmx_js() -> Response {
    static_response(HTMX_JS, JS_CONTENT_TYPE)
}

pub async fn htmx_sse_js() -> Response {
    static_response(HTMX_SSE_JS, JS_CONTENT_TYPE)
}

pub async fn dashboard_css() -> Response {
    static_response(DASHBOARD_CSS, CSS_CONTENT_TYPE)
}

/// `GET /static/pico.min.css` — vendored Pico.css v2 (#497).
pub async fn pico_css() -> Response {
    static_response(PICO_CSS, CSS_CONTENT_TYPE)
}

/// `GET /static/panel.js` — panel v2 delegated row-click handler (#516).
pub async fn panel_js() -> Response {
    static_response(PANEL_JS, JS_CONTENT_TYPE)
}

fn static_response(body: &'static str, content_type: &'static str) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    let h = resp.headers_mut();
    if let Ok(v) = content_type.parse() {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = STATIC_CACHE_CONTROL.parse() {
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

    #[tokio::test]
    async fn dashboard_css_served_with_css_content_type() {
        let resp = dashboard_css().await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("text/css"));
    }

    #[tokio::test]
    async fn dashboard_css_includes_width_bucket_classes() {
        // The width-bucket scheme replaces inline `style="width: X%"` on
        // the progress bar so the strict CSP (no inline styles) holds.
        assert!(DASHBOARD_CSS.contains(".ctx-bar__fill--w-0"));
        assert!(DASHBOARD_CSS.contains(".ctx-bar__fill--w-50"));
        assert!(DASHBOARD_CSS.contains(".ctx-bar__fill--w-100"));
    }

    #[tokio::test]
    async fn panel_js_served_with_js_content_type() {
        let resp = panel_js().await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("application/javascript"));
    }

    #[tokio::test]
    async fn panel_js_is_delegated_handler_with_data_href() {
        // Sanity-check the vendored handler — must use the `data-href`
        // attribute and skip inner `<a>` clicks, otherwise the CSP-safe
        // delegated dispatch can't replace the old inline `onclick`.
        assert!(PANEL_JS.contains("tr.agent-row[data-href]"));
        assert!(PANEL_JS.contains("data-href") || PANEL_JS.contains("dataset.href"));
        assert!(!PANEL_JS.contains("onclick"));
    }

    /// Regression guard for #522: opacity-based dimming on Panel v2
    /// elements compounded with Pico.css v2's already-muted light theme
    /// and dropped table headers / KPI labels / breakdown / breadcrumb
    /// below WCAG AA. The fix replaces those with explicit `color:`
    /// declarations that meet ≥4.5:1 on white.
    #[tokio::test]
    async fn dashboard_css_panel_v2_uses_explicit_colors_not_opacity() {
        // The token providing the muted color, declared on `.panel`.
        assert!(
            DASHBOARD_CSS.contains("--panel-text-muted"),
            "missing --panel-text-muted custom property"
        );
        // The concrete contrast-safe value (`#475569` ≈ 7.5:1 on #fff).
        assert!(
            DASHBOARD_CSS.contains("#475569"),
            "missing slate-600 muted color value"
        );

        // The selectors previously dimmed via `opacity` must now bind
        // `color:` from the token. We only check a substring of the
        // generated declarations — a layout reflow can reorder
        // properties, but `color: var(--panel-text-muted)` is durable.
        //
        // Anchor lookups to the Panel v2 section (after the token is
        // declared) so legacy v1 rules like the original `.breadcrumb`
        // — which lives earlier in the file and never used opacity —
        // don't trigger a false positive on selectors that v2 redeclares.
        let v2_start = DASHBOARD_CSS
            .find("--panel-text-muted")
            .expect("Panel v2 token declaration must exist");
        let v2_section = &DASHBOARD_CSS[v2_start..];

        let must_use_muted = [
            ".panel-kpi .kpi-grid dt",
            ".panel-kpi .kpi-breakdown",
            ".panel-weekly .weekly-cap-hits",
            ".model-rows th, .agent-rows th, .sessions-list th",
            ".breadcrumb",
            ".legacy-fold",
            ".panel small",
        ];
        for sel in must_use_muted {
            let rule_start = v2_section
                .find(sel)
                .unwrap_or_else(|| panic!("selector not found in Panel v2 section: {sel}"));
            let rule_end = v2_section[rule_start..]
                .find('}')
                .map(|i| rule_start + i)
                .unwrap_or(v2_section.len());
            let rule_body = &v2_section[rule_start..rule_end];
            assert!(
                rule_body.contains("color: var(--panel-text-muted)"),
                "{sel} no longer binds color: var(--panel-text-muted) — body was: {rule_body}",
            );
            assert!(
                !rule_body.contains("opacity:"),
                "{sel} still uses opacity-based dimming — body was: {rule_body}",
            );
        }
    }
}
