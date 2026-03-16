use crate::process::ProcessManager;
use crate::protocol::{self, ProcessConfig, Request, Response};
use crate::proxy::{Backend, ProxyManager};
use crate::state;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{error, info};

const SOCK_PATH: &str = "/tmp/nyrun/nyrun.sock";

pub struct DaemonState {
    pub process_mgr: ProcessManager,
    pub proxy_mgr: ProxyManager,
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
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            state_clone.lock().await.process_mgr.check_and_restart().await;
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
        Request::Update { name, .. } => {
            Response::Error(format!("update not yet implemented for '{}'", name))
        }
        Request::Logs { name, lines } => {
            let st = state.lock().await;
            match st.process_mgr.get_logs(&name, lines) {
                Ok(logs) => Response::Logs(logs),
                Err(e) => Response::Error(e),
            }
        }
        Request::Save => {
            let st = state.lock().await;
            let configs = st.process_mgr.get_configs();
            match state::save_state(&configs) {
                Ok(()) => Response::Ok(format!("saved {} processes", configs.len())),
                Err(e) => Response::Error(e),
            }
        }
        Request::Kill => {
            let mut st = state.lock().await;
            let msg = st.process_mgr.kill_all().await;
            let _ = state::save_state(&[]);
            Response::Ok(msg)
        }
    }
}

async fn handle_run(state: &Arc<Mutex<DaemonState>>, config: ProcessConfig) -> Response {
    let name = config.name.clone();
    let is_spa = config.spa;
    let port_mapping = config.port_mapping.clone();

    let pm = match &port_mapping {
        Some(pm) => pm.clone(),
        None => return Response::Error("run requires --p port mapping".to_string()),
    };

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
            .add_route(&name, pm.public_port, pm.host.clone(), backend)
            .await
        {
            return Response::Error(e);
        }

        // Register as a SPA "process" in the process manager for tracking
        if let Err(e) = st.process_mgr.register_spa(config).await {
            return Response::Error(e);
        }

        Response::Ok(format!(
            "SPA '{}' serving on port {}",
            name, pm.public_port
        ))
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
                    .add_route(&name, pm.public_port, pm.host.clone(), backend)
                    .await
                {
                    // Rollback: kill the process
                    let _ = st.process_mgr.delete(&name).await;
                    return Response::Error(format!("proxy setup failed: {e}"));
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
