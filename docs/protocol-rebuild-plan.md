# CodeSeeX Protocol Rebuild Plan

This plan defines the next rewrite pass for the Rust/Tauri implementation. The goal is not to keep patching the current `server.rs` tool loop. The goal is to rebuild the conversation pipeline around the verified Electron protocol and then move code behind clear boundaries.

## Non-Negotiable Baseline

The Electron implementation is the protocol source of truth until a replacement behavior is deliberately designed, documented, and tested.

The following behavior must not change:

- Codex talks to CodeSeeX through Responses-compatible local endpoints.
- CodeSeeX talks to DeepSeek/custom upstream through Chat Completions-compatible requests.
- CodeSeeX-owned tools must not be exposed as client-executable Responses `function_call` items.
- MCP and other Codex-provided external tools must remain Codex-native `function_call` passthrough.
- `apply_patch` must remain Codex-native `custom_tool_call`; CodeSeeX must not apply the patch internally.
- DeepSeek assistant messages with `tool_calls` must preserve `reasoning_content` when replayed upstream.
- Streaming must emit one coherent Codex turn: thinking/commentary/tool calls first, final answer once, then `response.completed`.
- Failed/interrupted turns must not persist partial assistant final text as historical truth.

## Rust Architecture Principles

The rewrite must not recreate the Electron implementation as a Rust god object. The Electron code is the protocol baseline, not the architectural template.

The Rust implementation should use the language strengths:

- Closed protocol decisions should be enums, not stringly typed branches.
- Each module should own one protocol boundary and expose a narrow API.
- The request pipeline should be a small coordinator, not a place where all logic accumulates.
- Invalid protocol states should be unrepresentable where practical.
- Fallible boundaries should return typed errors with stable diagnostic categories.
- Tests should target pure modules first, then end-to-end Codex-compatible streams.
- Dynamic dispatch should be reserved for plugin/community tools; built-in protocol behavior should stay strongly typed.

## Target Pipeline

The new proxy core should be organized as a deterministic pipeline:

```text
Responses request
  -> request normalization
  -> previous response chain reconstruction
  -> context compiler
  -> tool registry and ownership resolution
  -> DeepSeek Chat payload
  -> upstream call
  -> tool turn coordinator
  -> Responses item/event emitter
  -> durable request checkpoint
```

Each stage should have one owner module and one responsibility. No stage should silently repair another stage's mistakes.

The orchestrator should look like a pipeline of decisions:

```text
RequestContext
  -> CompiledContext
  -> UpstreamRequest
  -> UpstreamTurn
  -> ToolDecision
  -> ResponsePlan
  -> PersistedCheckpoint
```

It should not directly know how to parse every tool, emit every SSE event, execute every hosted tool, and update every state table.

## Proposed Rust Module Shape

```text
crates/codeseex-core/src/
  protocol/
    responses.rs       # Codex-facing request/response item types
    chat.rs            # DeepSeek/OpenAI-compatible Chat message/tool types
    sse.rs             # Typed Responses stream events
  tools/
    ownership.rs       # ToolOwner, ToolDecision, collision rules
    schema.rs          # Model-facing function declarations
    items.rs           # Response item conversion helpers
  context/
    compiler.rs        # Context compilation and budget decisions
    facts.rs           # Tool fact ledger
    compaction.rs      # Deterministic compact payloads
  models.rs
  catalog.rs
  urls.rs

crates/codeseex-proxy/src/
  http/
    router.rs          # Axum routes only
    errors.rs          # HTTP error mapping
  responses/
    handler.rs         # Thin request handler
    emitter.rs         # Responses record/SSE emitter
  upstream/
    client.rs          # reqwest client and URL/header normalization
    stream.rs          # Chat streaming parser
  tools/
    registry.rs        # Built-in, community, external registration
    coordinator.rs     # Tool loop state machine
    hosted.rs          # CodeSeeX-hosted built-ins
    external.rs        # MCP/Codex passthrough adapters
  app_state.rs         # Shared app dependencies

crates/codeseex-store/src/
  request_repo.rs      # Request lifecycle and chain reconstruction
  event_repo.rs
  usage_repo.rs
```

This shape keeps `server.rs` from becoming the new center of gravity. The HTTP handler should assemble dependencies and call the pipeline; it should not contain the protocol.

## Core Types

The tool protocol should be driven by typed ownership:

```rust
enum ToolOwner {
    CodexNative(CodexNativeTool),
    CodeseexHosted(HostedTool),
    Community(CommunityToolId),
    External(ExternalToolRef),
}

enum CodexNativeTool {
    ApplyPatch,
}

enum HostedTool {
    WebSearch,
    ListDirectory,
    ReadFileRange,
    WorkspaceSearch,
}

enum ToolDecision {
    ExecuteInProxy(Vec<HostedToolCall>),
    ReturnToCodex(Vec<ClientToolCall>),
    ExecuteThenReturn {
        proxy_calls: Vec<HostedToolCall>,
        client_calls: Vec<ClientToolCall>,
    },
    UnknownTool { name: String },
}
```

This prevents the previous regression class where CodeSeeX-owned tools accidentally became ordinary client `function_call` items.

Responses output should also be typed before serialization:

```rust
enum ResponseOutputItem {
    Message(ResponseMessage),
    Reasoning(ReasoningItem),
    FunctionCall(ClientFunctionCall),
    CustomToolCall(NativeCustomToolCall),
    WebSearchCall(WebSearchCallItem),
    ProxyToolCall(ProxyToolCallItem),
    Compaction(CompactionItem),
}
```

Only `FunctionCall` should emit client-executable function-call argument events. `ProxyToolCall` must never share that code path.

## Module Boundaries

### `responses_adapter`

Owns the Codex-facing protocol.

- Normalize `/v1/responses` input items.
- Convert response items back into Chat messages only when the tool pair is valid.
- Emit Responses records and streaming SSE events.
- Preserve `phase`, item ids, output order, `parallel_tool_calls`, `error`, and `incomplete_details`.
- Never execute tools.

### `conversation_state`

Owns durable request lifecycle and parent chain reconstruction.

- Persist `in_progress`, `completed`, `failed`, and `interrupted`.
- Reconstruct long `previous_response_id` chains.
- Replay completed turns fully.
- Replay failed/interrupted turns using only user/system input and verified facts.
- Reject corrupt state rather than overwriting it with empty state.

### `context_compiler`

Owns model-visible historical context.

- Merge current input, previous messages, compact summaries, and verified tool facts.
- Protect tool facts over assistant self-description.
- Preserve legal assistant/tool protocol pairs.
- Preserve `reasoning_content` only where DeepSeek tool-call replay requires it.
- Redact inline `data:` URLs and large binary payloads.
- Apply deterministic budget compaction only near real context or storage limits.

### `tool_registry`

Owns tool declarations and ownership.

- Register CodeSeeX system tools first.
- Deduplicate model-facing function names.
- Resolve every selected tool call to exactly one owner.
- Treat same-name collisions as CodeSeeX-owned when the name is a known CodeSeeX tool.
- Keep MCP/external request tools as passthrough tools.

### `tool_coordinator`

Owns the DeepSeek tool loop.

- Partition upstream `tool_calls` by ownership.
- Execute only CodeSeeX-hosted tools.
- Return native/client tools to Codex without executing them.
- Append CodeSeeX-hosted tool results as legal Chat `tool` messages.
- Stop on native/external tool calls so Codex can execute them.
- Detect tool-loop non-convergence with diagnostics, not silent recursion.

### `upstream_client`

Owns DeepSeek/custom upstream requests.

- Normalize model aliases.
- Normalize base URLs, official `/v1` compatibility, headers, timeout, and proxy settings.
- Send Chat Completions payloads.
- Parse streaming deltas into reasoning, content, tool-call deltas, and usage.
- Preserve upstream error body enough for user-visible diagnostics.

## Tool Ownership Matrix

| Tool class | Upstream declaration | Response item | Executor | Replay into DeepSeek |
| --- | --- | --- | --- | --- |
| `apply_patch` | Chat function named `apply_patch` | `custom_tool_call` | Codex native | Later `custom_tool_call_output` becomes Chat `tool` result |
| `web_search` | Chat function named `web_search` | `web_search_call` | CodeSeeX | Immediate Chat `tool` result, then continue upstream |
| Built-in CodeSeeX tools | Chat function by tool id | `proxy_tool_call` plus display-only usage message | CodeSeeX | Immediate Chat `tool` result, then continue upstream |
| Community tools | Chat function by tool id | `proxy_tool_call` plus display-only usage message | CodeSeeX, only explicit manifest execution | Immediate Chat `tool` result, then continue upstream |
| MCP/external tools | Normalized Chat function | `function_call` with original name/namespace | Codex native | Later `function_call_output` becomes Chat `tool` result |

No tool may appear in more than one executor path.

## Streaming Contract

The streaming adapter must match the verified Electron behavior:

- Start with `response.created` and `response.in_progress`.
- Emit reasoning output and optional display-only thinking before visible text/tool output.
- Close reasoning before emitting content or tool calls.
- Mark pre-tool assistant text as `phase: "commentary"`.
- Mark final answer text as `phase: "final_answer"`.
- Emit native/external function-call argument deltas only for tools Codex should execute.
- Emit CodeSeeX-owned tools as display/proxy items, never as client-executable `function_call`.
- Reuse the same output items in `response.completed.response.output`.
- End with `response.completed` and `data: [DONE]`.

## Rewrite Order

1. Freeze parity tests around the Electron baseline.
2. Extract pure protocol types and ownership enums.
3. Replace ad-hoc tool partitioning with a `ToolOwnership` resolver.
4. Replace direct Responses item construction with a `ResponsesEmitter`.
5. Replace the current tool loop with a `ToolCoordinator`.
6. Move upstream request construction and streaming parsing into `upstream_client`.
7. Move request state lifecycle into `conversation_state`.
8. Keep UI and desktop code stable until the proxy protocol is passing parity.

## Current Implementation Progress

Completed in the first rebuild pass:

- `tools::ownership` owns `ChatToolCall`, typed ownership resolution, and partitioning between CodeSeeX-hosted, Codex-native, and external/MCP calls.
- `tools::chat_protocol` owns DeepSeek Chat tool-call parsing and legal assistant tool-call replay messages, including `reasoning_content` preservation.
- `tools::registry` owns tool registry data, enabled tool ids, settings, tool definition deduplication, and `tool_choice` normalization.
- `tools::response_items` owns conversion from Chat tool calls to Codex-visible response output items, including native `apply_patch`, native `web_search_call`, and CodeSeeX `proxy_tool_call` display items.
- `tools::hosted` owns CodeSeeX-hosted tool execution checks, execution dispatch, and verified tool fact summarization.
- `tools::coordinator` owns the non-streaming DeepSeek tool loop and stops before Codex-native/MCP execution.
- `upstream::payload` owns Chat payload normalization for model aliases, sampling parameters, response format, thinking mode, and streaming usage.
- `response_sse` owns Responses SSE event serialization, reasoning item encoding/decoding, display-only thinking markdown, native function/custom tool events, and web search call events.
- `app_state`, `manager_api`, `config_payload`, `http_utils`, and `http_response` split shared app dependencies, manager/config APIs, TOML payload parsing, version/time helpers, and generic HTTP response wrapping out of `server.rs`.
- `responses::context`, `responses::conversion`, `responses::usage`, and `responses::stream_tool_calls` own history/context compilation, non-streaming Chat-to-Responses conversion, usage mapping, and streaming tool-call delta assembly.

Still intentionally left for later passes:

- The streaming tool loop still lives in `server.rs`, but now depends on extracted ownership, Chat protocol, hosted execution, response item, SSE, and streaming tool-call modules.
- Request lifecycle orchestration still needs a narrower module once the streaming path has a typed emitter/coordinator boundary.
- Mixed CodeSeeX-hosted plus Codex-native/external tool turns still intentionally execute hosted tools first. Before release parity, add an order-preserving response projection test and decide whether client-visible output should follow model call order or actual execution order.
- `server.rs` should keep shrinking until it is mostly Axum routing plus request orchestration.

## Required Tests Before Continuing Feature Work

- Plain streaming answer produces exactly one final assistant message.
- DeepSeek reasoning appears before final content and is not replayed as normal assistant text.
- Hosted built-in tool call executes inside CodeSeeX and never emits `function_call`.
- `web_search` emits `web_search_call` events and never emits `proxy_tool_call`.
- `apply_patch` emits `custom_tool_call` and does not modify files inside CodeSeeX.
- MCP/external tool emits native `function_call` and is not swallowed by CodeSeeX.
- Mixed hosted plus external tools execute hosted first, then stop for Codex-native execution.
- Parent chain replay preserves completed tool pairs and drops unsafe failed partial assistant text.
- Upstream 400 errors expose the useful error body in logs and state.
- Tool loop limit produces a protocol diagnostic that identifies the repeated tool owner and call id.

## What Can Be Changed

- File/module organization.
- Internal Rust types and trait names.
- Storage implementation details, if observable behavior stays equivalent.
- Tool-loop diagnostics and safety caps.
- Removal of dead paths such as proxy-side `apply_patch` execution, after tests prove the native path.

## What Must Not Be Changed Without A New Decision

- Tool ownership.
- Responses item types at the Codex boundary.
- DeepSeek Chat tool replay rules.
- MCP passthrough semantics.
- `apply_patch` native execution semantics.
- Thinking/reasoning ordering and replay requirements.
- Durable request lifecycle rules.
