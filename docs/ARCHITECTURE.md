# deskd — Architecture Diagrams

Living architecture documentation. Diagrams reflect the **actual code** as of the post-refactoring layout, not aspirational design. Mermaid renders natively in GitHub.

When the code changes in ways that affect these diagrams, update them in the same PR.

---

## 1. Layer Diagram

deskd follows a hexagonal (ports-and-adapters) architecture. Dependency direction is enforced by the module structure under `src/` and validated by archlint.

```mermaid
flowchart TD
    main["main.rs / bin/<br/>composition root"]
    app["app/<br/>use cases · worker · serve · MCP tools"]
    ports["ports/<br/>traits: MessageBus, Executor,<br/>TaskReader/Writer, ContextRepository,<br/>StateMachineReader/Writer<br/><i>+ bus_wire DTOs</i>"]
    infra["infra/<br/>UnixBus · InMemoryBus<br/>TaskStore · SmStore · ContextStore<br/>AgentProcess · AcpProcess<br/><i>+ dto/ adapters</i>"]
    domain["domain/<br/>pure types: Message · Task ·<br/>MainBranch/Node · ModelDef/Instance ·<br/>DomainEvent · Agent · WorkItem"]

    main --> app
    main --> infra
    app --> ports
    app --> domain
    infra --> ports
    infra --> domain
    ports --> domain

    classDef domainNode fill:#e8f5e9,stroke:#2e7d32,color:#000
    classDef portNode fill:#fff3e0,stroke:#e65100,color:#000
    classDef infraNode fill:#e3f2fd,stroke:#1565c0,color:#000
    classDef appNode fill:#f3e5f5,stroke:#6a1b9a,color:#000
    classDef mainNode fill:#fafafa,stroke:#424242,color:#000

    class domain domainNode
    class ports portNode
    class infra infraNode
    class app appNode
    class main mainNode
```

**Rules** (enforced by module boundaries):

- `domain/` depends only on `std` and `serde_json::Value`. Pure data types, no serde derives, no I/O.
- `ports/` depends only on `domain/`. Defines trait interfaces (object-safe via `Pin<Box<dyn Future>>`) plus shared wire DTOs in `ports::bus_wire`.
- `infra/` depends on `ports/` + `domain/`. Concrete implementations: Unix sockets, file stores, subprocess executors. Owns `infra::dto/` adapters that carry serde derives.
- `app/` orchestrates domain + ports for use cases (worker loop, serve command, MCP tools, graph engine). Does not depend on `infra/` directly — it receives trait objects from the composition root.
- `main.rs` and the binaries in `src/bin/` wire concrete `infra` types into `app` consumers.

---

## 2. Domain Model

Core types live in `src/domain/` and are referenced by traits in `src/ports/`. Domain types have **no serde derives** — wire/persistence formats are owned by adapter layers.

```mermaid
classDiagram
    class Message {
        +String id
        +String source
        +String target
        +Value payload
        +Option~String~ reply_to
        +Metadata metadata
    }
    class Metadata {
        +u8 priority
        +bool fresh
    }
    class Envelope {
        <<enum>>
        Register(Register)
        Message(Message)
        List
    }

    class Task {
        +String id
        +String description
        +TaskStatus status
        +TaskCriteria criteria
        +Option~String~ assignee
        +Option~String~ result
        +Option~String~ error
        +u32 attempt
        +u32 max_retries
        +Option~String~ retry_after
        +Option~String~ sm_instance_id
    }
    class TaskStatus {
        <<enum>>
        Pending
        Active
        Done
        Failed
        Cancelled
        DeadLetter
    }
    class TaskCriteria {
        +Option~String~ model
        +Vec~String~ labels
    }

    class MainBranch {
        +String agent
        +u32 budget_tokens
        +Vec~Node~ nodes
        +to_system_prompt() String
        +partition_by_tags(groups) Vec~MainBranch~
    }
    class Node {
        +String id
        +NodeKind kind
        +String label
        +u32 tokens_estimate
        +Vec~String~ tags
    }
    class NodeKind {
        <<enum>>
        Static{role, content}
        Live{command, args, max_age_secs, ...}
    }

    class ModelDef {
        +String name
        +Vec~String~ states
        +String initial
        +Vec~String~ terminal
        +Vec~TransitionDef~ transitions
    }
    class TransitionDef {
        +String from
        +String to
        +StepType step_type
        +Option~TaskCriteria~ criteria
        +u32 max_retries
    }
    class Instance {
        <<sm runtime>>
        +String id
        +String model
        +String current_state
    }

    class DomainEvent {
        <<enum>>
        TaskCreated · TaskClaimed · TaskCompleted ·
        TaskFailed · SmTransitioned · etc.
    }

    Message "1" *-- "1" Metadata
    Envelope --> Message
    Task --> TaskStatus
    Task "1" *-- "1" TaskCriteria
    MainBranch "1" *-- "*" Node
    Node --> NodeKind
    ModelDef "1" *-- "*" TransitionDef
    TransitionDef ..> TaskCriteria : may carry
    Instance ..> ModelDef : runs

    note for Message "domain/message.rs<br/>no serde — pure data"
    note for Task "domain/task.rs<br/>retry: exponential backoff"
    note for MainBranch "domain/context.rs<br/>materialized via to_system_prompt()"
    note for ModelDef "domain/statemachine.rs"
    note for DomainEvent "domain/events.rs<br/>JSON via infra::dto::bus"
```

### Port Traits

| Port trait | File | Implementations |
|---|---|---|
| `MessageBus` | `ports/bus.rs` | `infra::unix_bus::UnixBus` (prod), `infra::memory_bus::InMemoryBus` (tests) |
| `Executor` | `ports/executor.rs` | Claude/Memory `AgentProcess`, `AcpProcess` (constructed in `app/worker.rs::start_executor`) |
| `TaskReader` + `TaskWriter` | `ports/store.rs` | `infra::task_store::TaskStore` (file-backed), `infra::memory_store` (tests) |
| `StateMachineReader` + `StateMachineWriter` | `ports/store.rs` | `infra::sm_store`, `infra::memory_store` |
| `ContextRepository` | `ports/store.rs` | `infra::context_store` |

`TaskRepository` and `StateMachineRepository` are blanket-impl supertraits combining the ISP-split reader/writer pairs.

---

## 3. Sequence Diagrams

### 3.1 Task Lifecycle — create, claim, execute, complete

```mermaid
sequenceDiagram
    autonumber
    participant Client as Client<br/>(MCP / CLI)
    participant TW as TaskWriter
    participant Q as TaskStore<br/>(file queue)
    participant Worker as Worker loop<br/>(app/worker.rs)
    participant Exec as Executor<br/>(Claude/ACP/Memory)
    participant Bus as MessageBus

    Client->>TW: create(description, criteria, created_by)
    TW->>Q: persist Task{status=Pending, attempt=0}
    Q-->>TW: Task
    TW-->>Client: Task

    loop poll
        Worker->>TW: claim_next(agent, model, labels)
        TW->>Q: scan + filter by criteria + retry_after
        Q-->>TW: Option<Task>
    end
    TW-->>Worker: Task{status=Active, assignee=agent}

    Worker->>Exec: send_task(message, progress_sink, image, limits)
    Exec-->>Worker: stream chunks via ProgressSink
    Note over Worker,Bus: progress chunks routed to Bus<br/>(telegram.out / queue:replies)

    alt success
        Exec-->>Worker: TurnResult{response_text, cost_usd, turns, tokens}
        Worker->>TW: complete(id, result_text, cost, turns)
        TW->>Q: status=Done
    else failure
        Exec-->>Worker: Err
        Worker->>TW: fail(id, error_msg)
        Note right of TW: if attempt < max_retries:<br/>compute_retry_after<br/>(30s × 2^attempt, cap 5m)<br/>else status=DeadLetter
    end
```

### 3.2 Message Flow — bus routing

```mermaid
sequenceDiagram
    autonumber
    participant Sender as Sender client<br/>(CLI · adapter · sub-agent)
    participant Wire as ports::bus_wire<br/>(BusEnvelope JSON)
    participant Sock as Unix socket<br/>{work_dir}/.deskd/bus.sock
    participant Server as bus_server<br/>(infra::bus_server)
    participant Sub as Subscriber client<br/>(worker · adapter)

    Sender->>Sender: Message → BusMessage<br/>(impl From in bus_wire)
    Sender->>Wire: serialize BusEnvelope::Message
    Wire->>Sock: newline-delimited JSON
    Sock->>Server: parse BusEnvelope
    Server->>Server: route by target<br/>(agent:* · queue:* · telegram.out:* · broadcast)
    loop matched subscribers
        Server->>Sub: BusEnvelope::Message
        Sub->>Sub: BusMessage → Message<br/>(impl From back to domain)
    end

    Note over Sender,Server: register flow:<br/>BusEnvelope::Register{name, subscriptions}<br/>before recv loop starts
    Note over Server: list flow:<br/>BusEnvelope::List → list_response{clients}
```

The bus server retries the connection with exponential backoff (10 attempts, 100ms initial) — see `app/worker.rs::bus_connect`.

### 3.3 Context Materialization — graph → system prompt → executor

```mermaid
sequenceDiagram
    autonumber
    participant Spawner as Agent spawn<br/>(serve / add_persistent_agent)
    participant Repo as ContextRepository
    participant Branch as MainBranch<br/>(domain/context.rs)
    participant Live as Live node runner<br/>(shell exec, cached)
    participant Exec as Executor

    Spawner->>Repo: load(default_main_path(work_dir))
    Repo-->>Spawner: MainBranch{nodes, budget_tokens}

    loop materialize
        Spawner->>Branch: iterate nodes
        alt NodeKind::Static
            Branch-->>Spawner: role + content (as-is)
        else NodeKind::Live
            Branch->>Live: command + args
            Live-->>Branch: stdout (cached if max_age_secs)
            Branch-->>Spawner: inject_as + content
        end
    end

    Spawner->>Branch: to_system_prompt()
    Branch-->>Spawner: "## label\ncontent\n\n…" (static-only)

    Spawner->>Exec: spawn with system_prompt
    Note over Exec: subsequent send_task<br/>calls reuse the session
```

---

## 4. DTO Boundary

Domain types are pure. Serde lives at the edges. Conversions happen at the port boundary so that `infra` adapters can speak wire/file formats without leaking serde into `domain`.

```mermaid
flowchart LR
    subgraph DOM["domain/ (pure)"]
      DM["Message"]
      DT["Task"]
      DE["DomainEvent"]
      DC["MainBranch / Node"]
      DS["Instance / ModelDef"]
    end

    subgraph WIRE["ports/bus_wire (serde DTO at port)"]
      BM["BusMessage"]
      BR["BusRegister"]
      BE["BusEnvelope"]
    end

    subgraph DTO["infra/dto/ (serde adapters)"]
      DTOBUS["dto::bus<br/>DomainEvent → Value"]
      DTOTASK["dto::task<br/>Task ↔ TaskFile"]
      DTOCTX["dto::context<br/>MainBranch ↔ YAML"]
      DTOCFG["dto::config<br/>workspace · deskd.yaml"]
      DTOINST["dto::instance<br/>Instance ↔ JSON"]
    end

    subgraph ADAPT["infra/ adapters"]
      UB["unix_bus / bus_server"]
      TS["task_store"]
      SS["sm_store"]
      CS["context_store"]
    end

    DM <-->|impl From| BM
    BM --> BE
    BR --> BE
    BE -->|JSON over Unix socket| UB

    DE -->|impl From&lt;&amp;DomainEvent&gt; for Value| DTOBUS
    DTOBUS --> UB

    DT <-->|encode/decode| DTOTASK
    DTOTASK <-->|YAML on disk| TS

    DC <-->|encode/decode| DTOCTX
    DTOCTX <-->|YAML on disk| CS

    DS <-->|encode/decode| DTOINST
    DTOINST <-->|JSON on disk| SS

    classDef dom fill:#e8f5e9,stroke:#2e7d32,color:#000
    classDef wire fill:#fff3e0,stroke:#e65100,color:#000
    classDef dto fill:#e3f2fd,stroke:#1565c0,color:#000
    classDef adapt fill:#f3e5f5,stroke:#6a1b9a,color:#000
    class DM,DT,DE,DC,DS dom
    class BM,BR,BE wire
    class DTOBUS,DTOTASK,DTOCTX,DTOCFG,DTOINST dto
    class UB,TS,SS,CS adapt
```

### Where conversions live

| From → To | Module | Notes |
|---|---|---|
| `domain::Message` ↔ `BusMessage` | `ports/bus_wire.rs` | `impl From` in both directions; metadata flattened into wire fields |
| `DomainEvent` → `serde_json::Value` | `infra/dto/bus.rs` | One-way: events are emitted, never parsed back |
| `domain::Task` ↔ task DTO | `infra/dto/task.rs` | YAML on disk in the per-agent task directory |
| `domain::MainBranch` ↔ context YAML | `infra/dto/context.rs` | `default_main_path(work_dir) = {work_dir}/.deskd/context/main.yaml` |
| `domain::Instance` ↔ instance JSON | `infra/dto/instance.rs` | One file per state-machine instance |
| `WorkspaceConfig`, `UserConfig` | `infra/dto/config.rs` | Parsed from `workspace.yaml` and per-agent `deskd.yaml` |

`bus_wire` lives in `ports/` (not `infra/`) because the wire format is part of the contract that any `MessageBus` implementation must speak — multiple adapters share it.

---

## Keeping diagrams honest

These diagrams are checked into `docs/` so they live alongside the code. Updates land in the PR that changes the underlying structure. archlint validates the layer dependency arrows in the **Layer Diagram** against `archlint.yaml`.
