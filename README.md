# deskd

[![CI](https://github.com/kgatilin/deskd/actions/workflows/ci.yml/badge.svg)](https://github.com/kgatilin/deskd/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/kgatilin/deskd/graph/badge.svg)](https://codecov.io/gh/kgatilin/deskd)

**Agent orchestration runtime — fractal message bus for AI agents**

Spawn, route, and manage AI agents. Each agent gets its own isolated message bus (Unix socket), persistent session, and worker loop. Agents communicate via pub/sub routing; adapters bridge external platforms (Telegram, Discord). Skill graphs encode multi-step workflows as executable DAGs.

---

## Quick Start

**Install** (prebuilt binary):
```bash
curl -fsSL https://raw.githubusercontent.com/kgatilin/deskd/main/install.sh | bash
```

**Configure** a workspace:
```yaml
# workspace.yaml
agents:
  - name: dev
    work_dir: /home/dev
    command: [claude, --output-format, stream-json, ...]
    telegram:
      token: ${BOT_TOKEN}   # optional
    budget_usd: 50.0         # optional, default 50
```

**Run**:
```bash
deskd serve --config workspace.yaml
```

---

## Architecture

```
workspace.yaml
    ↓
deskd serve
    ↓
┌──────────────────────┐     ┌──────────────────────┐
│ agent: kira           │     │ agent: dev            │
│ bus.sock              │     │ bus.sock              │
│ worker (Claude)       │     │ worker (Claude)       │
│ [telegram adapter]    │     │                       │
└──────────────────────┘     └──────────────────────┘
```

Each agent is fully isolated — its own socket, inbox, logs, and session state. Agents discover each other via the bus routing protocol.

**Source modules:**

| Module | Role |
|--------|------|
| `bus.rs` | Per-agent Unix socket pub/sub with glob routing |
| `worker.rs` | Worker loop: read bus → execute via Claude → post result |
| `agent.rs` | Agent state, create/recover, streaming Claude calls |
| `inbox.rs` | File-based inbox for async task results |
| `mcp.rs` | MCP server (stdio JSON-RPC) for Claude tool integration |
| `graph.rs` | Skill graph engine: DAG of tool groups + LLM decision nodes |
| `schedule.rs` | Cron-based scheduled actions on the bus |
| `adapters/telegram.rs` | Telegram bot adapter (teloxide) |
| `unified_inbox` | Unified view across all agent inboxes |

---

## Key Features

- **Persistent sessions** — agents survive restarts; session ID and cost are tracked in `~/.deskd/agents/<name>.yaml`
- **Sub-agents** — spawn child agents dynamically via MCP `add_persistent_agent`
- **Skill graphs** — YAML-defined DAGs mixing tool execution and LLM decision nodes; run with `deskd graph run`
- **MCP tools** — `deskd mcp` exposes `send_message`, `add_persistent_agent`, `run_graph` as Claude tools
- **Telegram & Discord adapters** — route chat messages to/from the agent bus
- **Schedule system** — cron-based triggers defined in `deskd.yaml`
- **Unified inbox** — async task results readable by sender name

---

## CLI

```bash
# Start all agents
deskd serve --config workspace.yaml

# Send a task to an agent
deskd agent send <name> "task" --socket <bus.sock>

# Read inbox (async replies)
deskd agent read <sender>
deskd agent read <sender> --clear   # read and delete

# Agent management
deskd agent list --socket <bus.sock>
deskd agent stats <name>
deskd agent create <name> [--prompt ...] [--model ...]
deskd agent rm <name>

# Skill graphs
deskd graph run <file.yaml>
deskd graph run <file.yaml> --work-dir .
deskd graph validate <file.yaml>

# MCP server (invoked by Claude via --mcp-config)
deskd mcp --agent <name>
```

---

## Configuration

### `workspace.yaml` — defines agents for `deskd serve`

```yaml
agents:
  - name: dev
    work_dir: /home/dev
    command: [claude, --output-format, stream-json, ...]
    telegram:
      token: ${BOT_TOKEN}
    budget_usd: 50.0
```

### `deskd.yaml` — per-agent config (at `{work_dir}/deskd.yaml`)

```yaml
model: claude-sonnet-4-6
system_prompt: "You are..."
max_turns: 100
mcp_config: '{"mcpServers":{...}}'
telegram:
  routes:
    - chat_id: -1234567890
agents: []      # sub-agents
schedules: []   # cron jobs
channels: []    # named bus targets
```

### `web:` — optional web control panel (#443)

When the workspace defines a `web:` block with `enabled: true`, `deskd serve`
also starts a small axum HTTP server with Telegram magic-link login.

```yaml
web:
  enabled: true
  bind: 127.0.0.1:8127             # local-only; reverse proxy handles TLS
  external_url: https://deskd.example.com
  session_ttl_days: 30
  magic_link_ttl_seconds: 300
  allowed_telegram_ids: [123456]
  audit_log: ~/.deskd/logs/web-audit.jsonl
  rate_limit:
    auth_requests_per_hour: 20
```

Bind is intentionally local. Front it with a reverse proxy (caddy, nginx) that
terminates TLS, forwards `X-Forwarded-For`, and proxies to `127.0.0.1:8127`.

### Key paths

| Path | Purpose |
|------|---------|
| `{work_dir}/.deskd/bus.sock` | Agent's message bus socket |
| `~/.deskd/agents/{name}.yaml` | Agent state (cost, turns, session_id) |
| `~/.deskd/inbox/{sender}/` | Task results for async reading |
| `~/.deskd/logs/{name}.log` | Agent logs |

---

## Bus Protocol

Unix socket, newline-delimited JSON.

```json
// Register
{"type":"register","name":"cli","subscriptions":["agent:cli"]}

// Send message
{"type":"message","id":"uuid","source":"cli","target":"agent:dev","payload":{"task":"..."}}

// List clients
{"type":"list"}
```

Routing targets: `agent:<name>`, `queue:<name>`, `telegram.out:<chat_id>`, `broadcast`, glob patterns (`agent:*`).

---

## Build

```bash
cargo build --release

# Linux static (containers/VPS)
cargo build --release --target x86_64-unknown-linux-musl

# macOS — re-sign after build
codesign --force --sign - target/release/deskd
```

Quality gate (CI requires all three):
```bash
cargo fmt && cargo clippy -- -D warnings && cargo test
```

---

## License

MIT
