//! Agent-as-file: load standalone markdown files with YAML frontmatter as
//! sub-agent definitions.
//!
//! Each file describes one agent: identity (`name`), runtime config (model,
//! subscribe, etc.), `jobs` (cron + prompt), and a system-prompt body.
//!
//! Layout:
//! ```text
//! ---
//! name: blog
//! model: claude-sonnet-4-6
//! subscribe:
//!   - "agent:blog"
//! jobs:
//!   - cron: "0 30 8 * * *"
//!     prompt: |
//!       Morning check-in...
//! ---
//!
//! You are Konstantin's blog manager. ...
//! ```
//!
//! `jobs` are translated to `ScheduleDef` entries targeting `agent:<name>`
//! with `action: raw` and the prompt as the payload — the same shape the
//! schedule runner already understands.
//!
//! See issue kgatilin/deskd#370.
//!
//! Conventions:
//! - File extension `.agent.md` (filename stem minus `.agent` is the
//!   default `name` if frontmatter omits it).
//! - The body (everything after the closing `---`) becomes the agent's
//!   `system_prompt`, overriding any `system_prompt` set in frontmatter.
//! - `subscribe` defaults to `["agent:<name>"]` if not provided.

use crate::config::{ScheduleAction, ScheduleDef, SubAgentDef};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::path::Path;

/// A single cron-driven job declared in an agent file.
#[derive(Debug, Clone, Deserialize)]
struct AgentJob {
    cron: String,
    prompt: String,
    #[serde(default)]
    timezone: Option<String>,
}

/// Result of loading a single agent file: the sub-agent definition plus
/// any schedules derived from its `jobs` block.
#[derive(Debug, Clone)]
pub struct LoadedAgentFile {
    pub agent: SubAgentDef,
    pub schedules: Vec<ScheduleDef>,
}

/// Load every `*.agent.md` file in `dir` (non-recursive), returning the
/// derived agent + schedule definitions. Files are processed in sorted
/// order for deterministic output. Missing or non-directory paths return
/// an empty vec rather than an error so config bootstrapping is forgiving.
pub fn load_agent_dir(dir: &Path) -> Result<Vec<LoadedAgentFile>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    if !dir.is_dir() {
        anyhow::bail!("agents_dir is not a directory: {}", dir.display());
    }

    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read agents_dir: {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.ends_with(".agent.md"))
                    .unwrap_or(false)
        })
        .collect();
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let loaded = load_agent_file(&path)
            .with_context(|| format!("failed to load agent file: {}", path.display()))?;
        out.push(loaded);
    }
    Ok(out)
}

/// Load a single agent file from `path`.
pub fn load_agent_file(path: &Path) -> Result<LoadedAgentFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read agent file: {}", path.display()))?;
    let (frontmatter, body) = split_frontmatter(&content)?;

    let default_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|name| {
            name.strip_suffix(".agent.md")
                .unwrap_or_else(|| name.strip_suffix(".md").unwrap_or(name))
        })
        .unwrap_or("")
        .to_string();

    parse_agent(frontmatter, body, &default_name)
}

/// Parse frontmatter + body into a `LoadedAgentFile`. `default_name` is
/// used when the frontmatter omits `name`.
fn parse_agent(frontmatter: &str, body: &str, default_name: &str) -> Result<LoadedAgentFile> {
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(frontmatter).context("failed to parse agent frontmatter as YAML")?;

    // Empty frontmatter → empty mapping. serde_yaml gives Value::Null for
    // an empty document; promote to a mapping so downstream lookups work.
    if value.is_null() {
        value = serde_yaml::Value::Mapping(Default::default());
    }

    let map = value
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("agent frontmatter must be a YAML mapping"))?;

    // Pull `jobs` out before deserializing into SubAgentDef — it's not a
    // SubAgentDef field.
    let jobs: Vec<AgentJob> = match map.remove("jobs") {
        Some(jobs_val) => serde_yaml::from_value(jobs_val).context("invalid `jobs` block")?,
        None => Vec::new(),
    };

    // Default name from filename stem if not in frontmatter.
    let name_key = serde_yaml::Value::String("name".to_string());
    if !map.contains_key(&name_key) {
        if default_name.is_empty() {
            anyhow::bail!("agent file has no `name` and filename does not provide a default");
        }
        map.insert(name_key.clone(), default_name.into());
    }
    let name: String = map
        .get(&name_key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("agent `name` must be a string"))?
        .to_string();

    // Default subscribe to ["agent:<name>"] when absent (keeps SubAgentDef
    // happy — `subscribe` has no #[serde(default)]).
    let subscribe_key = serde_yaml::Value::String("subscribe".to_string());
    if !map.contains_key(&subscribe_key) {
        let target = serde_yaml::Value::String(format!("agent:{}", name));
        map.insert(subscribe_key, serde_yaml::Value::Sequence(vec![target]));
    }

    // Body overrides any frontmatter system_prompt.
    let body_trimmed = body.trim();
    if !body_trimmed.is_empty() {
        map.insert(
            serde_yaml::Value::String("system_prompt".to_string()),
            serde_yaml::Value::String(body_trimmed.to_string()),
        );
    }

    let agent: SubAgentDef = serde_yaml::from_value(value)
        .context("agent frontmatter does not match SubAgentDef schema")?;

    let schedules = jobs
        .into_iter()
        .map(|job| ScheduleDef {
            cron: job.cron,
            target: format!("agent:{}", agent.name),
            action: ScheduleAction::Raw,
            // `fire_raw` accepts a scalar string as the payload directly.
            config: Some(serde_yaml::Value::String(job.prompt)),
            timezone: job.timezone,
        })
        .collect();

    Ok(LoadedAgentFile { agent, schedules })
}

/// Split a markdown-with-frontmatter document into (frontmatter, body).
/// The opening `---` must be the very first line; the closing `---` must
/// appear on its own line. Both delimiters are excluded from the returned
/// slices.
fn split_frontmatter(content: &str) -> Result<(&str, &str)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);

    let mut lines = content.split_inclusive('\n');
    let first = lines.next().ok_or_else(|| anyhow!("agent file is empty"))?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        anyhow::bail!("agent file must start with '---' frontmatter delimiter");
    }

    let frontmatter_start = first.len();
    let mut cursor = frontmatter_start;
    let mut body_start = None;

    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            body_start = Some(cursor + line.len());
            break;
        }
        cursor += line.len();
    }

    let body_start = body_start
        .ok_or_else(|| anyhow!("agent file is missing a closing '---' frontmatter delimiter"))?;

    Ok((&content[frontmatter_start..cursor], &content[body_start..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn split_frontmatter_basic() {
        let doc = "---\nname: foo\n---\nbody text\n";
        let (fm, body) = split_frontmatter(doc).unwrap();
        assert_eq!(fm, "name: foo\n");
        assert_eq!(body, "body text\n");
    }

    #[test]
    fn split_frontmatter_crlf() {
        let doc = "---\r\nname: foo\r\n---\r\nbody\r\n";
        let (fm, body) = split_frontmatter(doc).unwrap();
        assert_eq!(fm, "name: foo\r\n");
        assert_eq!(body, "body\r\n");
    }

    #[test]
    fn split_frontmatter_empty_body() {
        let doc = "---\nname: foo\n---\n";
        let (fm, body) = split_frontmatter(doc).unwrap();
        assert_eq!(fm, "name: foo\n");
        assert_eq!(body, "");
    }

    #[test]
    fn split_frontmatter_missing_open() {
        let err = split_frontmatter("name: foo\n---\nbody").unwrap_err();
        assert!(err.to_string().contains("must start with '---'"));
    }

    #[test]
    fn split_frontmatter_missing_close() {
        let err = split_frontmatter("---\nname: foo\nno close\n").unwrap_err();
        assert!(err.to_string().contains("missing a closing"));
    }

    #[test]
    fn split_frontmatter_strips_bom() {
        let doc = "\u{feff}---\nname: foo\n---\nbody\n";
        let (fm, body) = split_frontmatter(doc).unwrap();
        assert_eq!(fm, "name: foo\n");
        assert_eq!(body, "body\n");
    }

    #[test]
    fn parse_minimal_agent() {
        let fm = "model: claude-sonnet-4-6\n";
        let body = "You are blog.\n";
        let loaded = parse_agent(fm, body, "blog").unwrap();
        assert_eq!(loaded.agent.name, "blog");
        assert_eq!(loaded.agent.model, "claude-sonnet-4-6");
        assert_eq!(loaded.agent.subscribe, vec!["agent:blog".to_string()]);
        assert_eq!(loaded.agent.system_prompt, "You are blog.");
        assert!(loaded.schedules.is_empty());
    }

    #[test]
    fn frontmatter_name_overrides_filename() {
        let fm = "name: explicit\nmodel: claude-sonnet-4-6\n";
        let loaded = parse_agent(fm, "", "from-filename").unwrap();
        assert_eq!(loaded.agent.name, "explicit");
    }

    #[test]
    fn jobs_become_schedules() {
        let fm = r#"
model: claude-sonnet-4-6
jobs:
  - cron: "0 30 8 * * *"
    prompt: "Morning check-in"
  - cron: "0 0 12 * * 5"
    prompt: "Weekly review"
    timezone: "Europe/Berlin"
"#;
        let loaded = parse_agent(fm, "", "blog").unwrap();
        assert_eq!(loaded.schedules.len(), 2);
        assert_eq!(loaded.schedules[0].cron, "0 30 8 * * *");
        assert_eq!(loaded.schedules[0].target, "agent:blog");
        assert!(matches!(loaded.schedules[0].action, ScheduleAction::Raw));
        assert_eq!(
            loaded.schedules[0].config,
            Some(serde_yaml::Value::String("Morning check-in".to_string()))
        );
        assert!(loaded.schedules[0].timezone.is_none());
        assert_eq!(
            loaded.schedules[1].timezone.as_deref(),
            Some("Europe/Berlin")
        );
    }

    #[test]
    fn body_overrides_frontmatter_system_prompt() {
        let fm = "model: claude-sonnet-4-6\nsystem_prompt: \"old\"\n";
        let body = "new prompt from body\n";
        let loaded = parse_agent(fm, body, "blog").unwrap();
        assert_eq!(loaded.agent.system_prompt, "new prompt from body");
    }

    #[test]
    fn explicit_subscribe_preserved() {
        let fm = r#"
model: claude-sonnet-4-6
subscribe:
  - "telegram.in:*"
  - "agent:blog"
"#;
        let loaded = parse_agent(fm, "", "blog").unwrap();
        assert_eq!(
            loaded.agent.subscribe,
            vec!["telegram.in:*".to_string(), "agent:blog".to_string()]
        );
    }

    #[test]
    fn empty_frontmatter_uses_filename_and_defaults() {
        // Only required field is `model` → an empty frontmatter would fail
        // SubAgentDef parsing. Verify the error is descriptive.
        let err = parse_agent("", "body", "blog").unwrap_err();
        assert!(
            err.to_string().contains("SubAgentDef")
                || err.to_string().contains("missing field")
                || format!("{:#}", err).contains("model")
        );
    }

    #[test]
    fn load_agent_dir_returns_empty_when_missing() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let loaded = load_agent_dir(&missing).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn load_agent_dir_picks_up_files_sorted() {
        let tmp = TempDir::new().unwrap();
        for stem in ["zeta", "alpha", "mid"] {
            let path = tmp.path().join(format!("{}.agent.md", stem));
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "---\nmodel: claude-sonnet-4-6\n---\nbody for {}", stem).unwrap();
        }
        // A non-agent file should be ignored.
        std::fs::write(tmp.path().join("README.md"), "# readme").unwrap();

        let loaded = load_agent_dir(tmp.path()).unwrap();
        let names: Vec<_> = loaded.iter().map(|l| l.agent.name.clone()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn load_agent_file_uses_filename_stem_for_name() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("blog.agent.md");
        std::fs::write(&path, "---\nmodel: claude-sonnet-4-6\n---\nYou are blog.\n").unwrap();
        let loaded = load_agent_file(&path).unwrap();
        assert_eq!(loaded.agent.name, "blog");
    }
}
