use async_trait::async_trait;
use bytes::Bytes;
use http::StatusCode;
use moka::future::Cache;
use pingora::http::ResponseHeader;
use pingora::upstreams::peer::HttpPeer;
use pingora_core::server::Server;
use pingora_core::server::configuration::ServerConf;
use pingora_proxy::{ProxyHttp, Session, http_proxy_service};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::acme::ChallengeStore;
use crate::metrics::{HostLabels, Metrics, RequestLabels};
use crate::tls::DynamicCertStore;

// --- Route types ---

#[derive(Clone, Debug)]
pub enum Backend {
    Proxy(SocketAddr),
    Spa(PathBuf),
}

#[derive(Clone, Debug)]
struct RouteEntry {
    name: String,
    port: u16,
    host: Option<String>,
    backend: Backend,
}

// --- Shared route table ---

#[derive(Default)]
struct RouteTable {
    entries: Vec<RouteEntry>,
}

impl RouteTable {
    fn find(&self, port: u16, host: Option<&str>) -> Option<&RouteEntry> {
        let port_routes: Vec<&RouteEntry> =
            self.entries.iter().filter(|r| r.port == port).collect();

        if let Some(h) = host
            && let Some(r) = port_routes.iter().find(|r| r.host.as_deref() == Some(h))
        {
            return Some(r);
        }
        // Fallback to catch-all (no host specified)
        port_routes.iter().find(|r| r.host.is_none()).copied()
    }
}

// --- Pingora ProxyHttp implementation ---

pub struct NyrunProxy {
    routes: Arc<RwLock<RouteTable>>,
    cache: Cache<String, (ResponseHeader, Bytes)>,
    metrics: Option<Metrics>,
    challenge_store: ChallengeStore,
}

pub struct ProxyCtx {
    cache_key: String,
    should_cache: bool,
    response_header: Option<ResponseHeader>,
    response_body: Vec<u8>,
    request_start: Instant,
    request_bytes: u64,
    response_bytes: u64,
}

/// Get the listening port from the session's socket digest
fn get_listen_port(session: &Session) -> u16 {
    if let Some(digest) = session.digest()
        && let Some(sd) = &digest.socket_digest
        && let Some(addr) = sd.local_addr()
        && let Some(inet) = addr.as_inet()
    {
        return inet.port();
    }
    80
}

#[async_trait]
impl ProxyHttp for NyrunProxy {
    type CTX = ProxyCtx;

    fn new_ctx(&self) -> Self::CTX {
        if let Some(m) = &self.metrics {
            m.active_connections.inc();
        }
        ProxyCtx {
            cache_key: String::new(),
            should_cache: false,
            response_header: None,
            response_body: Vec::new(),
            request_start: Instant::now(),
            request_bytes: 0,
            response_bytes: 0,
        }
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
        // Track request size from Content-Length
        ctx.request_bytes = session
            .req_header()
            .headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        // ACME HTTP-01 challenge handler
        let uri_path = session.req_header().uri.path();
        if let Some(token) = uri_path.strip_prefix("/.well-known/acme-challenge/")
            && let Some(key_auth) = self.challenge_store.get(token).await
        {
            let body = Bytes::from(key_auth);
            let mut header = ResponseHeader::build(StatusCode::OK, Some(2))?;
            header.insert_header("content-type", "text/plain")?;
            header.insert_header("content-length", body.len().to_string())?;
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session.write_response_body(Some(body), true).await?;
            return Ok(true);
        }

        let listen_port = get_listen_port(session);
        let host = extract_host(session);
        let table = self.routes.read().await;
        let route = table.find(listen_port, host.as_deref());

        match route {
            Some(RouteEntry {
                backend: Backend::Spa(dir),
                ..
            }) => {
                let uri_path = session.req_header().uri.path().to_string();
                let dir = dir.clone();
                drop(table);
                serve_spa(session, &uri_path, &dir).await?;
                Ok(true)
            }
            Some(RouteEntry {
                backend: Backend::Proxy(_),
                ..
            }) => {
                drop(table);

                // Only cache GET requests
                if session.req_header().method != http::Method::GET {
                    return Ok(false);
                }

                let host_str = host.as_deref().unwrap_or("localhost");
                let uri = session.req_header().uri.path();
                let query = session
                    .req_header()
                    .uri
                    .query()
                    .map_or(String::new(), |q| format!("?{}", q));

                let cache_key = format!("{}{}{}", host_str, uri, query);
                ctx.cache_key = cache_key.clone();

                // Check cache
                if let Some((mut header, body)) = self.cache.get(&cache_key).await {
                    debug!(key = %cache_key, "cache HIT");
                    if let Some(m) = &self.metrics {
                        m.record_cache_hit();
                    }
                    let _ = header.insert_header("X-Cache", "HIT");
                    session
                        .write_response_header(Box::new(header), true)
                        .await?;
                    session.write_response_body(Some(body), true).await?;
                    return Ok(true);
                }

                if let Some(m) = &self.metrics {
                    m.record_cache_miss();
                }
                Ok(false)
            }
            None => {
                drop(table);
                let body = Bytes::from("no route found\n");
                let mut header = ResponseHeader::build(StatusCode::NOT_FOUND, Some(2))?;
                header.insert_header("content-type", "text/plain")?;
                header.insert_header("content-length", body.len().to_string())?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session.write_response_body(Some(body), true).await?;
                Ok(true)
            }
        }
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        let listen_port = get_listen_port(session);
        let host = extract_host(session);
        let table = self.routes.read().await;
        let route = table.find(listen_port, host.as_deref());

        match route {
            Some(RouteEntry {
                backend: Backend::Proxy(addr),
                ..
            }) => {
                let peer = HttpPeer::new(*addr, false, String::new());
                Ok(Box::new(peer))
            }
            _ => Err(pingora::Error::explain(
                pingora::ErrorType::HTTPStatus(502),
                "no upstream route",
            )),
        }
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut pingora::http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        // X-Forwarded-For / X-Real-IP
        if let Some(client_addr) = session.client_addr() {
            let ip_str = client_addr.to_string();
            let ip = ip_str.split(':').next().unwrap_or(&ip_str);
            let _ = upstream_request.insert_header("X-Forwarded-For", ip);
            if !upstream_request.headers.contains_key("X-Real-IP") {
                let _ = upstream_request.insert_header("X-Real-IP", ip);
            }
        }

        // X-Forwarded-Proto
        let is_https = session.digest().is_some_and(|d| d.ssl_digest.is_some());
        let _ = upstream_request
            .insert_header("X-Forwarded-Proto", if is_https { "https" } else { "http" });

        // X-Forwarded-Host
        if let Some(host) = session.req_header().headers.get("Host")
            && let Ok(host_str) = host.to_str()
        {
            let _ = upstream_request.insert_header("X-Forwarded-Host", host_str);
        }

        Ok(())
    }

    async fn response_filter(
        &self,
        _session: &mut Session,
        resp: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        if ctx.cache_key.is_empty() {
            return Ok(());
        }

        // Only cache 200 OK responses
        if resp.status == StatusCode::OK {
            ctx.should_cache = true;
            ctx.response_header = Some(resp.clone());
        }

        Ok(())
    }

    fn response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<Option<Duration>> {
        // Track response bytes for all requests
        if let Some(b) = body {
            ctx.response_bytes += b.len() as u64;
        }

        if !ctx.should_cache {
            return Ok(None);
        }

        if let Some(b) = body {
            // Cap at 5MB to prevent OOM
            if ctx.response_body.len() + b.len() > 5 * 1024 * 1024 {
                ctx.should_cache = false;
                ctx.response_body.clear();
                return Ok(None);
            }
            ctx.response_body.extend_from_slice(b);
        }

        if end_of_stream && let Some(mut header) = ctx.response_header.take() {
            let cached_body = Bytes::from(std::mem::take(&mut ctx.response_body));
            let _ = header.remove_header("Transfer-Encoding");
            let _ = header.insert_header("Content-Length", cached_body.len().to_string());

            let cache_key = ctx.cache_key.clone();
            let cache = self.cache.clone();
            tokio::spawn(async move {
                cache.insert(cache_key, (header, cached_body)).await;
            });
        }

        Ok(None)
    }

    async fn logging(
        &self,
        session: &mut Session,
        e: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        if let Some(m) = &self.metrics {
            m.active_connections.dec();

            let duration = ctx.request_start.elapsed().as_secs_f64();
            m.http_request_duration_seconds.observe(duration);

            let status = session
                .response_written()
                .map(|r| r.status.as_u16())
                .unwrap_or(0);
            let method = session.req_header().method.to_string();
            let host = extract_host(session).unwrap_or_default();

            m.http_requests_total
                .get_or_create(&RequestLabels {
                    method,
                    status,
                    host: host.clone(),
                })
                .inc();

            // Network bytes
            let host_labels = HostLabels { host: host.clone() };
            m.network_received_bytes_total
                .get_or_create(&host_labels)
                .inc_by(ctx.request_bytes);
            m.network_sent_bytes_total
                .get_or_create(&host_labels)
                .inc_by(ctx.response_bytes);

            // Response size distribution
            m.response_size_bytes.observe(ctx.response_bytes as f64);

            // Upstream errors
            if e.is_some() {
                m.upstream_errors_total.get_or_create(&host_labels).inc();
            }
        }
    }
}

fn extract_host(session: &Session) -> Option<String> {
    session
        .req_header()
        .headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_string())
}

// --- SPA file serving ---

async fn serve_spa(session: &mut Session, uri_path: &str, dir: &Path) -> pingora::Result<()> {
    let clean_path = uri_path.trim_start_matches('/');
    let file_path = if clean_path.is_empty() {
        dir.join("index.html")
    } else {
        dir.join(clean_path)
    };

    // Security: prevent path traversal
    let canonical_dir = match dir.canonicalize() {
        Ok(d) => d,
        Err(_) => {
            let mut header = ResponseHeader::build(StatusCode::INTERNAL_SERVER_ERROR, Some(1))?;
            header.insert_header("content-type", "text/plain")?;
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session
                .write_response_body(Some(Bytes::from("SPA directory not found\n")), true)
                .await?;
            return Ok(());
        }
    };

    let resolved = if file_path.exists() {
        match file_path.canonicalize() {
            Ok(p) if p.starts_with(&canonical_dir) => p,
            _ => canonical_dir.join("index.html"),
        }
    } else {
        // SPA fallback: serve index.html for non-file routes
        canonical_dir.join("index.html")
    };

    match tokio::fs::read(&resolved).await {
        Ok(content) => {
            let mime = content_type(&resolved);
            let mut header = ResponseHeader::build(StatusCode::OK, Some(2))?;
            header.insert_header("content-type", mime)?;
            header.insert_header("content-length", content.len().to_string())?;
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session
                .write_response_body(Some(Bytes::from(content)), true)
                .await?;
        }
        Err(_) => {
            let mut header = ResponseHeader::build(StatusCode::NOT_FOUND, Some(1))?;
            header.insert_header("content-type", "text/plain")?;
            session
                .write_response_header(Box::new(header), false)
                .await?;
            session
                .write_response_body(Some(Bytes::from("not found\n")), true)
                .await?;
        }
    }

    Ok(())
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("xml") => "application/xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

// --- Proxy manager ---

const CACHE_CAPACITY: u64 = 10000;
const DEFAULT_CACHE_TTL_SECS: u64 = 3;

pub struct ProxyManager {
    routes: Arc<RwLock<RouteTable>>,
    cache: Cache<String, (ResponseHeader, Bytes)>,
    cache_ttl_secs: u64,
    ports: Vec<u16>,
    tls_ports: HashSet<u16>,
    cert_store: Arc<DynamicCertStore>,
    metrics: Option<Metrics>,
    challenge_store: ChallengeStore,
}

impl ProxyManager {
    pub fn new(metrics: Option<Metrics>, challenge_store: ChallengeStore) -> Self {
        let cache = Cache::builder()
            .max_capacity(CACHE_CAPACITY)
            .time_to_live(Duration::from_secs(DEFAULT_CACHE_TTL_SECS))
            .build();

        Self {
            routes: Arc::new(RwLock::new(RouteTable::default())),
            cache,
            cache_ttl_secs: DEFAULT_CACHE_TTL_SECS,
            ports: Vec::new(),
            tls_ports: HashSet::new(),
            cert_store: Arc::new(DynamicCertStore::new()),
            metrics,
            challenge_store,
        }
    }

    pub fn set_cache_ttl(&mut self, secs: u64) {
        self.cache_ttl_secs = secs;
        self.cache = Cache::builder()
            .max_capacity(CACHE_CAPACITY)
            .time_to_live(Duration::from_secs(secs))
            .build();
    }

    pub fn challenge_store(&self) -> &ChallengeStore {
        &self.challenge_store
    }

    pub fn cert_store(&self) -> &Arc<DynamicCertStore> {
        &self.cert_store
    }

    pub async fn add_route(
        &mut self,
        name: &str,
        port: u16,
        host: Option<String>,
        backend: Backend,
    ) -> Result<(), String> {
        self.add_route_with_tls(name, port, host, backend, None)
            .await
    }

    pub async fn add_route_with_tls(
        &mut self,
        name: &str,
        port: u16,
        host: Option<String>,
        backend: Backend,
        ssl: Option<(String, String)>, // (cert_path, key_path)
    ) -> Result<(), String> {
        {
            let mut table = self.routes.write().await;
            table.entries.retain(|r| r.name != name);
            table.entries.push(RouteEntry {
                name: name.to_string(),
                port,
                host: host.clone(),
                backend: backend.clone(),
            });
        }

        // Handle TLS cert
        if let Some((cert_path, key_path)) = &ssl {
            let sni_host = host.as_deref().unwrap_or("default");
            self.cert_store
                .add_cert(sni_host, cert_path, key_path)
                .await?;
            self.tls_ports.insert(port);
        }

        let need_new_listener = !self.ports.contains(&port);
        if need_new_listener {
            self.ports.push(port);
        }

        // Start a new Pingora server with ALL ports.
        // Pingora uses SO_REUSEPORT so the new server coexists with any previous one
        // while the old one continues to serve existing connections.
        if need_new_listener {
            let routes = Arc::clone(&self.routes);
            let cache = self.cache.clone();
            let ports = self.ports.clone();
            let tls_ports = self.tls_ports.clone();
            let cert_store = Arc::clone(&self.cert_store);
            let metrics = self.metrics.clone();
            let challenge_store = self.challenge_store.clone();
            std::thread::spawn(move || {
                start_pingora_server(
                    ports,
                    tls_ports,
                    routes,
                    cache,
                    cert_store,
                    metrics,
                    challenge_store,
                );
            });
        }

        info!(
            name,
            port,
            host = ?host,
            backend = ?backend,
            tls = ssl.is_some(),
            "route added"
        );
        Ok(())
    }

    pub async fn remove_routes(&mut self, name: &str) {
        let mut table = self.routes.write().await;
        table.entries.retain(|r| r.name != name);
        info!(name, "routes removed");
    }
}

fn start_pingora_server(
    ports: Vec<u16>,
    tls_ports: HashSet<u16>,
    routes: Arc<RwLock<RouteTable>>,
    cache: Cache<String, (ResponseHeader, Bytes)>,
    cert_store: Arc<DynamicCertStore>,
    metrics: Option<Metrics>,
    challenge_store: ChallengeStore,
) {
    let mut server = match Server::new(None) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "failed to create pingora server");
            return;
        }
    };

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    server.configuration = Arc::new(ServerConf {
        daemon: false,
        grace_period_seconds: Some(0),
        graceful_shutdown_timeout_seconds: Some(0),
        threads,
        work_stealing: true,
        ..Default::default()
    });
    server.bootstrap();

    let proxy = NyrunProxy {
        routes,
        cache,
        metrics,
        challenge_store,
    };
    let mut service = http_proxy_service(&server.configuration, proxy);

    for port in &ports {
        let addr = format!("0.0.0.0:{}", port);
        if tls_ports.contains(port) {
            match cert_store.to_tls_settings() {
                Ok(settings) => {
                    service.add_tls_with_settings(&addr, None, settings);
                    info!(port, "TLS listener added");
                }
                Err(e) => {
                    error!(port, error = %e, "failed to create TLS settings, falling back to TCP");
                    service.add_tcp(&addr);
                }
            }
        } else {
            service.add_tcp(&addr);
        }
    }

    server.add_service(service);

    info!(ports = ?ports, tls_ports = ?tls_ports, "pingora proxy started");
    server.run_forever();
}
