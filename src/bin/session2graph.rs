//! session2graph — Convert Claude Code session JSONL files into archlint-compatible
//! architecture.yaml format.
//!
//! Usage: session2graph <input.jsonl> [--output output.yaml]

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "session2graph",
    about = "Convert Claude Code session JSONL to architecture.yaml"
)]
struct Cli {
    /// Path to the session JSONL file
    input: PathBuf,

    /// Output file path (default: stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,
}

// ── Input types (JSONL) ──────────────────────────────────────────────

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SessionLine {
    #[serde(default)]
    #[allow(dead_code)]
    r#type: String,
    #[serde(default)]
    #[allow(dead_code)]
    uuid: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    parent_uuid: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize, Debug)]
struct Message {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: MessageContent,
}

#[derive(Deserialize, Debug, Default)]
#[serde(untagged)]
enum MessageContent {
    #[default]
    Empty,
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: serde_json::Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(other)]
    Unknown,
}

// ── Output types (YAML) ─────────────────────────────────────────────

#[derive(Serialize)]
struct ArchGraph {
    components: Vec<Component>,
    links: Vec<Link>,
}

#[derive(Serialize)]
struct Component {
    id: String,
    title: String,
    entity: String,
}

#[derive(Serialize)]
struct Link {
    from: String,
    to: String,
    r#type: String,
}

// ── Conversion ───────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.len() <= max {
        s
    } else {
        format!("{}...", &s[..max])
    }
}

fn tool_use_title(name: &str, input: &serde_json::Value) -> String {
    let args = match input {
        serde_json::Value::Object(map) => {
            let keys: Vec<&str> = map.keys().take(2).map(|k| k.as_str()).collect();
            let parts: Vec<String> = keys
                .iter()
                .filter_map(|k| {
                    map.get(*k).map(|v| match v {
                        serde_json::Value::String(s) => truncate(s, 30),
                        _ => truncate(&v.to_string(), 30),
                    })
                })
                .collect();
            parts.join(", ")
        }
        _ => String::new(),
    };
    format!("tool_use: {name}({args})")
}

fn tool_result_title(content: &serde_json::Value) -> String {
    let len = match content {
        serde_json::Value::String(s) => s.len(),
        serde_json::Value::Array(arr) => {
            // Sum up text content blocks
            arr.iter()
                .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                .map(|s| s.len())
                .sum()
        }
        _ => content.to_string().len(),
    };
    format!("tool_result: [{len} chars]")
}

struct GraphBuilder {
    components: Vec<Component>,
    links: Vec<Link>,
    counter: usize,
    // Map tool_use id -> component id for linking results back
    tool_use_ids: HashMap<String, String>,
    // Track last component id per message for chaining
    last_assistant_id: Option<String>,
    last_user_id: Option<String>,
}

impl GraphBuilder {
    fn new() -> Self {
        Self {
            components: Vec::new(),
            links: Vec::new(),
            counter: 0,
            tool_use_ids: HashMap::new(),
            last_assistant_id: None,
            last_user_id: None,
        }
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}_{:03}", self.counter)
    }

    fn process_line(&mut self, line: &SessionLine) {
        let message = match &line.message {
            Some(m) => m,
            None => return,
        };

        let owned_blocks;
        let blocks: &[ContentBlock] = match &message.content {
            MessageContent::Blocks(b) => b,
            MessageContent::Text(s) => {
                owned_blocks = vec![ContentBlock::Text { text: s.clone() }];
                &owned_blocks
            }
            MessageContent::Empty => return,
        };

        // Track whether we produced any components from this message to link via parentUuid
        let mut first_component_of_message: Option<String> = None;

        for block in blocks {
            match block {
                ContentBlock::Text { text } if message.role == "user" => {
                    let id = self.next_id("msg");
                    let title = format!("user: {}", truncate(text, 60));

                    // Link from previous assistant to this user (conversation flow)
                    if let Some(ref prev) = self.last_assistant_id {
                        self.links.push(Link {
                            from: prev.clone(),
                            to: id.clone(),
                            r#type: "response".into(),
                        });
                    }

                    self.components.push(Component {
                        id: id.clone(),
                        title,
                        entity: "user_message".into(),
                    });
                    self.last_user_id = Some(id.clone());
                    if first_component_of_message.is_none() {
                        first_component_of_message = Some(id);
                    }
                }
                ContentBlock::Text { text } if message.role == "assistant" => {
                    let id = self.next_id("msg");
                    let title = format!("assistant: {}", truncate(text, 60));

                    // Link user -> assistant
                    if let Some(ref prev) = self.last_user_id {
                        self.links.push(Link {
                            from: prev.clone(),
                            to: id.clone(),
                            r#type: "response".into(),
                        });
                    }

                    self.components.push(Component {
                        id: id.clone(),
                        title,
                        entity: "assistant_message".into(),
                    });
                    self.last_assistant_id = Some(id.clone());
                    if first_component_of_message.is_none() {
                        first_component_of_message = Some(id);
                    }
                }
                ContentBlock::ToolUse {
                    id: tid,
                    name,
                    input,
                } => {
                    let comp_id = self.next_id("tool");
                    let title = tool_use_title(name, input);

                    // Link assistant -> tool_use
                    if let Some(ref prev) = self.last_assistant_id {
                        self.links.push(Link {
                            from: prev.clone(),
                            to: comp_id.clone(),
                            r#type: "invocation".into(),
                        });
                    }

                    self.tool_use_ids.insert(tid.clone(), comp_id.clone());
                    self.components.push(Component {
                        id: comp_id.clone(),
                        title,
                        entity: "tool_use".into(),
                    });
                    if first_component_of_message.is_none() {
                        first_component_of_message = Some(comp_id);
                    }
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                } => {
                    let comp_id = format!("{}r", self.next_id("tool"));
                    let title = tool_result_title(content);

                    // Link tool_use -> tool_result
                    if let Some(from_id) = self.tool_use_ids.get(tool_use_id) {
                        self.links.push(Link {
                            from: from_id.clone(),
                            to: comp_id.clone(),
                            r#type: "result".into(),
                        });
                    }

                    self.components.push(Component {
                        id: comp_id.clone(),
                        title,
                        entity: "tool_result".into(),
                    });

                    // tool_result feeds into next assistant turn — track it
                    // so the next assistant text can link from it
                    self.last_user_id = Some(comp_id.clone());
                    if first_component_of_message.is_none() {
                        first_component_of_message = Some(comp_id);
                    }
                }
                ContentBlock::Thinking { thinking } => {
                    let id = self.next_id("msg");
                    let title = format!("thinking: {}", truncate(thinking, 60));

                    if let Some(ref prev) = self.last_user_id {
                        self.links.push(Link {
                            from: prev.clone(),
                            to: id.clone(),
                            r#type: "response".into(),
                        });
                    }

                    self.components.push(Component {
                        id: id.clone(),
                        title,
                        entity: "thinking".into(),
                    });
                    self.last_assistant_id = Some(id.clone());
                    if first_component_of_message.is_none() {
                        first_component_of_message = Some(id);
                    }
                }
                _ => {}
            }
        }
    }

    fn build(self) -> ArchGraph {
        ArchGraph {
            components: self.components,
            links: self.links,
        }
    }
}

fn convert(input: &str) -> Result<ArchGraph> {
    let mut builder = GraphBuilder::new();

    for (i, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionLine>(line) {
            Ok(session_line) => builder.process_line(&session_line),
            Err(e) => {
                eprintln!("warning: skipping line {}: {e}", i + 1);
            }
        }
    }

    Ok(builder.build())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let input = fs::read_to_string(&cli.input)
        .with_context(|| format!("failed to read {}", cli.input.display()))?;

    let graph = convert(&input)?;
    let yaml = serde_yaml::to_string(&graph).context("failed to serialize YAML")?;

    match cli.output {
        Some(path) => {
            fs::write(&path, &yaml)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        None => {
            io::stdout().write_all(yaml.as_bytes())?;
        }
    }

    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"type":"user","uuid":"u1","parentUuid":null,"message":{"role":"user","content":[{"type":"text","text":"fix the login bug in auth.rs"}]},"timestamp":"2025-01-01T00:00:00Z"}
{"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"text","text":"I'll look at auth.rs to find the login bug."},{"type":"tool_use","id":"tu1","name":"Read","input":{"file_path":"src/auth.rs"}}]},"timestamp":"2025-01-01T00:00:01Z"}
{"type":"user","uuid":"u2","parentUuid":"a1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"fn login() { ... }"}]},"timestamp":"2025-01-01T00:00:02Z"}
{"type":"assistant","uuid":"a2","parentUuid":"u2","message":{"role":"assistant","content":[{"type":"text","text":"Found the bug — the password check is missing. Let me fix it."},{"type":"tool_use","id":"tu2","name":"Edit","input":{"file_path":"src/auth.rs","old_string":"fn login()","new_string":"fn login_fixed()"}}]},"timestamp":"2025-01-01T00:00:03Z"}
"#;

    #[test]
    fn test_basic_conversion() {
        let graph = convert(FIXTURE).unwrap();

        // Should have: user_msg, assistant_msg, tool_use(Read), tool_result, assistant_msg, tool_use(Edit)
        assert_eq!(graph.components.len(), 6);

        // Check entity types
        assert_eq!(graph.components[0].entity, "user_message");
        assert_eq!(graph.components[1].entity, "assistant_message");
        assert_eq!(graph.components[2].entity, "tool_use");
        assert_eq!(graph.components[3].entity, "tool_result");
        assert_eq!(graph.components[4].entity, "assistant_message");
        assert_eq!(graph.components[5].entity, "tool_use");

        // Check titles
        assert!(
            graph.components[0]
                .title
                .starts_with("user: fix the login bug")
        );
        assert!(graph.components[2].title.contains("Read"));
        assert!(graph.components[3].title.contains("chars]"));
        assert!(graph.components[5].title.contains("Edit"));

        // Check links exist
        assert!(!graph.links.is_empty());

        // user -> assistant response link
        let first_link = &graph.links[0];
        assert_eq!(first_link.r#type, "response");

        // assistant -> tool invocation link
        let invocation_links: Vec<_> = graph
            .links
            .iter()
            .filter(|l| l.r#type == "invocation")
            .collect();
        assert_eq!(invocation_links.len(), 2); // Read and Edit

        // tool_use -> tool_result link
        let result_links: Vec<_> = graph
            .links
            .iter()
            .filter(|l| l.r#type == "result")
            .collect();
        assert_eq!(result_links.len(), 1);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 60), "short");
        let long = "a".repeat(100);
        let result = truncate(&long, 60);
        assert!(result.len() <= 64); // 60 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_tool_result_title() {
        let content = serde_json::Value::String("hello world".into());
        assert_eq!(tool_result_title(&content), "tool_result: [11 chars]");
    }

    #[test]
    fn test_empty_input() {
        let graph = convert("").unwrap();
        assert!(graph.components.is_empty());
        assert!(graph.links.is_empty());
    }

    #[test]
    fn test_yaml_output_format() {
        let graph = convert(FIXTURE).unwrap();
        let yaml = serde_yaml::to_string(&graph).unwrap();

        // Should contain expected keys
        assert!(yaml.contains("components:"));
        assert!(yaml.contains("links:"));
        assert!(yaml.contains("entity: user_message"));
        assert!(yaml.contains("entity: tool_use"));
        assert!(yaml.contains("type: response"));
        assert!(yaml.contains("type: invocation"));
        assert!(yaml.contains("type: result"));
    }

    #[test]
    fn test_thinking_block() {
        let input = r#"{"type":"user","uuid":"u1","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think about this carefully..."},{"type":"text","text":"Here is my answer."}]}}"#;

        let graph = convert(input).unwrap();
        let thinking = graph
            .components
            .iter()
            .find(|c| c.entity == "thinking")
            .expect("should have thinking component");
        assert!(thinking.title.starts_with("thinking: Let me think"));
    }

    #[test]
    fn test_skips_malformed_lines() {
        let input = "not json at all\n{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}";
        let graph = convert(input).unwrap();
        assert_eq!(graph.components.len(), 1);
    }
}
