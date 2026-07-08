use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};
use std::sync::Arc;
use std::time::Duration;

pub struct Metrics {
    registry: Registry,
    requests_total: IntCounterVec,
    retries_total: IntCounterVec,
    upstream_status_total: IntCounterVec,
    request_duration_seconds: HistogramVec,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        let registry = Registry::new();

        let requests_total = IntCounterVec::new(
            Opts::new("proxy_requests_total", "Total requests received"),
            &["route"],
        )
        .unwrap();
        registry.register(Box::new(requests_total.clone())).ok();

        let retries_total = IntCounterVec::new(
            Opts::new("proxy_retries_total", "Total retry attempts"),
            &["route"],
        )
        .unwrap();
        registry.register(Box::new(retries_total.clone())).ok();

        let upstream_status_total = IntCounterVec::new(
            Opts::new(
                "proxy_upstream_status_total",
                "Upstream status code distribution",
            ),
            &["route", "status"],
        )
        .unwrap();
        registry
            .register(Box::new(upstream_status_total.clone()))
            .ok();

        let request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "proxy_request_duration_seconds",
                "Request duration in seconds",
            ),
            &["route"],
        )
        .unwrap();
        registry
            .register(Box::new(request_duration_seconds.clone()))
            .ok();

        Arc::new(Self {
            registry,
            requests_total,
            retries_total,
            upstream_status_total,
            request_duration_seconds,
        })
    }

    pub fn record_request(&self, route: &str) {
        self.requests_total.with_label_values(&[route]).inc();
    }

    pub fn record_retry(&self, route: &str) {
        self.retries_total.with_label_values(&[route]).inc();
    }

    pub fn record_upstream_status(&self, route: &str, status: u16) {
        self.upstream_status_total
            .with_label_values(&[route, &status.to_string()])
            .inc();
    }

    pub fn record_duration(&self, route: &str, duration: Duration) {
        self.request_duration_seconds
            .with_label_values(&[route])
            .observe(duration.as_secs_f64());
    }

    pub fn render(&self) -> String {
        let mut buf = Vec::new();
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        encoder.encode(&metric_families, &mut buf).ok();
        String::from_utf8(buf).unwrap_or_default()
    }
}
