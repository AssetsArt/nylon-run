use crate::process::ProcessManager;
use crate::state;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

const NYRUN_DIR: &str = "/tmp/nyrun";
const PID_PATH: &str = "/tmp/nyrun/nyrun.pid";
const SOCK_PATH: &str = "/tmp/nyrun/nyrun.sock";

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
    // Check if socket exists and is connectable
    if !Path::new(SOCK_PATH).exists() {
        return false;
    }
    // Try connecting synchronously
    std::os::unix::net::UnixStream::connect(SOCK_PATH).is_ok()
}

/// Spawn daemon as a detached child process
pub fn spawn_daemon() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own executable: {e}"))?;

    let _child = std::process::Command::new(exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn daemon: {e}"))?;

    // Wait briefly for daemon to start
    std::thread::sleep(std::time::Duration::from_millis(500));

    if is_daemon_running() {
        Ok(())
    } else {
        Err("daemon failed to start".to_string())
    }
}

/// Ensure daemon is running, spawn if needed
pub fn ensure_daemon() -> Result<(), String> {
    if is_daemon_running() {
        return Ok(());
    }
    eprintln!("[nyrun] starting daemon...");
    spawn_daemon()
}

/// Run the daemon main loop (called when invoked with hidden `daemon` subcommand)
pub async fn run_daemon() {
    ensure_dirs();
    write_pid();

    // Set up signal handler for graceful shutdown
    let manager = Arc::new(Mutex::new(ProcessManager::new()));

    // Restore saved processes
    let saved = state::load_state();
    if !saved.is_empty() {
        info!(count = saved.len(), "restoring saved processes");
        manager.lock().await.restore_processes(saved).await;
    }

    info!("daemon started (pid: {})", std::process::id());

    // Run Unix socket server
    crate::server::run_server(manager).await;
}

pub fn cleanup() {
    let _ = std::fs::remove_file(SOCK_PATH);
    let _ = std::fs::remove_file(PID_PATH);
}
