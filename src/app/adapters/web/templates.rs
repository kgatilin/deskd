//! Inline HTML templates for the web adapter (#443).
//!
//! Kept minimal on purpose — the dashboard is a placeholder that future
//! children of #442 will replace. CSP forbids inline `<script>` tags so all
//! interactivity must come from same-origin assets (none today).

/// Render the `/login` page with a single button. The CSRF field is required
/// even though the button has no real form fields beyond `_csrf` because the
/// CSRF middleware checks every POST.
pub fn login_page(csrf: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>deskd · login</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
</head>
<body>
<main>
  <h1>deskd</h1>
  <p>Sign in via Telegram. We will send a one-time link to your configured account.</p>
  <form method="post" action="/login/request">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit">Send link to Telegram</button>
  </form>
</main>
</body>
</html>"#,
        csrf = html_escape(csrf)
    )
}

/// Render the dashboard (#444 + #446). Mobile-first, server-rendered.
/// Live updates arrive over SSE via vendored htmx + the htmx-ext-sse
/// extension; both scripts and the stylesheet are served from `/static/*`
/// so the strict CSP from #443 (`script-src 'self'; style-src 'self'`)
/// holds without permitting inline `<style>` elements or `style=`
/// attributes.
pub fn dashboard_page(
    telegram_id: i64,
    csrf: &str,
    refresh_form_html: &str,
    vps_strip_html: &str,
    agents_section_html: &str,
) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>deskd · dashboard</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="stylesheet" href="/static/dashboard.css">
<script src="/static/htmx.min.js"></script>
<script src="/static/htmx-sse.js"></script>
</head>
<body>
<header class="topbar">
  <h1>deskd</h1>
  <span class="topbar__user">tg:{telegram_id}</span>
  <form class="topbar__logout" method="post" action="/logout">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit">Log out</button>
  </form>
</header>
<main>
  <section hx-ext="sse" sse-connect="/events" sse-swap="vps-strip" id="vps-strip-wrap">
    {vps_strip_html}
  </section>
  {refresh_form_html}
  {agents_section_html}
</main>
</body>
</html>"#,
        telegram_id = telegram_id,
        csrf = html_escape(csrf),
        refresh_form_html = refresh_form_html,
        vps_strip_html = vps_strip_html,
        agents_section_html = agents_section_html,
    )
}

/// Render the «refresh now» form (#446). CSRF-protected `<form method=POST>`
/// — no JS framework, so the strict CSP holds without exceptions.
pub fn metrics_refresh_form(csrf: &str) -> String {
    format!(
        r#"<form class="metrics-refresh" method="post" action="/metrics/refresh">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit">Refresh disk metrics</button>
</form>"#,
        csrf = html_escape(csrf)
    )
}

/// Render the `/agent/<name>` detail page (#446 surface). The dedicated
/// page lists the agent's home-dir size and the top-5 subdirectory
/// breakdown. Header / nav match the dashboard.
pub fn agent_detail_page(
    telegram_id: i64,
    csrf: &str,
    agent_name: &str,
    detail_html: &str,
) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>deskd · {agent}</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="stylesheet" href="/static/dashboard.css">
</head>
<body>
<header class="topbar">
  <h1>deskd</h1>
  <span class="topbar__user">tg:{telegram_id}</span>
  <form class="topbar__logout" method="post" action="/logout">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit">Log out</button>
  </form>
</header>
<main>
  <p><a href="/">← dashboard</a></p>
  <h2>{agent}</h2>
  {detail_html}
</main>
</body>
</html>"#,
        telegram_id = telegram_id,
        csrf = html_escape(csrf),
        agent = html_escape(agent_name),
        detail_html = detail_html,
    )
}

/// Page shown after a successful magic-link request: tells the user the link
/// has been dispatched. Kept on a separate URL so a refresh doesn't re-POST.
pub fn link_sent_page() -> &'static str {
    r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>deskd · link sent</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
</head>
<body>
<main>
  <h1>Login link sent</h1>
  <p>Check Telegram for a one-time login link. It is single-use and expires shortly.</p>
</main>
</body>
</html>"#
}

fn html_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            other => other.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_page_includes_csrf_token() {
        let html = login_page("token-xyz");
        assert!(html.contains(r#"value="token-xyz""#));
        assert!(html.contains(r#"action="/login/request""#));
    }

    #[test]
    fn login_page_escapes_csrf_token() {
        // Defense in depth — even though tokens are base64url they go through
        // the escaper. Use an XSS-shaped fake input to verify.
        let html = login_page("<script>alert(1)</script>");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn dashboard_page_includes_logout_form() {
        let html = dashboard_page(
            42,
            "csrf-1",
            "<form></form>",
            "<section class='vps-strip'></section>",
            "<section></section>",
        );
        assert!(html.contains("tg:42"));
        assert!(html.contains(r#"action="/logout""#));
        assert!(html.contains(r#"value="csrf-1""#));
    }

    #[test]
    fn dashboard_page_loads_vendored_htmx() {
        let html = dashboard_page(1, "x", "", "", "");
        // Vendored under /static/ — never reach out to a CDN, keeps strict
        // CSP (script-src 'self') intact.
        assert!(html.contains(r#"src="/static/htmx.min.js""#));
        assert!(html.contains(r#"src="/static/htmx-sse.js""#));
    }

    #[test]
    fn dashboard_page_links_external_stylesheet() {
        // CSP `style-src 'self'` forbids inline <style> blocks; the CSS is
        // served from /static/dashboard.css instead. See #450 review.
        let html = dashboard_page(1, "x", "", "", "");
        assert!(
            html.contains(r#"<link rel="stylesheet" href="/static/dashboard.css">"#),
            "dashboard must link external stylesheet"
        );
    }

    #[test]
    fn dashboard_page_has_no_inline_style_block() {
        // Regression guard: the strict CSP from #443 does NOT permit inline
        // <style> elements. If this assertion ever fires, the dashboard will
        // load unstyled in production.
        let html = dashboard_page(
            7,
            "csrf-token",
            "",
            "<section class='vps-strip'></section>",
            "<section></section>",
        );
        assert!(
            !html.contains("<style>") && !html.contains("<style "),
            "inline <style> element snuck back into the dashboard HTML"
        );
    }

    #[test]
    fn dashboard_page_includes_word_dashboard_for_smoke_tests() {
        // The existing #443 integration test asserts on the literal word
        // "dashboard" appearing in the HTML; preserve that.
        let html = dashboard_page(1, "x", "", "", "");
        assert!(html.contains("dashboard"));
    }

    #[test]
    fn dashboard_page_wraps_vps_strip_in_sse_target() {
        // #446: when the disk collector publishes `metrics.updated`, the
        // SSE stream emits a `vps-strip` named event so htmx swaps the
        // top-of-page strip without a polling loop.
        let html = dashboard_page(1, "x", "", "<section class='vps-strip'></section>", "");
        assert!(html.contains(r#"sse-swap="vps-strip""#));
    }

    #[test]
    fn metrics_refresh_form_posts_to_endpoint_with_csrf() {
        let html = metrics_refresh_form("csrf-x");
        assert!(html.contains(r#"action="/metrics/refresh""#));
        assert!(html.contains(r#"value="csrf-x""#));
        assert!(html.contains("method=\"post\""));
    }

    #[test]
    fn agent_detail_page_renders_name_and_back_link() {
        let html = agent_detail_page(1, "csrf", "kira", "<p>body</p>");
        assert!(html.contains("kira"));
        assert!(html.contains(r#"<a href="/">← dashboard</a>"#));
        assert!(html.contains("<p>body</p>"));
    }

    #[test]
    fn agent_detail_page_escapes_name() {
        let html = agent_detail_page(1, "csrf", "<x>", "body");
        assert!(html.contains("&lt;x&gt;"));
        assert!(!html.contains("<title>deskd · <x>"));
    }
}
