# Telegram MTProto history retrieval

Tracking issue: [kgatilin/deskd#376](https://github.com/kgatilin/deskd/issues/376).
This document describes the design only — phase 1 ships the skeleton,
phase 2 wires the real `grammers-client` integration.

## Why MTProto

deskd's default Telegram path is the Bot API (teloxide). In groups with
privacy mode enabled — the default for Bot API users — a bot only sees
messages that mention or reply to it. It cannot read history, scan other
participants' messages, or recover context it missed while offline.

That blocks agents like Kira from answering questions such as
"summarize today's thread" or "what did Konstantin say yesterday?". The
user-account MTProto API has no such restriction, so we add it as an
optional, read-only parallel path.

## Feature flag

The grammers dependency is large. It is gated behind an opt-in cargo
feature:

```
cargo build                       # default — no mtproto, no grammers
cargo build --features mtproto    # pulls in grammers-client
```

Default builds are unaffected. The `telegram_history` MCP tool is still
advertised in `tools/list` even without the feature so agents get a
consistent contract; the handler simply errors with a rebuild hint.

## Config example

```yaml
# deskd.yaml (per-agent user config)
telegram:
  routes:
    - chat_id: -1001234567890
  mtproto:
    api_id: 12345
    api_hash: "deadbeef"
    session_path: "/var/lib/deskd/tg-session.bin"
    phone: "+1234567890"
    allowed_chats:
      kira: [-1001234567890, -1003733725513]
      dev:  [-1001234567890]
```

The same `mtproto` block also exists on workspace-level
`TelegramConfig` for ops-managed deployments; the running agent reads
the per-user entry.

## Login flow (intended — phase 2)

MTProto sessions are interactive to create: Telegram SMS-es a code.
We keep that out of the daemon hot path with a dedicated subcommand:

```
deskd telegram-login \
    --api-id 12345 \
    --api-hash deadbeef \
    --phone +1234567890 \
    --session-path /var/lib/deskd/tg-session.bin
```

It prompts for the code (and the 2FA password if set) on stdin and
writes the grammers session file. `deskd serve` then loads the session
non-interactively.

## ACL model

`telegram_history` enforces a per-agent allow-list. The handler checks
`allowed_chats[agent_name]` for the requested `chat_id`; unknown agents
and unlisted chats are denied with a clear error. Default is empty —
deny all. This prevents an agent from snooping on chats it was never
intended to see, even if it gets the session file path right.

## Security

The session file is equivalent to full account access — anyone with the
file can log in as you. Treat it like an SSH private key:

  - Mode 0600, owned by the agent's unix user.
  - Never commit it to git; never ship it in container images.
  - Prefer a dedicated Telegram account for agent use, not a personal
    one. Automation on a personal number carries ban risk and exposes
    private DMs to the agent.
  - Filesystem-level encryption (LUKS, encrypted home) is recommended
    for the host storing the session.

## Scope — phase 1 (this PR)

In scope:
  - `grammers-client` added as an optional dep behind `mtproto` feature.
  - `MtProtoConfig` on both workspace `TelegramConfig` and per-user
    `TelegramRoutesConfig`, with an `agent_can_query` ACL helper.
  - `infra::telegram_mtproto` skeleton with `ChatMessage` serde type
    and a `MtProtoClient` whose methods are `todo!()` stubs.
  - `telegram_history` MCP tool surfaced with feature + config + ACL
    guards.
  - `deskd telegram-login` subcommand prints a phase-2 notice.

Not in scope (phase 2):
  - Real grammers `Client::connect` / `messages.getHistory` calls.
  - Interactive login implementation.
  - Session-file encryption at rest beyond filesystem ACLs.
  - Posting via MTProto (Bot API stays the canonical send channel).
  - Media downloads or real-time message streaming.
