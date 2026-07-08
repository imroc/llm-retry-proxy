use arc_swap::ArcSwap;
use bytes::Bytes;
use http::StatusCode;
use http_body_util::{BodyExt, Full};
use std::sync::Arc;

use crate::config::Config;
use crate::metrics::Metrics;

pub type ResponseBody =
    http_body_util::combinators::BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

fn full_response(status: StatusCode, body: Bytes) -> http::Response<ResponseBody> {
    let mut resp = http::Response::new(
        Full::new(body)
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

pub fn handle_healthz(
    config: Arc<ArcSwap<Config>>,
    version: &'static str,
) -> http::Response<ResponseBody> {
    let config = config.load();
    let routes: Vec<&str> = config.route_names();
    let body = serde_json::json!({
        "ok": true,
        "version": version,
        "routes": routes,
    });
    full_response(StatusCode::OK, Bytes::from(body.to_string()))
}

pub fn handle_metrics(metrics: Arc<Metrics>) -> http::Response<ResponseBody> {
    let body = metrics.render();
    let mut resp = http::Response::new(
        Full::new(Bytes::from(body))
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            .boxed(),
    );
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        "text/plain; version=0.0.4".parse().unwrap(),
    );
    resp
}
