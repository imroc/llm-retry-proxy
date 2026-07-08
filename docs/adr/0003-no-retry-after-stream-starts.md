# ADR-0003: No retry after streaming response has started (HTTP 200 sent)

## Context

The proxy retries requests that receive retryable status codes (429, 5xx) from the upstream. But LLM APIs typically use SSE (Server-Sent Events) streaming — the upstream returns HTTP 200 and then streams tokens incrementally. If the stream breaks mid-way (connection drop, upstream error after headers), the proxy has already forwarded the 200 status and partial SSE data to the Client. It cannot "take back" the response and retry transparently.

## Decision

The proxy **only retries before the upstream response headers are received**. Once the upstream returns HTTP 200 and the proxy begins forwarding the streaming body to the Client, no retry is attempted. If the stream breaks, the proxy simply closes the connection to the Client.

## Rationale

1. **HTTP semantics are irreversible.** Once the proxy sends `200 OK` + headers to the Client, it cannot retroactively change the status code or restart the response body. Any retry would require a new HTTP response, which the Client would not expect.
2. **Buffering the full stream breaks streaming semantics.** The alternative — buffering the entire SSE response before forwarding — would eliminate real-time token streaming. The Client would see a long blank pause followed by the entire response at once. For AI CLI tools, this is an unacceptable UX degradation.
3. **429/5xx errors arrive in headers, not mid-stream.** In practice, rate limit (429) and server error (5xx) responses are returned as complete HTTP responses with the error status in the headers — they never start as 200 and then switch to an error. The scenario where retry would help (pre-header errors) is exactly the scenario the proxy handles.
4. **Clients handle mid-stream failures themselves.** Claude Code (v2.1.199+) preserves partial output and prompts the user to reply "continue." The proxy should not duplicate this responsibility.

## Considered Options

- **Buffer full stream then forward**: enables retry on mid-stream failure, but destroys real-time streaming UX and risks OOM on large responses. Rejected.
- **Custom signaling to Client on stream break**: no standard protocol exists for this in OpenAI-compatible APIs. Clients cannot interpret the signal. Rejected.
