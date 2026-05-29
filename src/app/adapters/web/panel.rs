//! Panel v2 HTML rendering (#497).
//!
//! Three levels, all server-rendered on top of Pico.css v2:
//!
//! - Level 1: `/` overview — KPI block + weekly token gauge + per-model
//!   breakdown + activity sparkline + agent rows.
//! - Level 2: `/agents/<name>` — per-agent detail with 24h / 7d / 30d
//!   tabs.
//! - Level 3: `/agents/<name>/sessions/<id>` — transcript view.
//!
//! Pico.css is vendored at `/static/pico.min.css` (also linked via the
//! template); the strict CSP from #443 (`style-src 'self'`) holds.
//!
//! ## CSP gotcha
//!
//! The CSP forbids inline `<style>` blocks and `style=` attributes.
//! Every color tier for the gauge is therefore handled via CSS classes
//! that live in `dashboard.css` (`.gauge--green`, `.gauge--yellow`,
//! `.gauge--red`).

use crate::app::adapters::web::event_query::{
    AgentRollup, GaugeTier, Kpi, ModelTokens, SessionInfo, WeeklyTokens,
};

/// HTML escape (small, identical to the inline helper in templates.rs).
fn esc(s: &str) -> String {
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

/// Render a u64 token / cost value with thousand separators.
pub fn fmt_u64(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

/// Format millions (M) shorthand for large counts.
pub fn fmt_short(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// Render the full Level 1 page body (inside `<body>` of the shared
/// shell, excluding the topbar). Order: KPI → weekly tokens → activity
/// sparkline → agent rows. Existing markers (`vps-strip`, `uptime`,
/// `sse-connect="/events"`) are preserved so the dashboard tests still
/// see them.
#[allow(clippy::too_many_arguments)]
pub fn level1_body(
    kpi: &Kpi,
    weekly: &WeeklyTokens,
    estimate: u64,
    sparkline: &str,
    agents: &[AgentRollup],
    legacy_strip_html: &str,
    legacy_agents_html: &str,
) -> String {
    let ratio = if estimate > 0 {
        weekly.total() as f64 / estimate as f64
    } else {
        0.0
    };
    let tier = GaugeTier::from_ratio(ratio);
    let bar = crate::app::adapters::web::event_query::unicode_bar(weekly.total(), estimate, 24);
    let pct = (ratio.clamp(0.0, 1.0) * 100.0).round() as u32;

    let kpi_html = render_kpi_block(kpi);
    let weekly_html = format!(
        r#"<section class="panel-weekly">
  <header><h2>Weekly tokens</h2></header>
  <p class="weekly-gauge {tier_class} font-mono">
    <span class="weekly-gauge__bar">{bar}</span>
    <span class="weekly-gauge__nums">{used} / {est} ({pct}%)</span>
  </p>
  <p class="weekly-cap-hits font-mono">limit-hits this week: {hits}</p>
  {model_rows}
</section>"#,
        tier_class = tier.css_class(),
        bar = esc(&bar),
        used = esc(&fmt_short(weekly.total())),
        est = esc(&fmt_short(estimate)),
        pct = pct,
        hits = weekly.weekly_cap_hits,
        model_rows = render_model_rows(&weekly.by_model)
    );

    let spark_html = format!(
        r#"<section class="panel-activity">
  <header><h2>Activity (24h iterations)</h2></header>
  <p class="font-mono activity-sparkline">{sp}</p>
</section>"#,
        sp = esc(sparkline)
    );

    let rows_html = render_agent_rows(agents);

    // The legacy chart block (`id="dashboard-chart-block"`) must precede
    // the legacy `vps-strip-wrap` element so older tests/tools (#484) see
    // the same DOM order. Callers can split the legacy markup into
    // `<chart_block>` (rendered above the strip) + `<remainder>` (folded
    // under «Legacy dashboard») by separating them with the literal token
    // `<!--LEGACY_SPLIT-->`; otherwise the whole `legacy_agents_html`
    // lives in the fold and we render only `legacy_strip_html` above.
    let (legacy_chart_above_strip, legacy_remainder) =
        match legacy_agents_html.split_once("<!--LEGACY_SPLIT-->") {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (String::new(), legacy_agents_html.to_string()),
        };

    format!(
        r#"<main class="panel container">
{kpi_html}
{weekly_html}
{spark_html}
<section class="panel-agents">
  <header><h2>Agents (7d)</h2></header>
  {rows_html}
</section>
{legacy_chart_above_strip}
<section class="vps-strip" hx-ext="sse" sse-connect="/events" sse-swap="vps-strip" id="vps-strip-wrap">
  {legacy_strip}
</section>
<details class="legacy-fold"><summary>Legacy dashboard</summary>
{legacy_agents}
</details>
</main>"#,
        kpi_html = kpi_html,
        weekly_html = weekly_html,
        spark_html = spark_html,
        rows_html = rows_html,
        legacy_chart_above_strip = legacy_chart_above_strip,
        legacy_strip = legacy_strip_html,
        legacy_agents = legacy_remainder,
    )
}

/// KPI block: 5 metrics + 4-component token breakdown. Lives at the very
/// top of Level 1 per the issue addendum.
pub fn render_kpi_block(k: &Kpi) -> String {
    format!(
        r#"<section class="panel-kpi">
  <header><h2>KPI · {period}</h2></header>
  <dl class="kpi-grid font-mono">
    <dt>closed</dt><dd>0 / p50=– ·</dd>
    <dt>tokens</dt><dd>{tokens_total}
      <small class="kpi-breakdown">in={tokens_in} out={tokens_out} cache_c={cache_c} cache_r={cache_r}</small>
    </dd>
    <dt>ratio</dt><dd>{ratio_str}</dd>
    <dt>pushback</dt><dd>0 / p50=– ·</dd>
    <dt>bugs</dt><dd>0 / p50=– ·</dd>
    <dt>sessions</dt><dd>{sessions} / p50=– ·</dd>
    <dt>limit-hits</dt><dd>{limit_hits} / p50=– ·</dd>
  </dl>
  <nav class="kpi-period">
    <a href="/?period=today">today</a>
    <a href="/?period=week">week</a>
    <a href="/?period=month">month</a>
  </nav>
</section>"#,
        period = esc(&k.period),
        tokens_total = esc(&fmt_short(k.tokens_total)),
        tokens_in = esc(&fmt_short(k.tokens_in)),
        tokens_out = esc(&fmt_short(k.tokens_out)),
        cache_c = esc(&fmt_short(k.cache_creation)),
        cache_r = esc(&fmt_short(k.cache_read)),
        ratio_str = esc(&render_ratio(k)),
        sessions = k.sessions,
        limit_hits = k.limit_hits,
    )
}

fn render_ratio(k: &Kpi) -> String {
    if k.tokens_total == 0 {
        "– / M".into()
    } else {
        let m_tokens = k.tokens_total as f64 / 1e6;
        let ratio = 0.0 / m_tokens.max(0.000_001); // closed=0 in v1; cron fills later.
        format!("{:.2} issues/M", ratio)
    }
}

fn render_model_rows(by_model: &std::collections::BTreeMap<String, ModelTokens>) -> String {
    if by_model.is_empty() {
        return r#"<p class="model-rows-empty"><small>no model usage yet</small></p>"#.into();
    }
    let mut rows = String::from(
        "<table class=\"model-rows font-mono\"><thead><tr><th>model</th><th>in</th><th>out</th><th>iter</th></tr></thead><tbody>",
    );
    for (m, t) in by_model.iter() {
        rows.push_str(&format!(
            "<tr><td>{m}</td><td>{ti}</td><td>{to}</td><td>{it}</td></tr>",
            m = esc(m),
            ti = esc(&fmt_short(t.tokens_in)),
            to = esc(&fmt_short(t.tokens_out)),
            it = t.iterations,
        ));
    }
    rows.push_str("</tbody></table>");
    rows
}

fn render_agent_rows(rows: &[AgentRollup]) -> String {
    if rows.is_empty() {
        return r#"<p class="agent-rows-empty"><small>no agent activity in this window</small></p>"#.into();
    }
    let mut out = String::from(
        "<table class=\"agent-rows font-mono\"><thead><tr><th>name</th><th>model</th><th>tokens</th><th>iter</th><th>⚠hits</th></tr></thead><tbody>",
    );
    for r in rows {
        out.push_str(&format!(
            r#"<tr class="agent-row" data-href="/agents/{a}">
<td><a href="/agents/{a}">{a}</a></td>
<td>{m}</td>
<td>{tok}</td>
<td>{it}</td>
<td>{hits}</td>
</tr>"#,
            a = esc(&r.agent),
            m = esc(&r.model),
            tok = esc(&fmt_short(r.total_tokens())),
            it = r.iterations,
            hits = r.limit_hits,
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Render the Level 2 body (within the shared shell) for `agent`,
/// `period` (24h / 7d / 30d), and the matching session list + sparkline.
pub fn level2_body(
    agent: &str,
    period_label: &str,
    rollup: &AgentRollup,
    sessions: &[SessionInfo],
    sparkline: &str,
) -> String {
    let tabs = format!(
        r#"<nav class="period-tabs">
  <a href="/agents/{a}?period=24h" class="tab{c24}">24h</a>
  <a href="/agents/{a}?period=7d" class="tab{c7}">7d</a>
  <a href="/agents/{a}?period=30d" class="tab{c30}">30d</a>
</nav>"#,
        a = esc(agent),
        c24 = if period_label == "24h" {
            " tab--active"
        } else {
            ""
        },
        c7 = if period_label == "7d" {
            " tab--active"
        } else {
            ""
        },
        c30 = if period_label == "30d" {
            " tab--active"
        } else {
            ""
        },
    );

    let sessions_html = if sessions.is_empty() {
        "<p><small>no sessions in window</small></p>".to_string()
    } else {
        let mut s = String::from(
            "<table class=\"sessions-list font-mono\"><thead><tr><th>started</th><th>session</th><th>tokens</th><th>iter</th></tr></thead><tbody>",
        );
        for sess in sessions {
            let started = sess
                .started_at
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "—".to_string());
            s.push_str(&format!(
                r#"<tr class="session-row">
<td>{started}</td>
<td><a href="/agents/{a}/sessions/{sid}">{sid_short}</a></td>
<td>{tok}</td>
<td>{it}</td>
</tr>"#,
                started = esc(&started),
                a = esc(agent),
                sid = esc(&sess.session_id),
                sid_short = esc(&truncate(&sess.session_id, 12)),
                tok = esc(&fmt_short(sess.total_tokens())),
                it = sess.iterations,
            ));
        }
        s.push_str("</tbody></table>");
        s
    };

    format!(
        r#"<main class="panel container">
<nav class="breadcrumb"><a href="/">← overview</a></nav>
<header><h1>agent · {a}</h1></header>
{tabs}
<section class="agent-rollup font-mono">
  <p>tokens={tok} · iter={it} · sessions={se} · ⚠hits={hits} · model={model}</p>
</section>
<section class="agent-sparkline">
  <header><h2>Activity ({period})</h2></header>
  <p class="font-mono">{sp}</p>
</section>
<section class="agent-sessions">
  <header><h2>Sessions</h2></header>
  {sessions_html}
</section>
</main>"#,
        a = esc(agent),
        tabs = tabs,
        tok = esc(&fmt_short(rollup.total_tokens())),
        it = rollup.iterations,
        se = rollup.sessions,
        hits = rollup.limit_hits,
        model = esc(&rollup.model),
        period = esc(period_label),
        sp = esc(sparkline),
        sessions_html = sessions_html,
    )
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

/// Render Level 3 transcript body (parsed view). The `view raw` link
/// drops to `?raw=1`.
pub fn level3_body(
    agent: &str,
    session_id: &str,
    lines: &[crate::app::adapters::web::transcript_view::TranscriptLine],
) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        r#"<main class="panel container">
<nav class="breadcrumb"><a href="/">overview</a> · <a href="/agents/{a}">{a}</a> · session</nav>
<header><h1>session · {sid_short}</h1>
<p><a href="/agents/{a}/sessions/{sid}?raw=1">view raw</a></p>
</header>
<pre class="transcript font-mono">"#,
        a = esc(agent),
        sid = esc(session_id),
        sid_short = esc(&truncate(session_id, 16)),
    ));
    for l in lines {
        body.push_str(&esc(&l.prefix));
        body.push_str(&esc(&l.text));
        body.push('\n');
    }
    body.push_str("</pre>\n</main>");
    body
}

/// Render Level 3 raw view (unparsed JSONL).
pub fn level3_raw_body(agent: &str, session_id: &str, raw: &str) -> String {
    format!(
        r#"<main class="panel container">
<nav class="breadcrumb"><a href="/">overview</a> · <a href="/agents/{a}">{a}</a> · session (raw)</nav>
<header><h1>session · {sid_short} <small>raw</small></h1>
<p><a href="/agents/{a}/sessions/{sid}">parsed view</a></p>
</header>
<pre class="transcript-raw font-mono">{raw}</pre>
</main>"#,
        a = esc(agent),
        sid = esc(session_id),
        sid_short = esc(&truncate(session_id, 16)),
        raw = esc(raw),
    )
}

/// Render the full HTML shell with the Panel v2 head (Pico.css + dashboard.css).
/// `body_html` is the inner page-level markup. Existing dashboard tests
/// check for `htmx.min.js`, `htmx-sse.js`, `/static/dashboard.css`, the
/// session owner's `tg:<id>` and a CSRF-protected logout form — we preserve
/// all of those markers.
pub fn shell_page(
    title: &str,
    telegram_id: i64,
    csrf: &str,
    body_html: &str,
    uptime_human: &str,
) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>deskd · {title}</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="stylesheet" href="/static/pico.min.css">
<link rel="stylesheet" href="/static/dashboard.css">
<script src="/static/htmx.min.js"></script>
<script src="/static/htmx-sse.js"></script>
<script src="/static/panel.js" defer></script>
</head>
<body>
<header class="container topbar">
  <hgroup>
    <h1>deskd · panel</h1>
    <p><small>uptime {uptime} · tg:{telegram_id}</small></p>
  </hgroup>
  <form class="topbar__logout" method="post" action="/logout">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit">Log out</button>
  </form>
</header>
{body}
</body>
</html>"#,
        title = esc(title),
        uptime = esc(uptime_human),
        telegram_id = telegram_id,
        csrf = esc(csrf),
        body = body_html,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::adapters::web::event_query::{Kpi, ModelTokens, WeeklyTokens};

    #[test]
    fn fmt_short_rounds_consistently() {
        assert_eq!(fmt_short(0), "0");
        assert_eq!(fmt_short(999), "999");
        assert_eq!(fmt_short(1_500), "1.5k");
        assert_eq!(fmt_short(1_500_000), "1.5M");
        assert_eq!(fmt_short(2_300_000_000), "2.3B");
    }

    #[test]
    fn fmt_u64_thousand_separators() {
        assert_eq!(fmt_u64(0), "0");
        assert_eq!(fmt_u64(1_234), "1,234");
        assert_eq!(fmt_u64(1_234_567), "1,234,567");
    }

    #[test]
    fn level1_body_contains_required_markers() {
        let kpi = Kpi {
            period: "today".into(),
            ..Default::default()
        };
        let weekly = WeeklyTokens::default();
        let body = level1_body(
            &kpi,
            &weekly,
            100_000_000,
            "····",
            &[],
            "<section class='vps-strip'></section>",
            "",
        );
        assert!(body.contains("Weekly tokens"));
        assert!(body.contains("KPI"));
        assert!(body.contains("limit-hits this week"));
        assert!(body.contains("vps-strip"));
        assert!(body.contains("sse-connect=\"/events\""));
        // Tier class included.
        assert!(body.contains("gauge--green"));
    }

    #[test]
    fn level1_body_renders_per_model_rows() {
        let kpi = Kpi {
            period: "today".into(),
            ..Default::default()
        };
        let mut weekly = WeeklyTokens::default();
        weekly.by_model.insert(
            "opus".into(),
            ModelTokens {
                tokens_in: 100,
                tokens_out: 50,
                iterations: 3,
            },
        );
        let body = level1_body(&kpi, &weekly, 100_000_000, "····", &[], "", "");
        assert!(body.contains("opus"));
        assert!(body.contains("model-rows"));
    }

    #[test]
    fn level2_body_renders_period_tabs_and_sessions() {
        let rollup = AgentRollup {
            agent: "dev".into(),
            model: "claude-opus-4-7".into(),
            tokens_in: 100,
            tokens_out: 50,
            iterations: 3,
            cost_usd: 0.0,
            limit_hits: 0,
            sessions: 1,
        };
        let sessions = vec![SessionInfo {
            session_id: "S1".into(),
            started_at: None,
            ended_at: None,
            model: "claude-opus-4-7".into(),
            tokens_in: 100,
            tokens_out: 50,
            iterations: 3,
        }];
        let body = level2_body("dev", "7d", &rollup, &sessions, "·····");
        assert!(body.contains("agent · dev"));
        assert!(body.contains(r#"href="/agents/dev?period=24h""#));
        assert!(body.contains(r#"href="/agents/dev?period=7d""#));
        assert!(body.contains(r#"href="/agents/dev/sessions/S1""#));
    }

    #[test]
    fn level3_body_renders_transcript_lines() {
        use crate::app::adapters::web::transcript_view::TranscriptLine;
        let lines = vec![
            TranscriptLine {
                prefix: "→ ".into(),
                text: "hello".into(),
            },
            TranscriptLine {
                prefix: "← ".into(),
                text: "world".into(),
            },
        ];
        let body = level3_body("dev", "S1", &lines);
        assert!(body.contains("→ hello"));
        assert!(body.contains("← world"));
        assert!(body.contains("view raw"));
    }

    #[test]
    fn level3_raw_body_renders_raw_jsonl() {
        let raw = "{\"type\":\"user\"}\n";
        let body = level3_raw_body("dev", "S1", raw);
        assert!(body.contains("parsed view"));
        assert!(body.contains("&quot;type&quot;"));
    }

    #[test]
    fn shell_page_contains_pico_and_dashboard_links() {
        let html = shell_page("test", 42, "csrf-x", "<p>x</p>", "1m");
        assert!(html.contains(r#"<link rel="stylesheet" href="/static/pico.min.css">"#));
        assert!(html.contains(r#"<link rel="stylesheet" href="/static/dashboard.css">"#));
        assert!(html.contains(r#"src="/static/htmx.min.js""#));
        assert!(html.contains(r#"src="/static/htmx-sse.js""#));
        assert!(html.contains("uptime 1m"));
        assert!(html.contains("tg:42"));
        assert!(html.contains(r#"action="/logout""#));
        assert!(html.contains(r#"value="csrf-x""#));
    }

    #[test]
    fn no_inline_style_block_in_panel_html() {
        let kpi = Kpi {
            period: "today".into(),
            ..Default::default()
        };
        let weekly = WeeklyTokens::default();
        let body = level1_body(&kpi, &weekly, 100_000_000, "····", &[], "", "");
        let full = shell_page("test", 42, "csrf-x", &body, "1m");
        let lower = full.to_ascii_lowercase();
        assert!(!lower.contains("<style>"));
        assert!(!lower.contains("<style "));
        assert!(!lower.contains("style=\""));
        assert!(!lower.contains("style='"));
    }

    #[test]
    fn agent_rows_use_data_href_not_onclick() {
        // #516: full-row navigation is wired via a delegated handler in
        // `/static/panel.js` that reads `data-href`; no inline `onclick`
        // attributes may appear (strict CSP from #443).
        let rows = vec![AgentRollup {
            agent: "dev".into(),
            model: "claude-opus-4-7".into(),
            tokens_in: 10,
            tokens_out: 5,
            iterations: 1,
            cost_usd: 0.0,
            limit_hits: 0,
            sessions: 1,
        }];
        let html = render_agent_rows(&rows);
        assert!(!html.contains("onclick"), "no inline onclick allowed");
        assert!(html.contains(r#"data-href="/agents/dev""#));
        assert!(html.contains(r#"class="agent-row""#));
    }

    #[test]
    fn shell_page_links_panel_js_for_delegated_click() {
        // #516: the shell must pull in `/static/panel.js` so the delegated
        // row-click handler runs alongside the existing htmx scripts.
        let html = shell_page("test", 42, "csrf-x", "<p>x</p>", "1m");
        assert!(html.contains(r#"src="/static/panel.js""#));
    }
}
