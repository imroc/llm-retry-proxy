# LLM Proxy

[中文文档](./README.zh-CN.md)

A thin local proxy for LLM APIs with automatic retry, protocol transform, and model-level routing. Designed for team-shared API scenarios where rate limiting (429) frequently interrupts AI CLI tool sessions.

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
- **Model-level routing** — route different models to different upstreams within a single route, enabling single-provider multi-model scenarios for tools like Codex
- **Prometheus metrics** — monitor retry rates and upstream status (per route + model)
- **Single binary** — low memory footprint, written in Rust

## Quick Start

```bash
# Build
make build

# Create config
cp config.example.toml config.toml
# Edit config.toml to point to your API

# Run
./target/release/llm-proxy --config config.toml --log-level info
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

### Model-level Routing

When the request body contains a `model` field, the proxy looks it up in the route's `models` map. If found, model-level config overrides the route-level config. This enables a single route (provider) to manage multiple models from different upstreams — ideal for AI CLI tools that only allow model switching within the same provider.

```toml
[routes.tkehub]
target = "http://tkehub.woa.com"
transform = "responses_to_chat"  # route-level default

# GLM supports Responses API natively — direct passthrough, no transform
[routes.tkehub.models."tke/glm-latest"]
transform = "none"

# DeepSeek goes to a different upstream with model name mapping
[routes.tkehub.models."tke/deepseek-flash-latest"]
target = "https://tokenhub.tencentmaas.com"
upstream_model = "deepseek-chat"
rewrite_response_model = true
max_delay_ms = 30000
```

Model-level fields (all optional — only specified fields override):

| Field | Description |
|-------|-------------|
| `target` | Upstream API URL |
| `transform` | `"responses_to_chat"` or `"none"` to explicitly disable |
| `upstream_model` | Rewrite the `model` field in the request body |
| `rewrite_response_model` | Rewrite the `model` field in responses back to the client's model name (default: false) |
| retry params | `max_retries`, `base_delay_ms`, `max_delay_ms`, `max_total_wait_ms`, `connect_timeout_secs`, `retry_status_codes` |

## Installation

```bash
make install
```

This interactively installs the binary and optionally sets up a systemd/launchd service.

## License

[MIT](./LICENSE)
