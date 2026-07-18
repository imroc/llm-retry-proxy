use arc_swap::ArcSwap;
use bytes::Bytes;
use http::{HeaderMap, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, StreamBody};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;
use crate::health::ResponseBody;
use crate::log::extract_model;
use crate::metrics::Metrics;
use crate::retry::{
    compute_backoff_ms, compute_delay_ms, is_retryable_status, parse_retry_after_ms,
};
use crate::transform;

const HOP_BY_HOP_REQUEST: &[&str] = &[
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "keep-alive",
    "proxy-authorization",
    "te",
    "trailer",
    "upgrade",
];

const HOP_BY_HOP_RESPONSE: &[&str] = &[
    "content-length",
    "content-encoding",
    "transfer-encoding",
    "connection",
    "keep-alive",
];

fn error_response(status: StatusCode, message: &str, error_type: &str) -> Response<ResponseBody> {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": error_type,
        }
    });
    let mut resp = Response::new(
        Full::new(Bytes::from(body.to_string()))
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            .boxed(),
    );
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    resp
}

fn match_route(path: &str) -> Option<(&str, &str)> {
    let path = path.strip_prefix('/')?;
    let (route_name, rest) = path.split_once('/')?;
    if route_name.is_empty() {
        return None;
    }
    Some((route_name, rest))
}

fn build_forward_headers(req_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (key, value) in req_headers {
        let lower = key.as_str().to_lowercase();
        if HOP_BY_HOP_REQUEST.contains(&lower.as_str()) {
            continue;
        }
        headers.insert(key.clone(), value.clone());
    }
    headers
}

fn strip_response_hop_by_hop(resp_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (key, value) in resp_headers {
        let lower = key.as_str().to_lowercase();
        if HOP_BY_HOP_RESPONSE.contains(&lower.as_str()) {
            continue;
        }
        headers.insert(key.clone(), value.clone());
    }
    headers
}

fn build_upstream_url(target: &str, rest: &str) -> String {
    let target = target.trim_end_matches('/');
    format!("{}/{}", target, rest)
}

/// Rewrite the `model` field in a JSON request body.
///
/// If the body is not valid JSON or has no `model` field, returns the original bytes.
fn rewrite_model_in_body(body: &[u8], new_model: &str) -> Bytes {
    let mut value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Bytes::copy_from_slice(body),
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(new_model.to_string()),
        );
        match serde_json::to_vec(&value) {
            Ok(bytes) => Bytes::from(bytes),
            Err(_) => Bytes::copy_from_slice(body),
        }
    } else {
        Bytes::copy_from_slice(body)
    }
}

pub async fn handle_request(
    req: Request<hyper::body::Incoming>,
    config: Arc<ArcSwap<Config>>,
    metrics: Arc<Metrics>,
    client: reqwest::Client,
    disconnect_token: CancellationToken,
    version: &'static str,
) -> Response<ResponseBody> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    // Health check
    if path == "/healthz" {
        return crate::health::handle_healthz(config, version);
    }

    // Metrics
    if path == "/metrics" {
        return crate::health::handle_metrics(metrics);
    }

    // Route matching
    let (route_name, rest) = match match_route(&path) {
        Some(v) => v,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                &format!(
                    "retry-proxy: unknown route \"{}\". Available routes: {}",
                    path,
                    config.load().route_names().join(", ")
                ),
                "retry_proxy_unknown_route",
            );
        }
    };

    let config = config.load();
    let route_config = match config.resolve_route(route_name) {
        Some(rc) => rc,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                &format!(
                    "retry-proxy: unknown route \"{}\". Available routes: {}",
                    route_name,
                    config.route_names().join(", ")
                ),
                "retry_proxy_unknown_route",
            );
        }
    };

    // Extract headers before consuming body
    let forward_headers = build_forward_headers(req.headers());

    // Read full request body (for replay + model extraction)
    let has_body = method != Method::GET && method != Method::HEAD;
    let body_bytes: Bytes = if has_body {
        match req.collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(e) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &format!("retry-proxy: failed to read request body: {}", e),
                    "retry_proxy_read_body_failed",
                );
            }
        }
    } else {
        Bytes::new()
    };

    // Extract client model and resolve model-level config
    let client_model = extract_model(&body_bytes);
    let model_str = client_model.as_deref().unwrap_or("");

    // Two-step resolve: route-level → model-level
    let route_config = if let Some(ref model) = client_model {
        route_config.resolve_model(model)
    } else {
        route_config
    };

    // Rewrite model in request body if upstream_model is configured
    let body_bytes = if let Some(ref upstream_model) = route_config.upstream_model {
        if upstream_model != model_str {
            tracing::debug!(
                "[{}|{}] rewriting model: {} → {}",
                route_name,
                model_str,
                model_str,
                upstream_model
            );
            rewrite_model_in_body(&body_bytes, upstream_model)
        } else {
            body_bytes
        }
    } else {
        body_bytes
    };

    let tag = if !model_str.is_empty() {
        format!("[{}|{}]", route_name, model_str)
    } else {
        format!("[{}]", route_name)
    };

    metrics.record_request(route_name, model_str);

    // Protocol transform: /v1/responses → /v1/chat/completions
    let transform_active = route_config.transform.as_deref() == Some(transform::RESPONSES_TO_CHAT);
    // Rewrite path for upstream: strip the leading version prefix since
    // target already contains it, then replace responses with chat/completions
    let rest = if transform_active {
        // Strip leading "v1/" or "v2/" version prefix (target already contains it)
        let stripped = rest
            .strip_prefix("v1/")
            .or_else(|| rest.strip_prefix("v2/"))
            .unwrap_or(rest);
        stripped
            .replace("responses", "chat/completions")
            .to_string()
    } else {
        rest.to_string()
    };

    let upstream_url = build_upstream_url(&route_config.target, &rest);

    // Determine the model name to use in responses (for rewrite_response_model)
    let response_rewrite_model = if route_config.rewrite_response_model {
        Some(model_str.to_string())
    } else {
        None
    };

    let start_time = Instant::now();
    let mut attempt: u32 = 0;
    let mut total_wait_ms: u64 = 0;

    loop {
        // Check max_retries
        if attempt >= route_config.max_retries {
            warn!(
                "{} -> {} retry exhausted (max_retries={}), returning 502",
                tag, upstream_url, route_config.max_retries
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!(
                    "retry-proxy: upstream request failed after {} attempts (max retries exceeded)",
                    attempt
                ),
                "retry_proxy_upstream_failed",
            );
        }

        // Check max_total_wait_ms (fallback)
        if route_config.max_total_wait_ms > 0 && total_wait_ms >= route_config.max_total_wait_ms {
            warn!(
                "{} -> {} total wait budget exceeded ({}ms), returning 502",
                tag, upstream_url, route_config.max_total_wait_ms
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!(
                    "retry-proxy: upstream request failed after total wait budget ({}ms) exceeded",
                    route_config.max_total_wait_ms
                ),
                "retry_proxy_upstream_failed",
            );
        }

        // Build request body (apply transform if active)
        let request_body = if transform_active {
            match transform::responses_to_chat_request(&body_bytes) {
                Some(body) => {
                    tracing::debug!("{} transform: responses → chat ({} bytes)", tag, body.len());
                    body
                }
                None => {
                    tracing::warn!("{} transform failed, sending original body", tag);
                    body_bytes.clone()
                }
            }
        } else {
            body_bytes.clone()
        };

        tracing::debug!(
            "{} upstream URL: {} body={}bytes",
            tag,
            upstream_url,
            request_body.len()
        );

        // Build request
        let req_builder = client
            .request(method.clone(), &upstream_url)
            .headers(forward_headers.clone());

        let req_builder = if has_body {
            req_builder.body(request_body)
        } else {
            req_builder
        };

        // Send request with disconnect detection
        let send_result = tokio::select! {
            r = req_builder.send() => r,
            _ = disconnect_token.cancelled() => {
                info!("{} -> {} client disconnected during request", tag, upstream_url);
                return error_response(
                    StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_GATEWAY),
                    "retry-proxy: client disconnected",
                    "retry_proxy_client_disconnected",
                );
            }
        };

        match send_result {
            Ok(response) => {
                let status = response.status().as_u16();
                let retryable = is_retryable_status(status, &route_config.retry_status_codes);

                if retryable && attempt < route_config.max_retries {
                    // Extract headers before consuming body
                    let retry_after_ms = parse_retry_after_ms(response.headers());
                    // Read error body for logging
                    let body_text = response.text().await.unwrap_or_default();
                    let backoff_ms = compute_backoff_ms(
                        route_config.base_delay_ms,
                        route_config.max_delay_ms,
                        attempt,
                    );
                    let delay_ms = compute_delay_ms(retry_after_ms, backoff_ms);

                    // Check total wait budget
                    if route_config.max_total_wait_ms > 0
                        && total_wait_ms + delay_ms > route_config.max_total_wait_ms
                    {
                        warn!(
                            "{} -> {} total wait would exceed budget ({}+{} > {}ms), returning 502",
                            tag,
                            upstream_url,
                            total_wait_ms,
                            delay_ms,
                            route_config.max_total_wait_ms
                        );
                        return error_response(
                            StatusCode::BAD_GATEWAY,
                            &format!(
                                "retry-proxy: upstream request failed after total wait budget ({}ms) would be exceeded",
                                route_config.max_total_wait_ms
                            ),
                            "retry_proxy_upstream_failed",
                        );
                    }

                    info!(
                        "{} -> {} HTTP {} retry {}/{} in {}ms (total_wait={}ms) body={}",
                        tag,
                        upstream_url,
                        status,
                        attempt + 1,
                        route_config.max_retries,
                        delay_ms,
                        total_wait_ms + delay_ms,
                        if body_text.len() > 500 {
                            &body_text[..500]
                        } else {
                            &body_text
                        }
                    );

                    metrics.record_retry(route_name, model_str);
                    attempt += 1;
                    total_wait_ms += delay_ms;

                    // Sleep with disconnect detection
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                        _ = disconnect_token.cancelled() => {
                            info!("{} -> {} client disconnected during backoff", tag, upstream_url);
                            return error_response(
                                StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_GATEWAY),
                                "retry-proxy: client disconnected",
                                "retry_proxy_client_disconnected",
                            );
                        }
                    }
                    continue;
                }

                if retryable {
                    warn!(
                        "{} -> {} retry exhausted (max_retries={}), returning status={}",
                        tag, upstream_url, route_config.max_retries, status
                    );
                }

                // Non-retryable or exhausted: stream response to client
                metrics.record_upstream_status(route_name, model_str, status);
                let duration = start_time.elapsed();
                metrics.record_duration(route_name, model_str, duration);

                if transform_active {
                    return build_transform_response(
                        response,
                        response_rewrite_model.as_deref(),
                        &tag,
                        &upstream_url,
                        disconnect_token,
                    )
                    .await;
                }

                if response_rewrite_model.is_some() {
                    return build_streaming_response_with_rewrite(
                        response,
                        response_rewrite_model.as_deref().unwrap_or(""),
                        &tag,
                        &upstream_url,
                        disconnect_token,
                    )
                    .await;
                }

                return build_streaming_response(response, &tag, &upstream_url, disconnect_token)
                    .await;
            }
            Err(e) => {
                // Network error
                if attempt >= route_config.max_retries {
                    warn!(
                        "{} -> {} network error exhausted (max_retries={}): {}",
                        tag, upstream_url, route_config.max_retries, e
                    );
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        &format!(
                            "retry-proxy: upstream request failed after {} attempts: {}",
                            attempt + 1,
                            e
                        ),
                        "retry_proxy_upstream_failed",
                    );
                }

                let backoff_ms = compute_backoff_ms(
                    route_config.base_delay_ms,
                    route_config.max_delay_ms,
                    attempt,
                );
                let delay_ms = compute_delay_ms(None, backoff_ms);

                if route_config.max_total_wait_ms > 0
                    && total_wait_ms + delay_ms > route_config.max_total_wait_ms
                {
                    warn!(
                        "{} -> {} total wait would exceed budget ({}+{} > {}ms), returning 502",
                        tag, upstream_url, total_wait_ms, delay_ms, route_config.max_total_wait_ms
                    );
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        &format!(
                            "retry-proxy: upstream request failed after total wait budget ({}ms) would be exceeded: {}",
                            route_config.max_total_wait_ms, e
                        ),
                        "retry_proxy_upstream_failed",
                    );
                }

                info!(
                    "{} -> {} network error retry {}/{} in {}ms (total_wait={}ms): {}",
                    tag,
                    upstream_url,
                    attempt + 1,
                    route_config.max_retries,
                    delay_ms,
                    total_wait_ms + delay_ms,
                    e
                );

                metrics.record_retry(route_name, model_str);
                attempt += 1;
                total_wait_ms += delay_ms;

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                    _ = disconnect_token.cancelled() => {
                        info!("{} -> {} client disconnected during backoff", tag, upstream_url);
                        return error_response(
                            StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_GATEWAY),
                            "retry-proxy: client disconnected",
                            "retry_proxy_client_disconnected",
                        );
                    }
                }
                continue;
            }
        }
    }
}

async fn build_streaming_response(
    response: reqwest::Response,
    tag: &str,
    upstream_url: &str,
    disconnect_token: CancellationToken,
) -> Response<ResponseBody> {
    let status = response.status();
    let headers = strip_response_hop_by_hop(response.headers());
    let tag = tag.to_string();
    let upstream_url = upstream_url.to_string();

    let (tx, rx) = mpsc::channel::<
        Result<hyper::body::Frame<Bytes>, Box<dyn std::error::Error + Send + Sync>>,
    >(32);

    // Spawn streaming task
    tokio::spawn(async move {
        let mut response = response;
        loop {
            tokio::select! {
                chunk = response.chunk() => {
                    match chunk {
                        Ok(Some(bytes)) => {
                            if tx.send(Ok(hyper::body::Frame::data(bytes))).await.is_err() {
                                // Client disconnected (receiver dropped)
                                break;
                            }
                        }
                        Ok(None) => break, // Stream ended normally
                        Err(e) => {
                            warn!("upstream stream error: {}", e);
                            let _ = tx.send(Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>)).await;
                            break;
                        }
                    }
                }
                _ = disconnect_token.cancelled() => {
                    info!("{} -> {} client disconnected during streaming", tag, upstream_url);
                    break;
                }
            }
        }
        // Dropping `response` closes the reqwest connection
        drop(response);
    });

    let stream = ReceiverStream::new(rx);
    let body = StreamBody::new(stream);

    let mut resp = Response::new(BodyExt::boxed(body));
    *resp.status_mut() = status;
    for (key, value) in &headers {
        resp.headers_mut().insert(key.clone(), value.clone());
    }
    resp
}

/// Build a streaming response with model field rewriting.
///
/// For SSE responses: parse each `data: {...}` line, replace the `model` field, re-serialize.
/// For non-SSE responses: buffer the full body, parse JSON, replace model, send.
async fn build_streaming_response_with_rewrite(
    response: reqwest::Response,
    rewrite_model: &str,
    tag: &str,
    upstream_url: &str,
    disconnect_token: CancellationToken,
) -> Response<ResponseBody> {
    let status = response.status();
    let headers = strip_response_hop_by_hop(response.headers());
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    let tag = tag.to_string();
    let upstream_url = upstream_url.to_string();
    let rewrite_model = rewrite_model.to_string();

    if !is_sse {
        // Non-SSE: buffer full body, parse JSON, replace model
        let body_bytes = response.bytes().await.unwrap_or_default();
        let rewritten = rewrite_model_in_json(&body_bytes, &rewrite_model);
        let mut resp = Response::new(
            Full::new(rewritten)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
                .boxed(),
        );
        *resp.status_mut() = status;
        for (key, value) in &headers {
            resp.headers_mut().insert(key.clone(), value.clone());
        }
        return resp;
    }

    // SSE streaming with model rewrite
    let (tx, rx) = mpsc::channel::<
        Result<hyper::body::Frame<Bytes>, Box<dyn std::error::Error + Send + Sync>>,
    >(32);

    let tag_clone = tag.clone();
    let url_clone = upstream_url.clone();
    tokio::spawn(async move {
        let mut response = response;
        let mut buf = String::new();

        loop {
            tokio::select! {
                chunk = response.chunk() => {
                    match chunk {
                        Ok(Some(bytes)) => {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            // Process complete lines
                            while let Some(i) = buf.find('\n') {
                                let line = buf[..=i].to_string();
                                buf = buf[i+1..].to_string();
                                let rewritten = rewrite_model_in_sse_line(&line, &rewrite_model);
                                let data = Bytes::from(rewritten);
                                if tx.send(Ok(hyper::body::Frame::data(data))).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            warn!("upstream stream error: {}", e);
                            break;
                        }
                    }
                }
                _ = disconnect_token.cancelled() => {
                    info!("{} -> {} client disconnected during streaming (rewrite)", tag_clone, url_clone);
                    break;
                }
            }
        }
        drop(response);
    });

    let stream = ReceiverStream::new(rx);
    let body = StreamBody::new(stream);

    let mut resp = Response::new(BodyExt::boxed(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        "text/event-stream".parse().unwrap(),
    );
    resp.headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    resp
}

/// Rewrite the `model` field in a JSON body.
fn rewrite_model_in_json(body: &[u8], new_model: &str) -> Bytes {
    let mut value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Bytes::copy_from_slice(body),
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(new_model.to_string()),
        );
    }
    match serde_json::to_vec(&value) {
        Ok(bytes) => Bytes::from(bytes),
        Err(_) => Bytes::copy_from_slice(body),
    }
}

/// Rewrite the `model` field in a single SSE line.
///
/// SSE lines that are not `data: {json}` are passed through unchanged.
fn rewrite_model_in_sse_line(line: &str, new_model: &str) -> String {
    let trimmed = line.trim_end();
    let Some(json_str) = trimmed.strip_prefix("data: ") else {
        return line.to_string();
    };
    let json_str = json_str.trim();
    if json_str == "[DONE]" {
        return line.to_string();
    }
    let mut value: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return line.to_string(),
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "model".to_string(),
            serde_json::Value::String(new_model.to_string()),
        );
    }
    match serde_json::to_string(&value) {
        Ok(serialized) => format!("data: {}\n\n", serialized),
        Err(_) => line.to_string(),
    }
}

/// Build a response for the responses_to_chat transform.
///
/// For non-streaming: buffer the full body and convert.
/// For streaming: convert SSE chunks in real time.
///
/// `rewrite_model`: if Some, use this model name in the response output instead
/// of the upstream model (for rewrite_response_model).
async fn build_transform_response(
    response: reqwest::Response,
    rewrite_model: Option<&str>,
    tag: &str,
    upstream_url: &str,
    disconnect_token: CancellationToken,
) -> Response<ResponseBody> {
    let tag = tag.to_string();
    let upstream_url = upstream_url.to_string();

    // Check content-type to determine streaming vs non-streaming
    let is_stream = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    if !is_stream {
        // Non-streaming: buffer and convert
        let chat_body = response.bytes().await.unwrap_or_default();
        let converted =
            transform::chat_response_to_responses(&chat_body, rewrite_model).unwrap_or(chat_body);
        let mut resp = Response::new(
            Full::new(converted)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
                .boxed(),
        );
        *resp.status_mut() = StatusCode::OK;
        resp.headers_mut().insert(
            http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        return resp;
    }

    // Streaming: convert SSE chunks
    // Use the rewrite model if provided, otherwise extract from the first response chunk
    let stream_model = rewrite_model.unwrap_or("").to_string();

    let (tx, rx) = mpsc::channel::<
        Result<hyper::body::Frame<Bytes>, Box<dyn std::error::Error + Send + Sync>>,
    >(32);

    let tag_clone = tag.clone();
    let url_clone = upstream_url.clone();
    tokio::spawn(async move {
        let mut response = response;
        // Determine model: if rewrite_model is set, use it; otherwise extract from first chunk
        let mut model = stream_model.clone();
        let mut state = transform::StreamTransformState::new(&model, None);
        // Buffer for incomplete SSE lines
        let mut buf = String::new();

        loop {
            tokio::select! {
                chunk = response.chunk() => {
                    match chunk {
                        Ok(Some(bytes)) => {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            // Process complete lines
                            while let Some(i) = buf.find('\n') {
                                let line = buf[..=i].trim_end().to_string();
                                buf = buf[i+1..].to_string();
                                // If we don't have a model yet (rewrite not set), try to extract from the chunk
                                if model.is_empty() {
                                    if let Some(m) = extract_model_from_sse_line(&line) {
                                        model = m;
                                        state.model = model.clone();
                                    }
                                }
                                if let Some(sse) = transform::transform_chat_sse_chunk(&line, &mut state) {
                                    let data = Bytes::from(sse);
                                    if tx.send(Ok(hyper::body::Frame::data(data))).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            warn!("upstream stream error: {}", e);
                            break;
                        }
                    }
                }
                _ = disconnect_token.cancelled() => {
                    info!("{} -> {} client disconnected during transform streaming", tag_clone, url_clone);
                    break;
                }
            }
        }
        // Always flush stream completion -- even if upstream closed without [DONE],
        // the client (Codex) needs response.completed to avoid "Reconnecting".
        // The completed flag in state prevents double-flush if [DONE] was already seen.
        if let Some(sse) = transform::transform_chat_sse_chunk("data: [DONE]", &mut state) {
            let data = Bytes::from(sse);
            let _ = tx.send(Ok(hyper::body::Frame::data(data))).await;
        }
        drop(response);
    });

    let stream = ReceiverStream::new(rx);
    let body = StreamBody::new(stream);

    let mut resp = Response::new(BodyExt::boxed(body));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        "text/event-stream".parse().unwrap(),
    );
    resp.headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    resp
}

/// Extract model name from an SSE `data: {...}` line.
fn extract_model_from_sse_line(line: &str) -> Option<String> {
    let data_str = line.strip_prefix("data: ")?.trim();
    if data_str == "[DONE]" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(data_str).ok()?;
    value.get("model")?.as_str().map(|s| s.to_string())
}
