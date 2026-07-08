use arc_swap::ArcSwap;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
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
    let disconnect_token = CancellationToken::new();

    // Client disconnect detection via TCP dup'd FD + MSG_PEEK.
    //
    // The monitor waits for the socket to become readable (which happens on
    // client disconnect: EOF or RST). It uses MSG_PEEK to avoid consuming
    // data from the shared kernel buffer.
    //
    // Known limitation: if the client sends pipelined data while waiting for
    // a response (rare with keep_alive(false) and LLM APIs), the monitor may
    // trigger a false disconnect. This is acceptable for the target use case.
    //
    // The monitor can be disabled with LLM_RETRY_PROXY_NO_DISCONNECT_MONITOR=1
    // for debugging or on platforms where dup/peek is not available.
    if std::env::var("LLM_RETRY_PROXY_NO_DISCONNECT_MONITOR").is_err() {
        let monitor_token = disconnect_token.clone();
        if let Ok(monitor_stream) = dup_tcp_stream(&stream) {
            tokio::spawn(async move {
                let stream = monitor_stream;
                loop {
                    if stream.readable().await.is_err() {
                        monitor_token.cancel();
                        return;
                    }
                    match peek_tcp_stream(&stream) {
                        Ok(0) | Err(_) => {
                            monitor_token.cancel();
                            return;
                        }
                        Ok(_) => {
                            // Data available — could be a false positive.
                            // Wait a short time and try again to confirm.
                            // If it's a genuine disconnect, the next peek
                            // will also return 0 or error.
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            match peek_tcp_stream(&stream) {
                                Ok(0) | Err(_) => {
                                    monitor_token.cancel();
                                    return;
                                }
                                Ok(_) => {
                                    // Still data — treat as disconnect (client
                                    // shouldn't send data while waiting).
                                    monitor_token.cancel();
                                    return;
                                }
                            }
                        }
                    }
                }
            });
        }
    }

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

/// Peek at a TCP stream using MSG_PEEK (does not consume data from buffer).
#[cfg(unix)]
fn peek_tcp_stream(stream: &TcpStream) -> std::io::Result<usize> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut buf = [0u8; 1];
    // SAFETY: recv with MSG_PEEK is a well-defined POSIX call. It reads data
    // from the socket without removing it from the receive buffer.
    let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut _, 1, libc::MSG_PEEK) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

#[cfg(not(unix))]
fn peek_tcp_stream(_stream: &TcpStream) -> std::io::Result<usize> {
    Ok(1) // No-op on non-Unix; disconnect detection disabled
}

/// Duplicate a TcpStream by dup()'ing the underlying raw FD.
#[cfg(unix)]
fn dup_tcp_stream(stream: &TcpStream) -> std::io::Result<TcpStream> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let raw_fd = stream.as_raw_fd();
    let new_fd = unsafe { libc::dup(raw_fd) };
    if new_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = unsafe { libc::fcntl(new_fd, libc::F_GETFL) };
    if flags < 0 {
        unsafe { libc::close(new_fd) };
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(new_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        unsafe { libc::close(new_fd) };
        return Err(std::io::Error::last_os_error());
    }
    let std_stream = unsafe { std::net::TcpStream::from_raw_fd(new_fd) };
    TcpStream::from_std(std_stream)
}

#[cfg(not(unix))]
fn dup_tcp_stream(_stream: &TcpStream) -> std::io::Result<TcpStream> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stream cloning not supported on this platform",
    ))
}
