# Changelog

## 0.5.0 - 2026-06-09

CodeSeeX 0.5.0 is the Rust/Tauri architecture release. It focuses on faster startup, stronger long-running desktop behavior, Codex tool compatibility, cleaner runtime data, improved Web Search, the new Vision module, and a smoother desktop installer experience.

### Highlights

- Rebuilt the desktop runtime around Rust and Tauri 2 for better performance and more reliable always-on behavior.
- Improved Codex tool compatibility, including native Apply Patch handling, native MCP passthrough, and safer hosted-tool ownership.
- Added the Vision module for image understanding and image generation through user-configured OpenAI-compatible endpoints.
- Improved Web Search with cleaner extraction, local/private target protection, and system proxy configuration.
- Updated the Codex model catalog for `deepseek-v4-pro` and `deepseek-v4-flash` with 1M context metadata and a 95% automatic compaction threshold.
- Refined the desktop UI, tool cards, icons, logs, usage display, settings layout, and installer flow.

### Added

- Added the Rust/Tauri desktop manager with tray controls, autostart, update checks, logs, usage, balance, and settings.
- Added Vision configuration for analyze endpoint, generation endpoint, model names, and API key.
- Added `vision_analyze` with support for HTTP(S) images, `data:image` URLs, `file://` URLs, workspace paths, and permitted local absolute paths.
- Added direct `input_image` attachment handling for `vision_analyze`, so Codex message images can be analyzed without shell base64 conversion or workspace copying.
- Added native-compatible `image_gen` exposure for Vision image generation, with legacy `vision_generate` kept for compatibility.
- Added dedicated tool icons for directory listing, file range reading, workspace search, Web Search, Apply Patch, and Vision.
- Added NSIS EXE installer support for installer language selection, current-user/all-users install mode, and smooth legacy desktop upgrades.

### Changed

- Improved Apply Patch schema guidance and native patch passthrough so Codex remains responsible for actual file mutation.
- Improved MCP handling so Codex-native MCP/external tools remain visible to the model and return to Codex for execution.
- Improved context replay for long Codex sessions, compacted history, and tool-result continuity.
- Improved same-turn hosted tool execution so multiple CodeSeeX tools can run concurrently while preserving stable Codex replay order.
- Reduced runtime/log file noise by keeping user logs compact, skipping diagnostic log persistence by default, and trimming oversized tool summaries before writing.
- Reduced stored request payload size by keeping full Codex request bodies out of runtime state and redacting inline `data:` / `input_image` payloads before logs or replay state.
- Updated tool descriptions to focus on user-facing capability instead of implementation details.
- Improved generated Codex TOML and catalog output for current Codex model-catalog requirements, including automatic refresh of stale generated catalogs.
- Improved Web Search and read-only workspace tools with bounded execution, cleaner summaries, and safer file/network boundaries.
- Improved generated-image handling so base64 image results are saved as local files, exposed as display-ready Markdown, and no longer returned as large inline tool payloads.
- Routed Codex auxiliary title and suggestion requests to Flash while keeping full conversation GPT-5 aliases on the configured default fallback.
