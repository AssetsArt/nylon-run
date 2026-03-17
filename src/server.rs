use crate::process::ProcessManager;
use crate::protocol::{self, PortMapping, ProcessConfig, Request, Response, SslConfig};
use crate::proxy::{Backend, ProxyManager};
use crate::state::StateStore;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{error, info};

const SOCK_PATH: &str = "/var/run/nyrun/nyrun.sock";

pub struct DaemonState {
    pub process_mgr: ProcessManager,
    pub proxy_mgr: ProxyManager,
    pub acme_configs: Arc<tokio::sync::RwLock<Vec<(String, String)>>>,
    pub state_store: StateStore,
}

pub async fn run_server(state: Arc<Mutex<DaemonState>>) {
    // Clean up stale socket
    let _ = std::fs::remove_file(SOCK_PATH);

    let listener = match UnixListener::bind(SOCK_PATH) {
        Ok(l) => l,
        Err(e) => {
            error!(error = %e, "failed to bind unix socket");
            return;
        }
    };

    info!(path = SOCK_PATH, "daemon listening");

    // Spawn process health checker
    let state_clone = Arc::clone(&state);
    tokio::spawn(async move {
        let mut tick = 0u64;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let mut st = state_clone.lock().await;
            st.process_mgr.check_and_restart().await;
            st.process_mgr.collect_process_stats();
            // Rotate logs every ~60s (30 ticks * 2s)
            tick += 1;
            if tick.is_multiple_of(30) {
                st.process_mgr.rotate_logs();
            }
        }
    });

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let st = Arc::clone(&state);
                tokio::spawn(handle_client(stream, st));
            }
            Err(e) => {
                error!(error = %e, "failed to accept connection");
            }
        }
    }
}

async fn handle_client(mut stream: UnixStream, state: Arc<Mutex<DaemonState>>) {
    let request: Request = match protocol::read_message(&mut stream).await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "failed to read request");
            return;
        }
    };

    let response = handle_request(request, &state).await;

    if let Err(e) = protocol::write_message(&mut stream, &response).await {
        error!(error = %e, "failed to write response");
    }
}

async fn handle_request(request: Request, state: &Arc<Mutex<DaemonState>>) -> Response {
    match request {
        Request::Bin { config } => {
            let mut st = state.lock().await;
            match st.process_mgr.spawn_process(config).await {
                Ok(msg) => Response::Ok(msg),
                Err(e) => Response::Error(e),
            }
        }
        Request::Run { config } => handle_run(state, config).await,
        Request::Ls => {
            let st = state.lock().await;
            Response::ProcessList(st.process_mgr.list())
        }
        Request::Del { name } => {
            let mut st = state.lock().await;
            st.proxy_mgr.remove_routes(&name).await;
            match st.process_mgr.delete(&name).await {
                Ok(msg) => Response::Ok(msg),
                Err(e) => {
                    // Even if process doesn't exist (SPA-only), that's OK
                    if e.contains("not found") {
                        Response::Ok(format!("'{}' removed", name))
                    } else {
                        Response::Error(e)
                    }
                }
            }
        }
        Request::Restart { name } => {
            let mut st = state.lock().await;
            match st.process_mgr.restart(&name).await {
                Ok(msg) => Response::Ok(msg),
                Err(e) => Response::Error(e),
            }
        }
        Request::Reload { name } => {
            let mut st = state.lock().await;
            match st.process_mgr.reload(&name).await {
                Ok(msg) => Response::Ok(msg),
                Err(e) => Response::Error(e),
            }
        }
        Request::Update {
            name,
            port,
            ssl,
            acme,
            env_file,
            args,
            image,
        } => handle_update(state, name, port, ssl, acme, env_file, args, image).await,
        Request::Logs { name, lines } => {
            let st = state.lock().await;
            match st.process_mgr.get_logs(&name, lines) {
                Ok(logs) => Response::Logs(logs),
                Err(e) => Response::Error(e),
            }
        }
        Request::Set { key, value } => {
            match key.as_str() {
                "cache-ttl" => match value.parse::<u64>() {
                    Ok(secs) => {
                        let mut st = state.lock().await;
                        st.proxy_mgr.set_cache_ttl(secs);
                        Response::Ok(format!("cache-ttl set to {}s", secs))
                    }
                    Err(_) => Response::Error(format!("invalid value '{}': expected seconds", value)),
                },
                _ => Response::Error(format!("unknown config key '{}'. available: cache-ttl", key)),
            }
        }
        Request::Export => {
            let st = state.lock().await;
            Response::ConfigList(st.process_mgr.get_configs())
        }
        Request::Save => {
            let st = state.lock().await;
            let configs = st.process_mgr.get_configs();
            match st.state_store.save(&configs).await {
                Ok(()) => Response::Ok(format!("saved {} processes", configs.len())),
                Err(e) => Response::Error(e),
            }
        }
        Request::Kill => {
            let mut st = state.lock().await;
            let msg = st.process_mgr.kill_all().await;
            let _ = st.state_store.save(&[]).await;
            st.state_store.close().await;
            Response::Ok(msg)
        }
    }
}

async fn handle_update(
    state: &Arc<Mutex<DaemonState>>,
    name: String,
    port: Option<String>,
    ssl: Option<Vec<String>>,
    acme: Option<String>,
    env_file: Option<String>,
    args: Option<String>,
    image: Option<String>,
) -> Response {
    // If a new OCI image is provided, pull and extract it before taking the lock
    let oci_update = if let Some(ref img_ref) = image {
        if !crate::oci::is_oci_reference(img_ref) {
            return Response::Error(format!("'{}' is not a valid OCI image reference", img_ref));
        }
        match crate::oci::pull_and_extract(img_ref, &name).await {
            Ok(extract_dir) => match crate::oci::find_entrypoint(&extract_dir) {
                Ok((entrypoint, extra_args)) => Some((img_ref.clone(), entrypoint, extra_args)),
                Err(e) => return Response::Error(format!("OCI entrypoint error: {e}")),
            },
            Err(e) => return Response::Error(format!("OCI pull failed: {e}")),
        }
    } else {
        None
    };

    // Parse port mapping if provided
    let port_mapping = match port.as_deref() {
        Some(p) => match parse_port_mapping_str(p) {
            Ok(pm) => Some(pm),
            Err(e) => return Response::Error(e),
        },
        None => None,
    };

    // Parse SSL config if provided
    let ssl_config = ssl.map(|s| SslConfig {
        cert_path: s.first().cloned().unwrap_or_default(),
        key_path: s.get(1).cloned().unwrap_or_default(),
    });

    // Parse env file if provided
    let env_vars = match &env_file {
        Some(ef) => match parse_env_file_str(ef) {
            Ok(v) => Some(v),
            Err(e) => return Response::Error(e),
        },
        None => None,
    };

    // Parse args if provided
    let parsed_args = args.as_ref().map(|a| shlex::split(a).unwrap_or_default());

    let mut st = state.lock().await;

    // Update the config in-place
    let old_config = match st.process_mgr.update_config(
        &name,
        port_mapping,
        ssl_config,
        acme,
        env_file,
        env_vars,
        parsed_args,
    ) {
        Ok(old) => old,
        Err(e) => return Response::Error(e),
    };

    // Apply OCI image update to config
    if let Some((img_ref, entrypoint, _extra_args)) = &oci_update {
        st.process_mgr.update_oci_config(&name, img_ref, entrypoint);
    }

    // Check if proxy route needs updating (port mapping changed)
    if port.is_some() {
        // Remove old routes and add new ones
        st.proxy_mgr.remove_routes(&name).await;

        // Get the updated config from process manager
        let configs = st.process_mgr.get_configs();
        if let Some(cfg) = configs.iter().find(|c| c.name == name)
            && let Some(pm) = &cfg.port_mapping
        {
            let backend = if cfg.spa {
                let dir = PathBuf::from(&cfg.path);
                Backend::Spa(dir.canonicalize().unwrap_or(dir))
            } else {
                let app_port = pm.app_port.unwrap_or(pm.public_port);
                let addr: SocketAddr = format!("127.0.0.1:{}", app_port).parse().unwrap();
                Backend::Proxy(addr)
            };

            if let Err(e) = st
                .proxy_mgr
                .add_route(&name, pm.public_port, pm.host.clone(), backend)
                .await
            {
                // Revert config on proxy failure
                let _ = st.process_mgr.update_config(
                    &name,
                    old_config.port_mapping,
                    old_config.ssl,
                    old_config.acme,
                    old_config.env_file,
                    Some(old_config.env_vars),
                    Some(old_config.args),
                );
                return Response::Error(format!("proxy route update failed: {e}"));
            }
        }
    }

    // Restart the process if it has a child (not SPA-only)
    let configs = st.process_mgr.get_configs();
    let is_spa = configs
        .iter()
        .find(|c| c.name == name)
        .is_some_and(|c| c.spa);

    if !is_spa {
        match st.process_mgr.restart(&name).await {
            Ok(msg) => Response::Ok(format!("updated and restarted: {msg}")),
            Err(e) => Response::Error(format!("config updated but restart failed: {e}")),
        }
    } else {
        Response::Ok(format!("'{}' config updated", name))
    }
}

fn parse_port_mapping_str(port: &str) -> Result<PortMapping, String> {
    let parts: Vec<&str> = port.split(':').collect();
    match parts.len() {
        1 => {
            let p: u16 = parts[0]
                .parse()
                .map_err(|_| format!("invalid port: {}", parts[0]))?;
            Ok(PortMapping {
                host: None,
                public_port: p,
                app_port: None,
            })
        }
        2 => {
            let public: u16 = parts[0]
                .parse()
                .map_err(|_| format!("invalid port: {}", parts[0]))?;
            let app: u16 = parts[1]
                .parse()
                .map_err(|_| format!("invalid port: {}", parts[1]))?;
            Ok(PortMapping {
                host: None,
                public_port: public,
                app_port: Some(app),
            })
        }
        3 => {
            let host = parts[0].to_string();
            let public: u16 = parts[1]
                .parse()
                .map_err(|_| format!("invalid port: {}", parts[1]))?;
            let app: u16 = parts[2]
                .parse()
                .map_err(|_| format!("invalid port: {}", parts[2]))?;
            Ok(PortMapping {
                host: Some(host),
                public_port: public,
                app_port: Some(app),
            })
        }
        _ => Err(format!("invalid port mapping: {port}")),
    }
}

fn parse_env_file_str(path: &str) -> Result<HashMap<String, String>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read env file '{}': {}", path, e))?;
    let mut vars = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_string();
            let value = value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            vars.insert(key, value);
        }
    }
    Ok(vars)
}

async fn handle_run(state: &Arc<Mutex<DaemonState>>, config: ProcessConfig) -> Response {
    let name = config.name.clone();
    let is_spa = config.spa;
    let port_mapping = config.port_mapping.clone();
    let acme_email = config.acme.clone();
    let ssl = config
        .ssl
        .as_ref()
        .map(|s| (s.cert_path.clone(), s.key_path.clone()));

    let pm = match &port_mapping {
        Some(pm) => pm.clone(),
        None => return Response::Error("run requires --p port mapping".to_string()),
    };

    // ACME requires a hostname in the port mapping
    if acme_email.is_some() && pm.host.is_none() {
        return Response::Error(
            "ACME requires host-based port mapping (e.g. --p domain.com:443:8000)".to_string(),
        );
    }

    if is_spa {
        // SPA mode: no process, just serve static files
        let dir = PathBuf::from(&config.path);
        if !dir.is_dir() {
            return Response::Error(format!("'{}' is not a directory", config.path));
        }

        let mut st = state.lock().await;
        let backend = Backend::Spa(dir.canonicalize().unwrap_or(dir));

        if let Err(e) = st
            .proxy_mgr
            .add_route_with_tls(&name, pm.public_port, pm.host.clone(), backend, ssl)
            .await
        {
            return Response::Error(e);
        }

        // Register as a SPA "process" in the process manager for tracking
        if let Err(e) = st.process_mgr.register_spa(config).await {
            return Response::Error(e);
        }

        // Handle ACME if requested
        if let Some(email) = &acme_email {
            let hostname = pm.host.as_ref().unwrap().clone();
            spawn_acme_issue(&st, hostname, email.clone());
        }

        Response::Ok(format!("SPA '{}' serving on port {}", name, pm.public_port))
    } else {
        // Process + proxy mode
        let app_port = pm.app_port.unwrap_or(pm.public_port);
        let backend_addr: SocketAddr = format!("127.0.0.1:{}", app_port).parse().unwrap();

        let mut st = state.lock().await;

        // Spawn the process first
        match st.process_mgr.spawn_process(config).await {
            Ok(msg) => {
                // Set up proxy route
                let backend = Backend::Proxy(backend_addr);
                if let Err(e) = st
                    .proxy_mgr
                    .add_route_with_tls(&name, pm.public_port, pm.host.clone(), backend, ssl)
                    .await
                {
                    // Rollback: kill the process
                    let _ = st.process_mgr.delete(&name).await;
                    return Response::Error(format!("proxy setup failed: {e}"));
                }

                // Handle ACME if requested
                if let Some(email) = &acme_email {
                    let hostname = pm.host.as_ref().unwrap().clone();
                    spawn_acme_issue(&st, hostname, email.clone());
                }

                let port_info = if let Some(ref host) = pm.host {
                    format!("{}:{} -> {}", host, pm.public_port, app_port)
                } else {
                    format!(":{} -> {}", pm.public_port, app_port)
                };
                Response::Ok(format!("{msg} (proxy {port_info})"))
            }
            Err(e) => Response::Error(e),
        }
    }
}

fn spawn_acme_issue(st: &DaemonState, hostname: String, email: String) {
    let challenge_store = st.proxy_mgr.challenge_store().clone();
    let cert_store = Arc::clone(st.proxy_mgr.cert_store());
    let acme_configs = Arc::clone(&st.acme_configs);
    tokio::spawn(async move {
        // Register for renewal
        {
            let mut configs = acme_configs.write().await;
            if !configs.iter().any(|(h, _)| h == &hostname) {
                configs.push((hostname.clone(), email.clone()));
            }
        }
        if let Err(e) =
            crate::acme::issue_cert(&email, &hostname, &challenge_store, &cert_store).await
        {
            tracing::error!(hostname = %hostname, error = %e, "ACME cert issuance failed");
        }
    });
}
