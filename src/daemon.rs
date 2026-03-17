use crate::acme::ChallengeStore;
use crate::metrics::{self, Metrics};
use crate::process::ProcessManager;
use crate::proxy::ProxyManager;
use crate::server::DaemonState;
use crate::state::{self, StateStore};
use prometheus_client::registry::Registry;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

const NYRUN_DIR: &str = "/var/run/nyrun";
const PID_PATH: &str = "/var/run/nyrun/nyrun.pid";
const SOCK_PATH: &str = "/var/run/nyrun/nyrun.sock";

pub fn ensure_dirs() {
    let dirs = ["", "logs", "certs", "oci", "state"];
    for d in dirs {
        let path = if d.is_empty() {
            NYRUN_DIR.to_string()
        } else {
            format!("{NYRUN_DIR}/{d}")
        };
        let _ = std::fs::create_dir_all(&path);
    }
}

pub fn write_pid() {
    let pid = std::process::id();
    let _ = std::fs::write(PID_PATH, pid.to_string());
}

pub fn is_daemon_running() -> bool {
    if !Path::new(SOCK_PATH).exists() {
        return false;
    }
    std::os::unix::net::UnixStream::connect(SOCK_PATH).is_ok()
}

pub fn spawn_daemon() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own executable: {e}"))?;

    let _child = std::process::Command::new(exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn daemon: {e}"))?;

    std::thread::sleep(std::time::Duration::from_millis(500));

    if is_daemon_running() {
        Ok(())
    } else {
        Err("daemon failed to start".to_string())
    }
}

pub fn ensure_daemon() -> Result<(), String> {
    if is_daemon_running() {
        return Ok(());
    }
    eprintln!("[nyrun] starting daemon...");
    spawn_daemon()
}

const METRICS_PORT: u16 = 9090;

pub async fn run_daemon() {
    ensure_dirs();
    write_pid();

    let mut registry = Registry::default();
    let metrics = Metrics::new_registered(&mut registry);
    let registry = Arc::new(registry);

    // Start metrics server
    let registry_clone = Arc::clone(&registry);
    tokio::spawn(async move {
        metrics::serve_metrics(METRICS_PORT, registry_clone).await;
    });

    let challenge_store = ChallengeStore::default();
    let acme_configs = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let proxy_mgr = ProxyManager::new(Some(metrics.clone()), challenge_store);

    // Start ACME renewal loop
    {
        let cert_store = Arc::clone(proxy_mgr.cert_store());
        let challenge_store = proxy_mgr.challenge_store().clone();
        let acme_configs = Arc::clone(&acme_configs);
        tokio::spawn(crate::acme::renewal_loop(
            cert_store,
            challenge_store,
            acme_configs,
        ));
    }

    // Open SlateDB state store
    let state_store = match StateStore::open().await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to open SlateDB state store");
            std::process::exit(1);
        }
    };

    // Migrate old JSON state if present
    state::migrate_json_to_slatedb(&state_store).await;

    // Restore saved processes
    let saved = state_store.load().await;
    if !saved.is_empty() {
        info!(count = saved.len(), "restoring saved processes");
    }

    let daemon_state = Arc::new(Mutex::new(DaemonState {
        process_mgr: ProcessManager::new(Some(metrics.clone())),
        proxy_mgr,
        acme_configs,
        state_store,
        cloud_shutdown: None,
    }));

    if !saved.is_empty() {
        daemon_state
            .lock()
            .await
            .process_mgr
            .restore_processes(saved)
            .await;
    }

    // Restore cloud agent connection if configured
    {
        let st = daemon_state.lock().await;
        if let Some((api_key, url)) = crate::cloud::load_cloud_config(&st.state_store).await {
            info!("restoring cloud agent connection");
            let agent = crate::cloud::CloudAgent::new(api_key, url);
            let shutdown_handle = agent.shutdown_handle();
            let state_clone = Arc::clone(&daemon_state);
            tokio::spawn(agent.run(state_clone));
            drop(st);
            daemon_state.lock().await.cloud_shutdown = Some(shutdown_handle);
        }
    }

    info!("daemon started (pid: {})", std::process::id());

    crate::server::run_server(daemon_state).await;
}

pub fn cleanup() {
    let _ = std::fs::remove_file(SOCK_PATH);
    let _ = std::fs::remove_file(PID_PATH);
}
