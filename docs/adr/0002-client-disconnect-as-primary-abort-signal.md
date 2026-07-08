# ADR-0002: Client disconnect as primary abort signal, max_total_wait as fallback

## Context

The proxy's core value proposition is "unlimited retry" — it should keep retrying until the request succeeds. But in practice, the AI CLI tool (Client) on the other end has its own timeout. If the proxy retries for longer than the Client's timeout, the Client disconnects and the retry work is wasted.

Different Clients have different timeouts:

| Client | Default timeout |
|--------|----------------|
| CodeBuddy Code | ~20 min (CODEBUDDY_STREAM_TIMEOUT_MS) |
| Claude Code | ~10 min (API_TIMEOUT_MS) |
| Codex CLI | Undisclosed, but finite |
| OpenCode | No timeout (indefinite) |

## Decision

The retry loop uses **two abort signals**, in priority order:

1. **Primary: Client disconnect.** The Client closing the TCP connection is the authoritative signal that "retry is no longer useful." The proxy listens for this throughout the retry loop — including during backoff waits — and responds immediately by stopping retries and closing the upstream connection.
2. **Fallback: `max_total_wait_ms`.** Only protects against the case where the Client has no timeout mechanism (e.g., OpenCode). Default is `0` (disabled — rely entirely on Client disconnect). Users can set a positive value as a safety net.

`max_retries` defaults to 9999 — a de facto "almost unlimited" value that prevents infinite loops in pathological cases but is never hit in practice.

## Rationale

1. **Client disconnect is the only reliable signal.** The proxy cannot know the Client's timeout value (it's not communicated in HTTP headers). But the Client itself enforces it — when the Client times out, it closes the connection. The proxy simply needs to listen.
2. **Backoff waits must be interruptible.** An earlier prototype had a bug where `await sleep(delay)` was not cancellable — the proxy would finish the full backoff wait before noticing the Client had disconnected. The fix: use `tokio::select!` to race the backoff timer against the Client disconnect signal.
3. **`max_total_wait` demoted from primary to fallback.** An earlier design used `max_total_wait` as the main constraint, hardcoded to match a specific Client's timeout. This was fragile (tied to one Client) and undercut the "unlimited retry" promise. The Client disconnect signal makes this unnecessary for most use cases.
4. **`max_retries=9999` not infinity.** A literal infinite loop risks CPU spin or resource leaks in edge cases (e.g., upstream returning 429 with zero-byte response indefinitely). 9999 attempts with 60s max backoff = ~16 hours of retries, which exceeds any realistic Client timeout.

## Considered Options

- **`max_total_wait` as primary constraint** (earlier design): required hardcoding to match a specific Client's timeout. Fragile and not truly "unlimited."
- **Adaptive timeout detection**: proxy learns Client timeouts by observing disconnect patterns. Over-engineered; the Client already communicates disconnect directly.
- **Truly infinite retry (no limits)**: risks pathological infinite loops; provides no safety net for Clients without timeouts.
