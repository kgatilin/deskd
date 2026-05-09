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

/// Render the placeholder dashboard. Child PRs of #442 will replace this with
/// a real agent overview.
pub fn dashboard_page(telegram_id: i64, csrf: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>deskd · dashboard</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
</head>
<body>
<main>
  <h1>deskd dashboard</h1>
  <p>Logged in as Telegram user <strong>{telegram_id}</strong>.</p>
  <p>Agent data lands in a future PR (#442 child 2).</p>
  <form method="post" action="/logout">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit">Log out</button>
  </form>
</main>
</body>
</html>"#,
        telegram_id = telegram_id,
        csrf = html_escape(csrf),
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
        let html = dashboard_page(42, "csrf-1");
        assert!(html.contains("Telegram user <strong>42</strong>"));
        assert!(html.contains(r#"action="/logout""#));
        assert!(html.contains(r#"value="csrf-1""#));
    }
}
