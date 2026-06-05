# State Contract

CodeSeeX state is an adapter ledger, not a second Codex transcript.

Codex owns the raw conversation/session files. Those files are useful for user-facing history, audit, and debugging, but CodeSeeX must not depend on parsing them to emulate the Responses API. The proxy needs its own durable state because it is the server that receives `previous_response_id`, compacts history, maps DeepSeek Chat messages back to Responses items, and recovers from interrupted tool loops.

## Responsibilities

The SQLite store may persist only data needed by the adapter boundary:

- Request lifecycle: `in_progress`, `completed`, `failed`, and `interrupted`.
- The `response_id` / `previous_response_id` chain needed to rebuild upstream context.
- Normalized request input that is needed for deterministic replay.
- Replayable `turn_messages` that are already legal for the upstream Chat protocol.
- Verified tool facts and compact records that protect against assistant self-description replacing tool reality.
- Usage, visible logs, and diagnostics needed by the local manager UI.

The store must stay useful after a crash or stream disconnect. A request is checkpointed at start, upgraded as tools complete, and finalized only when the turn really completes.

## Non-Responsibilities

The SQLite store must not become:

- A full copy of Codex jsonl sessions.
- A raw browser cache or webpage archive.
- A long-term dump of screenshots, `data:` URLs, binary payloads, or complete tool stdout.
- A secret store for API keys, Authorization headers, proxy credentials, or tokens.
- A source of model behavior that overrides Codex-native tool execution or MCP ownership.

If a fact is too large for durable adapter replay, persist a deterministic marker with size and hash instead of the full payload.

## Maintenance Rules

State maintenance must preserve schema and conversation identity while bounding risk:

- SQLite is opened with WAL journaling, normal synchronous mode, foreign keys enabled, and a busy timeout for local concurrent reads/writes.
- New writes are sanitized before they reach SQLite.
- Existing oversized request payloads are sanitized in place on maintenance; request rows are not deleted blindly.
- Maintenance may process multiple bounded batches per run, and reports when the batch limit is reached.
- Visible/debug event logs are retention-bound by `log_retention_days`.
- Large inline `data:` URLs are replaced by size/hash markers.
- Sensitive keys are redacted recursively.
- Long strings are truncated with size/hash markers.

This prevents the old single-file JSON failure mode where state grew without bounds and one parse failure could make the app unable to start.

## History Reconstruction

When rebuilding context:

- Completed parents may contribute final response data and replayable turn messages.
- Failed or interrupted parents may contribute user input and verified tool facts, but not partial assistant final text.
- Tool facts have higher evidence priority than assistant self-descriptions.
- Compact records are client summaries and must not override verified tool/request facts.

The result should be deterministic, bounded, and protocol-valid without reading Codex's private session files.
