# ADR-0001: Pure retry proxy — no failover, load balancing, or rate limiting

## Context

The proxy is designed for a specific pain point: team-shared LLM APIs with global rate limits cause 429 errors that interrupt AI CLI tool sessions. Existing open-source projects (LiteLLM, Marg, aisix) tend to bundle retry with failover, load balancing, budget management, and observability into full LLM gateways.

## Decision

The proxy is a **pure retry layer** — it does one thing: transparently retry requests to the same upstream when receiving retryable status codes (429, 5xx) or network errors. It does not do:

- **Failover** (switching to a different provider on failure)
- **Load balancing** (distributing requests across multiple upstreams)
- **Rate limiting / rate shaping** (queuing or throttling outgoing requests)
- **Budget management** (tracking spend per key/user)

## Rationale

1. **Validated by existing prototype.** A Node.js prototype (~300 lines) already proves the pure retry approach works for the target scenario.
2. **Clear differentiation.** Other projects compete on being "full LLM gateways." This project competes on being the thinnest possible retry layer — a single static binary with near-zero resource usage.
3. **Right-sized complexity.** Failover requires health checks, failback strategies, and upstream state tracking. Rate limiting requires token buckets and queue management. Each feature multiplies complexity and pushes against the "thin layer" value proposition.
4. **Extensible later.** Failover and load balancing can be added as opt-in features without rewriting the core retry loop. Starting narrow avoids premature commitment to abstractions.

## Considered Options

- **Full LLM gateway** (retry + failover + load balancing + rate limiting + budgets): competes directly with LiteLLM et al. High engineering cost, diluted focus.
- **Retry + failover**: adds complexity (health checks, failback) for marginal benefit in the core scenario — the problem is "team rate limit on one provider," not "provider is down."
