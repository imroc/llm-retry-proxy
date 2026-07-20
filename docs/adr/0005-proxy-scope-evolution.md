# ADR-0005: Proxy scope evolution — retry + transform + model routing

## Context

The project was originally named `llm-retry-proxy` and positioned as a "pure retry proxy" (ADR-0001). Since then, two capabilities have been added that go beyond pure retry:

1. **Protocol transform** (`responses_to_chat`): converts between OpenAI Responses API and Chat Completions API, enabling tools like Codex (which use Responses API) to work with any Chat Completions compatible provider.
2. **Model-level routing**: routes different models to different upstreams within a single route, enabling single-provider multi-model scenarios.

ADR-0001 explicitly rejected failover, load balancing, and rate limiting, and stated the project "does one thing." The new capabilities do not violate that boundary — they are still thin, transparent, deterministic layers on top of the core proxy. But the project name and positioning no longer reflect reality.

## Decision

Rename the project to `llm-proxy` and broaden the positioning from "pure retry proxy" to "thin local proxy for LLM APIs." The project's core capabilities are:

1. **Automatic retry** — unlimited transparent retry with exponential backoff + jitter
2. **Protocol transform** — request/response format conversion between API protocols
3. **Model-level routing** — per-model upstream selection within a single route

The project still does **not** do failover, load balancing, rate limiting, or budget management. The "thin layer" value proposition from ADR-0001 remains: a single binary, near-zero resource usage, transparent passthrough.

## Rationale

1. **Name should reflect scope.** `llm-retry-proxy` implies the project only does retry. After adding transform and model routing, the name is misleading to new users.

2. **Capabilities are coherent, not scope creep.** All three capabilities operate at the same layer — intercepting and forwarding HTTP requests to LLM APIs. They share the same config structure, the same request/response pipeline, and the same deployment model. They are not a slippery slope toward becoming a full gateway.

3. **ADR-0001's boundary still holds.** The new capabilities do not add upstream state tracking, health checks, failback strategies, token buckets, or queue management. The proxy remains stateless and deterministic.

4. **Crate name `llm-proxy-rs`** because `llm-proxy` was already taken on crates.io. The binary name remains `llm-proxy`.

## Considered Options

- **Keep name `llm-retry-proxy`**: misleading after adding transform and model routing. Rejected.

- **Rename to `llm-gateway`**: implies failover, load balancing, rate limiting, and budget management — capabilities the project explicitly does not have. Rejected.

- **Rename to `llm-proxy` + update ADR-0001 in place**: ADRs should be immutable historical records. ADR-0001 captured the decision at the time; ADR-0005 records the evolution. Accepted.
