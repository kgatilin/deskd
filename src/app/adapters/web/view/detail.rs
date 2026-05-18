//! Per-agent disk detail rendering (#446).
//!
//! Used by `GET /agent/<name>` to surface the home-dir size and the
//! top-5 subdirectory breakdown.

use chrono::{DateTime, Utc};

use crate::app::metrics::AgentBreakdownEntry;

use super::cards::format_relative;
use super::html_escape;

/// Render the disk-detail block for one agent. `format_bytes` is taken as
/// a function pointer so this module doesn't depend on the cards layout.
pub fn agent_disk_detail_html(
    home_dir: Option<&str>,
    total_bytes: Option<u64>,
    updated_at: Option<DateTime<Utc>>,
    breakdown: &[AgentBreakdownEntry],
    format_bytes: fn(u64) -> String,
) -> String {
    let home_label = home_dir.map(html_escape).unwrap_or_else(|| "—".to_string());
    let total_label = total_bytes
        .map(format_bytes)
        .unwrap_or_else(|| "—".to_string());
    let updated_label = updated_at
        .map(format_relative)
        .unwrap_or_else(|| "—".to_string());

    let breakdown_html = if breakdown.is_empty() {
        r#"<p class="agent-disk__empty">No subdirectory data available.</p>"#.to_string()
    } else {
        let mut items = String::new();
        for entry in breakdown {
            items.push_str(&format!(
                r#"    <li class="agent-disk__row"><span class="agent-disk__name">{name}</span> <span class="agent-disk__bytes">{bytes}</span></li>
"#,
                name = html_escape(&entry.name),
                bytes = format_bytes(entry.bytes),
            ));
        }
        format!(
            r#"<ol class="agent-disk__breakdown">
{items}</ol>"#,
        )
    };

    format!(
        r#"<section class="agent-disk">
  <dl class="agent-disk__summary">
    <dt>home dir</dt><dd><code>{home}</code></dd>
    <dt>total</dt><dd>{total}</dd>
    <dt>updated</dt><dd title="{updated_full}">{updated_short}</dd>
  </dl>
  <h3>top {n} subdirectories</h3>
  {breakdown_html}
</section>"#,
        home = home_label,
        total = total_label,
        updated_full = updated_at
            .map(|t| html_escape(&t.to_rfc3339()))
            .unwrap_or_default(),
        updated_short = html_escape(&updated_label),
        n = breakdown.len().max(1),
        breakdown_html = breakdown_html,
    )
}

#[cfg(test)]
mod tests {
    use super::super::cards::format_bytes;
    use super::*;
    use crate::app::metrics::AgentBreakdownEntry;

    #[test]
    fn renders_em_dash_when_no_data() {
        let html = agent_disk_detail_html(None, None, None, &[], format_bytes);
        assert!(html.contains("—"));
        assert!(html.contains("No subdirectory data available"));
    }

    #[test]
    fn lists_breakdown_largest_first() {
        let breakdown = vec![
            AgentBreakdownEntry {
                name: ".cargo".into(),
                bytes: 800 * 1024 * 1024,
            },
            AgentBreakdownEntry {
                name: "projects".into(),
                bytes: 400 * 1024 * 1024,
            },
        ];
        let html = agent_disk_detail_html(
            Some("/home/kira"),
            Some(1_200 * 1024 * 1024),
            None,
            &breakdown,
            format_bytes,
        );
        assert!(html.contains("<code>/home/kira</code>"));
        assert!(html.contains(".cargo"));
        assert!(html.contains("projects"));
        // .cargo is listed before projects.
        let i_cargo = html.find(".cargo").unwrap();
        let i_proj = html.find("projects").unwrap();
        assert!(i_cargo < i_proj);
    }

    #[test]
    fn updated_tooltip_carries_rfc3339() {
        let when: chrono::DateTime<chrono::Utc> =
            chrono::DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
        let html =
            agent_disk_detail_html(Some("/home/kira"), Some(0), Some(when), &[], format_bytes);
        // tooltip uses title="..." with the full RFC 3339 timestamp.
        assert!(html.contains(r#"title="2026-01-01T12:00:00+00:00""#));
    }

    #[test]
    fn escapes_subdir_names_for_xss_safety() {
        let breakdown = vec![AgentBreakdownEntry {
            name: "<x>".into(),
            bytes: 1,
        }];
        let html = agent_disk_detail_html(None, None, None, &breakdown, format_bytes);
        assert!(html.contains("&lt;x&gt;"));
        assert!(!html.contains("<x>"));
    }
}
