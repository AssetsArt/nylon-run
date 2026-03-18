use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{Histogram, exponential_buckets};
use prometheus_client::registry::Registry;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tracing::{error, info};

#[derive(Clone, Debug, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet)]
pub struct RequestLabels {
    pub method: String,
    pub status: u16,
    pub host: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet)]
pub struct ProcessLabels {
    pub name: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet)]
pub struct HostLabels {
    pub host: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet)]
pub struct CacheLabels {
    pub result: String, // "hit" or "miss"
}

#[derive(Clone)]
pub struct Metrics {
    pub http_requests_total: Family<RequestLabels, Counter>,
    pub http_request_duration_seconds: Histogram,
    pub active_connections: Gauge,
    pub cache_operations_total: Family<CacheLabels, Counter>,
    pub managed_processes: Gauge,
    pub process_restarts_total: Family<ProcessLabels, Counter>,
    pub process_cpu_usage: Family<ProcessLabels, Gauge>,
    pub process_memory_bytes: Family<ProcessLabels, Gauge>,
    pub network_received_bytes_total: Family<HostLabels, Counter>,
    pub network_sent_bytes_total: Family<HostLabels, Counter>,
    pub response_size_bytes: Histogram,
    pub upstream_errors_total: Family<HostLabels, Counter>,
    pub oci_pulls_total: Counter,
    pub oci_pull_errors_total: Counter,
}

impl Metrics {
    pub fn new_registered(registry: &mut Registry) -> Self {
        let http_requests_total = Family::<RequestLabels, Counter>::default();
        registry.register(
            "nyrun_http_requests",
            "Total HTTP requests proxied",
            http_requests_total.clone(),
        );

        let http_request_duration_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 16));
        registry.register(
            "nyrun_http_request_duration_seconds",
            "HTTP request latency in seconds",
            http_request_duration_seconds.clone(),
        );

        let active_connections = Gauge::default();
        registry.register(
            "nyrun_active_connections",
            "Currently active proxy connections",
            active_connections.clone(),
        );

        let cache_operations_total = Family::<CacheLabels, Counter>::default();
        registry.register(
            "nyrun_cache_operations",
            "Cache hit/miss counts",
            cache_operations_total.clone(),
        );

        let managed_processes = Gauge::default();
        registry.register(
            "nyrun_managed_processes",
            "Number of managed processes",
            managed_processes.clone(),
        );

        let process_restarts_total = Family::<ProcessLabels, Counter>::default();
        registry.register(
            "nyrun_process_restarts",
            "Total process restarts",
            process_restarts_total.clone(),
        );

        let process_cpu_usage = Family::<ProcessLabels, Gauge>::default();
        registry.register(
            "nyrun_process_cpu_usage_percent",
            "CPU usage per process (percentage, 0-100 per core)",
            process_cpu_usage.clone(),
        );

        let process_memory_bytes = Family::<ProcessLabels, Gauge>::default();
        registry.register(
            "nyrun_process_memory_bytes",
            "Resident memory per process in bytes",
            process_memory_bytes.clone(),
        );

        let network_received_bytes_total = Family::<HostLabels, Counter>::default();
        registry.register(
            "nyrun_network_received_bytes",
            "Total bytes received from clients",
            network_received_bytes_total.clone(),
        );

        let network_sent_bytes_total = Family::<HostLabels, Counter>::default();
        registry.register(
            "nyrun_network_sent_bytes",
            "Total bytes sent to clients",
            network_sent_bytes_total.clone(),
        );

        let response_size_bytes = Histogram::new(exponential_buckets(256.0, 4.0, 10));
        registry.register(
            "nyrun_response_size_bytes",
            "Response body size distribution in bytes",
            response_size_bytes.clone(),
        );

        let upstream_errors_total = Family::<HostLabels, Counter>::default();
        registry.register(
            "nyrun_upstream_errors",
            "Total upstream connection errors",
            upstream_errors_total.clone(),
        );

        let oci_pulls_total = Counter::default();
        registry.register(
            "nyrun_oci_pulls",
            "Total OCI image pulls",
            oci_pulls_total.clone(),
        );

        let oci_pull_errors_total = Counter::default();
        registry.register(
            "nyrun_oci_pull_errors",
            "Total OCI image pull failures",
            oci_pull_errors_total.clone(),
        );

        Self {
            http_requests_total,
            http_request_duration_seconds,
            active_connections,
            cache_operations_total,
            managed_processes,
            process_restarts_total,
            process_cpu_usage,
            process_memory_bytes,
            network_received_bytes_total,
            network_sent_bytes_total,
            response_size_bytes,
            upstream_errors_total,
            oci_pulls_total,
            oci_pull_errors_total,
        }
    }

    pub fn record_cache_hit(&self) {
        self.cache_operations_total
            .get_or_create(&CacheLabels {
                result: "hit".to_string(),
            })
            .inc();
    }

    pub fn record_cache_miss(&self) {
        self.cache_operations_total
            .get_or_create(&CacheLabels {
                result: "miss".to_string(),
            })
            .inc();
    }
}

/// Start a metrics HTTP server that can be stopped via a oneshot channel.
pub async fn serve_metrics_with_shutdown(
    port: u16,
    registry: Arc<Registry>,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let listener = match TcpListener::bind(format!("0.0.0.0:{}", port)).await {
        Ok(l) => l,
        Err(e) => {
            error!(port, error = %e, "failed to bind metrics server");
            return;
        }
    };

    info!(port, "metrics server listening");

    tokio::select! {
        _ = shutdown => {
            info!(port, "metrics server stopped");
        }
        _ = async {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(e) => {
                        error!(error = %e, "metrics accept error");
                        continue;
                    }
                };

                let registry = Arc::clone(&registry);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;

                    let mut body = String::new();
                    if encode(&mut body, &registry).is_err() {
                        let resp = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n";
                        let _ = stream.write_all(resp.as_bytes()).await;
                        return;
                    }

                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/openmetrics-text; version=1.0.0; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        } => {}
    }
}
