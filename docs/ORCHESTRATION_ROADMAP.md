# Agent Orchestration Roadmap

## Context

deskd's core value is multi-agent orchestration: message bus, task queue, state machine workflows, and adapters. The current SM/workflow implementation works for simple cases but lacks reliability (no timeouts enforced, no retries), verification (no way to validate agent output before progressing), and proper domain modeling (tasks are parallel entities instead of being owned by SM instances, no domain events).

This roadmap addresses these gaps across four phases, each building on the previous.

## Principles

- **Agent is just an Executor**: prompt in, result out. The agent knows nothing about tasks, state machines, or orchestration. It implements the `Executor` trait (`send_task` / `is_alive` / `stop` / `kill`).
- **Orchestration is outside the agent**: dispatch, verification, retry, routing, state transitions — all happen in deskd, not in the agent.
- **Three step types**: `agent` (stream, does real work), `check` (deterministic shell command, verifies artifacts), `validate` (`-p` mode with `--json-schema`, cheap LLM review).
- **Structured output via `--json-schema`** for validation steps — forces the model to return a typed verdict.
- **Domain events instead of manual notify calls** — transitions emit events, subscribers react.

## Phase 1: Workflow Reliability (foundation)

Foundation work to make SM workflows production-reliable. No new features, just making existing fields work and handling failure gracefully.

| # | Item | Effort | Issues |
|---|------|--------|--------|
| 1 | Task timeouts | S | #244 |
| 2 | Retry policy | S | #245 |
| 3 | Dead letter queue | S | #246 |
| 4 | Crash recovery hardening | M | #247 |

### 1. Task timeouts

**Problem**: `TransitionDef` has `timeout` and `timeout_goto` fields but they are never enforced. A stuck agent blocks the entire workflow forever.

**Approach**: Add a periodic sweep loop in the worker/serve path that checks active tasks against their timeout. When expired, force-transition the SM instance to the `timeout_goto` state and mark the task as failed.

**Files**: `src/app/worker.rs`, `src/app/statemachine.rs`, `src/domain/task.rs`

### 2. Retry policy

**Problem**: When a task fails, the SM instance is stuck. There is no automatic retry.

**Approach**: Add `attempt` counter and `max_retries` field to `Task`. On failure, if `attempt < max_retries`, re-queue the task as Pending with exponential backoff. The sweep loop handles re-dispatch.

**Files**: `src/domain/task.rs`, `src/app/worker.rs`

### 3. Dead letter queue

**Problem**: After max retries are exhausted, the task has nowhere to go.

**Approach**: Add `DeadLetter` variant to `TaskStatus`. After max retries, park the task in dead letter state for human review. Expose via `deskd task list --dead-letter` and MCP tool.

**Files**: `src/domain/task.rs`, `src/app/cli.rs`, `src/app/mcp.rs`

### 4. Crash recovery hardening

**Problem**: `dispatch_pending` has gaps — if deskd crashes after an agent returns a result but before the SM transition is applied, the task result is lost. No idempotency checks.

**Approach**: Make `dispatch_pending` idempotent: check if a result already exists before dispatching, handle the "result present but not transitioned" case. Add startup recovery scan.

**Files**: `src/app/worker.rs`, `src/app/statemachine.rs`

**Dependencies**: None — this is foundational work.

## Phase 2: Step Types & Verification

Add non-agent step types to validate work before progressing through the workflow.

| # | Item | Effort | Issues |
|---|------|--------|--------|
| 5 | Check steps | M | #248 |
| 6 | Validation steps | M | #249 |
| 7 | step_type enum on TransitionDef | S | #250 |
| 8 | Unified dispatch (WorkItem) | L | #251 |

### 5. Check steps

**Problem**: After an agent completes work, there is no way to verify artifacts before transitioning. Did the PR get created? Do tests pass? Does the file exist?

**Approach**: New step type `check` — runs a deterministic shell command. Exit code 0 = pass (transition to `to`), non-zero = fail (transition to error state or retry). No LLM involved.

**Files**: `src/app/worker.rs`, `src/domain/statemachine.rs`

### 6. Validation steps

**Problem**: Some verifications need judgment (code review, content quality) but don't need a full agent with tools. Currently the only option is spawning a full agent.

**Approach**: New step type `validate` — runs Claude in `-p` (print) mode with `--json-schema` to get a structured verdict (pass/fail + reason). Cheap, fast, no tools.

**Files**: `src/app/worker.rs`, `src/ports/executor.rs`

### 7. step_type enum on TransitionDef

**Problem**: `TransitionDef.step_type` is `Option<String>` — no type safety, no validation.

**Approach**: Replace with `enum StepType { Agent, Check, Validate, Human }`. Parse from YAML, validate at model load time.

**Files**: `src/domain/statemachine.rs`, `src/infra/dto.rs`

### 8. Unified dispatch (WorkItem abstraction)

**Problem**: Dual push/pull dispatch paths — direct bus messages for simple transitions vs. task queue for criteria-based ones. Logic is split and duplicated.

**Approach**: Single `WorkItem` abstraction that all transitions create. WorkItem can be dispatched via bus (direct) or queue (criteria-based). One code path for dispatch, progress, and completion.

**Files**: `src/app/worker.rs`, `src/domain/task.rs`, `src/app/statemachine.rs`

**Dependencies**: Phase 1 (reliability) should be done first — retry and timeout logic needs to be stable before refactoring dispatch.

## Phase 3: Domain Model Cleanup

Proper domain-driven design: events, entities, port traits used everywhere.

| # | Item | Effort | Issues |
|---|------|--------|--------|
| 9 | Domain events | M | #252 |
| 10 | Agent as domain entity | M | #253 |
| 11 | Wire port traits everywhere | M | #254, relates to #193 |
| 12 | Task owned by Instance | M | #255, relates to #210 |

### 9. Domain events

**Problem**: State changes are communicated via manual `notify` calls in transition definitions. No way for other parts of the system to react to events generically.

**Approach**: Emit domain events: `InstanceCreated`, `TransitionApplied`, `TaskDispatched`, `TaskCompleted`, `TaskTimedOut`, `AgentCrashed`. Notify becomes a subscriber, not a special case. Events flow through the bus.

**Files**: `src/domain/` (new events module), `src/app/statemachine.rs`, `src/app/worker.rs`

### 10. Agent as domain entity

**Problem**: Agent is just two enums (`SessionMode`, `AgentRuntime`) in `domain/agent.rs`. No name, no capabilities, no status tracking. Health checks exist on `Executor` but agent domain doesn't model health.

**Approach**: Promote Agent to a proper domain entity: name, capabilities (labels/model), status (`Ready`/`Busy`/`Unhealthy`), health check integration via `Executor::is_alive()`.

**Files**: `src/domain/agent.rs`, `src/app/worker.rs`

### 11. Wire port traits everywhere

**Problem**: `MessageBus`, `Executor`, and repository traits exist in `src/ports/` but application code often uses concrete types directly.

**Approach**: Wire port traits through all application code. StateMachine dispatch uses `MessageBus` trait, worker uses `Executor` trait, all stores go through repository traits. Enables testing with mocks.

**Files**: `src/app/statemachine.rs`, `src/app/worker.rs`, `src/app/mcp.rs`

**Existing work**: Relates to #193 (DIP — wire Executor trait).

### 12. Task owned by Instance

**Problem**: Task and Instance are parallel entities with a loose `sm_instance_id` link. This causes consistency issues — a task can exist without an instance, or an instance can lose track of its tasks.

**Approach**: Make Task part of the SM Instance aggregate. Instance owns its tasks. Task creation and completion go through the Instance, ensuring consistency.

**Files**: `src/domain/task.rs`, `src/domain/statemachine.rs`, `src/app/statemachine.rs`

**Existing work**: Relates to #210 (SM transitions always create tasks).

**Dependencies**: Phase 2 (step types) should land first — the WorkItem abstraction informs how tasks are owned.

## Phase 4: Agent Cooperation

Multi-agent patterns: sync queries, context agents, capability-based routing.

| # | Item | Effort | Issues |
|---|------|--------|--------|
| 13 | query_agent MCP tool | M | #257, #221 |
| 14 | Context agents | L | #258, #220, #222 |
| 15 | Agent capability discovery | M | #256 |

### 13. query_agent MCP tool

**Problem**: Bus is fire-and-forget. Agent A cannot ask agent B a question and wait for the answer. This blocks the "memory = live agents" model.

**Approach**: New MCP tool `query_agent(target, question, timeout)` — sends a message and blocks until the target responds. Implemented as a correlation-ID pattern on the bus.

**Existing issue**: #221

### 14. Context agents

**Problem**: Context agents need a lightweight worker path (no tools, no task queue) and context compaction (auto-summarize when context fills up). Both are partially implemented but not wired.

**Approach**: Lightweight worker path for context agents (#220). Wire context materialization into startup (#218, closed). Context compaction via auto-summarize (#222).

**Existing issues**: #220, #222 (and #218 which is closed/done)

### 15. Agent capability discovery

**Problem**: Agent routing is by hardcoded name. If you want "an agent that can write Go code," you have to know its name.

**Approach**: Agents declare capabilities (languages, tools, domains). Routing can match by capability instead of name. Discovery via MCP tool or bus query.

**Files**: `src/domain/agent.rs`, `src/app/worker.rs`, `src/app/mcp.rs`

**Dependencies**: Requires #253 (Agent as domain entity) for the capability model.

## Summary

| Phase | Items | Focus | Dependencies |
|-------|-------|-------|--------------|
| 1 | 1-4 | Reliability | None (start here) |
| 2 | 5-8 | Verification | Phase 1 |
| 3 | 9-12 | Domain model | Phase 2 |
| 4 | 13-15 | Cooperation | Phase 3 (#10 for #15) |

Total: 15 items. Phases are sequential but items within a phase can be parallelized.
