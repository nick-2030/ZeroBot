# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ZeroBot is a Rust-based AI Agent system providing CLI/TUI and SDK interfaces. It supports multi-agent orchestration, tool calling, MCP (Model Context Protocol) integration, persistent sessions, gateway mode, and plugin/skill systems.

## Build & Test Commands

```bash
cargo build                          # Build workspace
cargo test                           # Run all tests
cargo test -p zerobot-core           # Core crate tests only
cargo test -p zerobot-cli            # CLI crate tests only
cargo clippy                         # Lint
cargo run -p zerobot-cli             # Interactive TUI
cargo run -p zerobot-cli -- exec "prompt"   # One-shot execution
cargo run -p zerobot-cli -- gateway         # Gateway daemon
cargo run -p zerobot-cli -- acp             # Agent Client Protocol server
cargo run -p zerobot-cli -- session list    # List sessions
cargo run -p zerobot-cli -- config show     # Show config
```

Tests use inline `#[cfg(test)] mod tests` modules (no `tests/` directory). Dev dependencies: `httpmock` for HTTP mocking, `tempfile` for temp dirs, `pretty_assertions` for diffs.

## Workspace Architecture

Three crates with unified dependency management in root `Cargo.toml` (`[workspace.dependencies]`):

**zerobot-core** — Core library with all orchestration logic:
- `agent.rs` — `Agent::run_turn()` drives the provider↔tool execution loop
- `tool.rs` — `ToolRegistry` for built-in tools (read, write, edit, bash, glob, grep) + Subagent/Skill/MCP adapters
- `session.rs` — `SqliteSessionStore` for persistent sessions, messages, tool calls, approvals, todos
- `config.rs` — 6-layer config: CLI > Managed > Local > Project > User > Defaults
- `provider.rs` — `Provider` trait with `OpenAIProvider` and `AnthropicProvider`
- `mcp.rs` — MCP clients (stdio local / HTTP remote JSON-RPC)
- `hooks.rs` — `HookManager` with 20+ hook events (Allow/Deny/Modify decisions)
- `context.rs` — `ContextManager` for system prompt assembly, history trimming, context compaction
- `gateway.rs` — `GatewayRuntime` long-running daemon event loop
- `swarm/` — Multi-agent swarm coordination with `TeammateBackend` trait
- `skills.rs` — Skill discovery from `.claude/skills`, `.agents/skills`, `.zerobot/skills/`, remote URLs
- `memory.rs` — Memory store with prompt injection detection
- `plugin.rs` — JSON-RPC plugin system
- `kanban.rs` — Task board management
- `agent_dispatch.rs` — Multi-agent dispatch with isolation modes

**zerobot-cli** — CLI binary and TUI:
- `main.rs` — clap-based CLI (exec, session, config, gateway, cron, acp subcommands)
- `tui/` — ratatui-based terminal UI with 14 components, keybinding system, markdown rendering

**zerobot-sdk** — Embeddable SDK:
- `lib.rs` — `ZeroBot` client with `query()`/`query_stream()`, `SessionHandle` for session lifecycle

## Key Architectural Patterns

- The agent loop in `Agent::run_turn()` alternates between LLM provider calls and tool execution until completion or max steps (default 100).
- Tool calls can execute in parallel or serial based on provider response.
- Context compaction triggers automatically when context window approaches limits — summarizes history into compaction anchors.
- System prompts are assembled from `prompts/system/*.md` (identity, tools, style, etc.) and `prompts/modes/*.md` (execute, plan, review, coordinator).
- Hooks fire at 20+ lifecycle points and can allow, deny, or modify payloads.
- Multi-agent dispatch spawns subagents with configurable isolation (in-process or separate session).

## Configuration

Priority (highest first): CLI `--set` > managed `/etc/zerobot/` > local `.zerobot/settings.local.yaml` > project `.zerobot/settings.yaml` > user `~/.zerobot/settings.yaml` > defaults.

Runtime paths: session DB at `~/.zerobot/state/workspaces/{workspace}/zerobot.db`, logs at `~/.zerobot/logs/YYYY-MM-DD.log`.

Environment variables: `OPENAI_API_KEY` or `ANTHROPIC_API_KEY` (or configure via settings YAML).

## Conventions

- Rust 2021 edition, `resolver = "2"`, Apache-2.0 license.
- Comments, docs, and log messages are in Chinese; code identifiers are in English.
- Do not create docs, commit code, or push changes unless explicitly asked.
- `tmp/` contains external reference material, not project source code.
- `AGENTS.md` files at multiple levels provide project context for AI assistants.
