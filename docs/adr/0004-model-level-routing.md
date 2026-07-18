# ADR-0004: Model-level routing within a Route

## Context

AI CLI tools like Codex CLI restrict model switching to within the same provider. This means all models accessible to the user must be served from the same `base_url`. In practice, different models may need to be routed to different upstream API servers, with different protocol transforms and retry strategies.

For example, a Codex provider pointing to `http://127.0.0.1:8888/tkehub/v1` needs to serve both:
- GLM models (directly accessible via Responses API, no transform needed)
- DeepSeek models (accessed via a different upstream, require `responses_to_chat` transform, and need model name mapping)

The existing route-level config cannot express this — each route maps to exactly one upstream with one transform.

## Decision

Introduce **model-level overrides** within a Route. When the request body contains a `model` field, the proxy looks it up in the route's `models` map. If found, model-level config fields override the route-level config. If not found, the route-level config is used as-is (backward compatible).

Model-level overrides support all route-level fields plus two new fields:
- `upstream_model` — rewrites the `model` field in the request body before forwarding
- `rewrite_response_model` — rewrites the `model` field in responses back to the client's original model name

The `transform` field supports the special value `"none"` to explicitly disable a route-level transform for a specific model.

## Rationale

1. **Does not violate ADR-0001 (pure retry proxy).** Model-level routing is not failover or load balancing — it does not switch upstreams on failure. It routes based on the `model` field in the request, which is deterministic and known before the first attempt. The retry loop still retries the same upstream for the same request.

2. **Enables single-provider multi-model scenarios.** AI CLI tools that restrict model switching to within a provider can now access models from different upstreams through a single proxy route. This is the primary use case.

3. **Backward compatible.** Routes without a `models` map behave exactly as before. Users can incrementally add model-level overrides without changing existing config.

4. **Two-step resolve keeps the design clean.** `resolve_route()` produces route-level config, then `resolve_model()` applies model-level overrides. This separation mirrors the existing defaults→route override pattern and keeps the resolve logic simple.

5. **`upstream_model` handles name mapping.** Different upstreams may use different model names (e.g., `tke/deepseek-flash-latest` vs `deepseek-chat`). Rewriting the `model` field in the request body before forwarding handles this transparently.

6. **`rewrite_response_model` for maximum compatibility.** By default, responses carry the upstream's model name. For clients that depend on the original model name, `rewrite_response_model = true` rewrites it back. This is opt-in to avoid unnecessary parsing overhead.

## Considered Options

- **Separate routes per model**: requires the user to configure multiple providers in the AI CLI tool, which may not be possible (Codex restricts model switching to within a provider).
- **Failover between upstreams**: rejected by ADR-0001. Model-level routing is deterministic routing, not failure-driven failover.
- **Full LLM gateway with model routing**: competes with LiteLLM et al. and violates the thin-layer value proposition. Model-level routing is a minimal addition that solves the specific problem.
