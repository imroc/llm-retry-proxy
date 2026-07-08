# LLM Retry Proxy

A transparent local reverse proxy that adds unlimited automatic retry capability to any OpenAI-compatible LLM API, designed for team-shared API scenarios where rate limiting (429) frequently interrupts AI CLI tool sessions.

## Language

**Route**:
A named mapping from a URL path prefix to an upstream API server, with optional retry parameter overrides. The first path segment of an incoming request URL is the Route name (e.g., `/tkehub/v1/chat/completions` → Route `tkehub`).
_Avoid_: Upstream, Provider, Backend

**Upstream**:
The real LLM API server address that a Route's `target` points to. The proxy never accesses an Upstream directly; it always goes through a Route.
_Avoid_: Backend, Server, Target (Target is a config field name, not a concept)

**Attempt**:
A single HTTP request sent by the proxy to the upstream. The 1st Attempt is the initial request; subsequent Attempts after a failure are Retries.

**Retry**:
An Attempt made after the initial Attempt failed. "Retry #1" = the 2nd Attempt. The log format uses "retry N/M" where N starts from 1.
_Avoid_: Repeat, Redo

**Client**:
The AI CLI tool that sends requests to the proxy (e.g., CodeBuddy Code, Claude Code, Codex CLI, OpenCode). The proxy provides transparent retry service to the Client.
_Avoid_: User, Consumer

**Total Wait**:
Cumulative backoff wait time from when the proxy received the request to the current retry, excluding upstream response time. Used for `max_total_wait_ms` fallback judgment.
_Avoid_: Elapsed, Duration

**Client Disconnect**:
The primary abort signal for retry loops. When the Client closes the TCP connection (due to timeout or user cancellation), the proxy immediately stops retrying and closes the upstream connection, including during backoff waits.
