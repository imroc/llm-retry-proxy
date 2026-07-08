use arc_swap::ArcSwap;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::config::Config;
use crate::metrics::Metrics;
use crate::proxy;

pub async fn run(
    config: Arc<ArcSwap<Config>>,
    metrics: Arc<Metrics>,
    client: reqwest::Client,
    addr: SocketAddr,
    version: &'static str,
    mut shutdown: broadcast::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    info!("listening on http://{}", addr);

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, peer) = match accept_result {
                    Ok(v) => v,
                    Err(e) => {
                        error!("accept error: {}", e);
                        continue;
                    }
                };

                let config = config.clone();
                let metrics = metrics.clone();
                let client = client.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, peer, config, metrics, client, version).await {
                        debug!("connection error: {}", e);
                    }
                });
            }
            _ = shutdown.recv() => {
                info!("shutdown signal received, stopping new connections");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    config: Arc<ArcSwap<Config>>,
    metrics: Arc<Metrics>,
    client: reqwest::Client,
    version: &'static str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Client disconnect detection.
    //
    // A CancellationToken is passed to the proxy handler. It is used in
    // tokio::select! during backoff waits and streaming to allow early
    // cancellation when the client disconnects.
    //
    // Disconnect detection during streaming: the response body channel's
    // sender.send() will fail when the receiver (hyper) drops the response
    // body, which happens when the client disconnects.
    //
    // Disconnect detection during retry loop (before any response is sent):
    // not actively monitored at TCP level. A TCP dup'd FD + MSG_PEEK approach
    // was evaluated but rejected due to false positives from HTTP/1.1
    // half-close behavior (clients sending "Connection: close" half-close
    // the write side after the request, causing false EOF on the dup'd FD).
    // See ADR-0002 for the design rationale and this trade-off.
    //
    // If the client disconnects during a retry backoff, the proxy will
    // continue retrying until: the upstream returns a non-retryable response
    // (then the write fails), or max_retries / max_total_wait is hit.
    // This wastes at most one upstream request, which is acceptable.
    let disconnect_token = CancellationToken::new();

    let io = TokioIo::new(stream);
    let service = service_fn(move |req: http::Request<Incoming>| {
        let config = config.clone();
        let metrics = metrics.clone();
        let client = client.clone();
        let token = disconnect_token.clone();
        async move {
            Ok::<_, std::convert::Infallible>(
                proxy::handle_request(req, config, metrics, client, token, version).await,
            )
        }
    });

    http1::Builder::new()
        .keep_alive(false)
        .serve_connection(io, service)
        .await?;

    debug!("connection closed: {}", peer);
    Ok(())
}
