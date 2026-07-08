# LLM Retry Proxy

[中文文档](./README.zh-CN.md)

A transparent local reverse proxy that adds unlimited automatic retry to any OpenAI-compatible LLM API. Designed for team-shared API scenarios where rate limiting (429) frequently interrupts AI CLI tool sessions.

## Why?

When using AI CLI tools (CodeBuddy Code, Claude Code, Codex CLI, OpenCode) with team-shared model APIs, global rate limits cause 429 errors that interrupt sessions. Most CLI tools have limited or no retry capability, requiring manual "continue" to resume work.

This proxy sits between the CLI tool and the API, transparently retrying requests that receive 429/5xx errors with exponential backoff + jitter, so the CLI tool never sees the error.

## Features

- **Unlimited retry** — keeps retrying until the client disconnects or succeeds
- **Protocol-agnostic** — works with OpenAI, Anthropic Messages, and any HTTP API format
- **Transparent** — CLI tools don't need any retry support; just point them at the proxy
- **Streaming support** — SSE streaming passthrough without buffering
- **Client-aware** — detects client disconnect immediately (even during backoff) and stops retrying
- **Hot-reload config** — add/remove routes without restart
- **Prometheus metrics** — monitor retry rates and upstream status
- **Single binary** — low memory footprint, written in Rust

## Quick Start

```bash
# Build
make build

# Create config
cp config.example.toml config.toml
# Edit config.toml to point to your API

# Run
./target/release/llm-retry-proxy --config config.toml --log-level info
```

Point your AI CLI tool's API URL to `http://127.0.0.1:8888/{route_name}/{api_path}`.

See [docs/tools/](./docs/tools/) for configuration guides for specific AI CLI tools.

## Configuration

```toml
[defaults]
max_retries = 9999           # Effectively unlimited
base_delay_ms = 1000         # Exponential backoff base
max_delay_ms = 60000         # Backoff cap
max_total_wait_ms = 0        # 0 = rely on client disconnect
connect_timeout_secs = 30
retry_status_codes = [429, 500, 502, 503, 504, 408, 529]

[routes.myapi]
target = "https://api.example.com"
# Route-level overrides (all optional):
# max_retries = 500
# base_delay_ms = 2000
# max_delay_ms = 30000
```

See [config.example.toml](./config.example.toml) for a complete example.

## Installation

```bash
make install
```

This interactively installs the binary and optionally sets up a systemd/launchd service.

## License

[MIT](./LICENSE)
