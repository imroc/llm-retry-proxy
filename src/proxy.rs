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

    metrics.record_request(route_name);

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

    let model = extract_model(&body_bytes);
    let tag = if let Some(ref m) = model {
        format!("[{}|{}]", route_name, m)
    } else {
        format!("[{}]", route_name)
    };

    // Protocol transform: /v1/responses → /v1/chat/completions
    let transform_active = route_config.transform.as_deref() == Some(transform::RESPONSES_TO_CHAT);
    // Rewrite path for upstream: strip the leading version prefix since
    // target already contains it, then replace responses with chat/completions
    let rest = if transform_active {
        // Strip leading "v1/" or "v2/" version prefix (target already contains it)
        let stripped = rest
            .strip_prefix("v1/")
            .or_else(|| rest.strip_prefix("v2/"))
            .unwrap_or(&rest);
        stripped.replace("responses", "chat/completions")
            .trim_start_matches("v1/")
            .to_string()
    } else {
        rest.to_string()
    };

    let upstream_url = build_upstream_url(&route_config.target, &rest);

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

                    metrics.record_retry(route_name);
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
                metrics.record_upstream_status(route_name, status);
                let duration = start_time.elapsed();
                metrics.record_duration(route_name, duration);

                if transform_active {
                    return build_transform_response(
                        response,
                        model.as_deref().unwrap_or(""),
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

                metrics.record_retry(route_name);
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

/// Build a response for the responses_to_chat transform.
///
/// For non-streaming: buffer the full body and convert.
/// For streaming: convert SSE chunks in real time.
async fn build_transform_response(
    response: reqwest::Response,
    model: &str,
    tag: &str,
    upstream_url: &str,
    disconnect_token: CancellationToken,
) -> Response<ResponseBody> {
    let tag = tag.to_string();
    let upstream_url = upstream_url.to_string();
    let model = model.to_string();

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
        let converted = transform::chat_response_to_responses(&chat_body).unwrap_or(chat_body);
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
    let (tx, rx) = mpsc::channel::<
        Result<hyper::body::Frame<Bytes>, Box<dyn std::error::Error + Send + Sync>>,
    >(32);

    let tag_clone = tag.clone();
    let url_clone = upstream_url.clone();
    tokio::spawn(async move {
        let mut response = response;
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
