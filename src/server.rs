use crate::process::ProcessManager;
use crate::protocol::{self, Request, Response};
use crate::state;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{error, info};

const SOCK_PATH: &str = "/tmp/nyrun/nyrun.sock";

pub async fn run_server(manager: Arc<Mutex<ProcessManager>>) {
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
    let mgr_clone = Arc::clone(&manager);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            mgr_clone.lock().await.check_and_restart().await;
        }
    });

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let mgr = Arc::clone(&manager);
                tokio::spawn(handle_client(stream, mgr));
            }
            Err(e) => {
                error!(error = %e, "failed to accept connection");
            }
        }
    }
}

async fn handle_client(mut stream: UnixStream, manager: Arc<Mutex<ProcessManager>>) {
    let request: Request = match protocol::read_message(&mut stream).await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "failed to read request");
            return;
        }
    };

    let response = handle_request(request, &manager).await;

    if let Err(e) = protocol::write_message(&mut stream, &response).await {
        error!(error = %e, "failed to write response");
    }
}

async fn handle_request(request: Request, manager: &Arc<Mutex<ProcessManager>>) -> Response {
    match request {
        Request::Bin { config } | Request::Run { config } => {
            let mut mgr = manager.lock().await;
            match mgr.spawn_process(config).await {
                Ok(msg) => Response::Ok(msg),
                Err(e) => Response::Error(e),
            }
        }
        Request::Ls => {
            let mgr = manager.lock().await;
            Response::ProcessList(mgr.list())
        }
        Request::Del { name } => {
            let mut mgr = manager.lock().await;
            match mgr.delete(&name).await {
                Ok(msg) => Response::Ok(msg),
                Err(e) => Response::Error(e),
            }
        }
        Request::Restart { name } => {
            let mut mgr = manager.lock().await;
            match mgr.restart(&name).await {
                Ok(msg) => Response::Ok(msg),
                Err(e) => Response::Error(e),
            }
        }
        Request::Reload { name } => {
            let mut mgr = manager.lock().await;
            match mgr.reload(&name).await {
                Ok(msg) => Response::Ok(msg),
                Err(e) => Response::Error(e),
            }
        }
        Request::Update { name, .. } => {
            // TODO: implement config update
            Response::Error(format!("update not yet implemented for '{}'", name))
        }
        Request::Logs { name, lines } => {
            let mgr = manager.lock().await;
            match mgr.get_logs(&name, lines) {
                Ok(logs) => Response::Logs(logs),
                Err(e) => Response::Error(e),
            }
        }
        Request::Save => {
            let mgr = manager.lock().await;
            let configs = mgr.get_configs();
            match state::save_state(&configs) {
                Ok(()) => Response::Ok(format!("saved {} processes", configs.len())),
                Err(e) => Response::Error(e),
            }
        }
        Request::Kill => {
            let mut mgr = manager.lock().await;
            let msg = mgr.kill_all().await;
            // Save empty state
            let _ = state::save_state(&[]);
            Response::Ok(msg)
        }
    }
}
