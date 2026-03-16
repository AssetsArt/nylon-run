use crate::protocol::{ProcessConfig, ProcessInfo, ProcessStatus};
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

const NYRUN_DIR: &str = "/tmp/nyrun";

struct ManagedProcess {
    config: ProcessConfig,
    child: Option<Child>,
    pid: Option<u32>,
    status: ProcessStatus,
    started_at: Option<chrono::DateTime<Utc>>,
    restart_count: u32,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

pub struct ProcessManager {
    processes: HashMap<String, ManagedProcess>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: HashMap::new(),
        }
    }

    fn logs_dir(name: &str) -> PathBuf {
        PathBuf::from(NYRUN_DIR).join("logs").join(name)
    }

    fn ensure_dirs(name: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(Self::logs_dir(name))?;
        Ok(())
    }

    pub async fn spawn_process(&mut self, config: ProcessConfig) -> Result<String, String> {
        if self.processes.contains_key(&config.name) {
            return Err(format!("process '{}' already exists", config.name));
        }

        Self::ensure_dirs(&config.name).map_err(|e| format!("failed to create dirs: {e}"))?;

        let name = config.name.clone();
        let managed = self.start_process(config).await?;
        let pid = managed.pid;
        self.processes.insert(name.clone(), managed);
        Ok(format!(
            "process '{}' started (pid: {})",
            name,
            pid.unwrap_or(0)
        ))
    }

    async fn start_process(&self, config: ProcessConfig) -> Result<ManagedProcess, String> {
        let mut cmd = Command::new(&config.path);
        cmd.args(&config.args);

        for (k, v) in &config.env_vars {
            cmd.env(k, v);
        }

        // Capture stdout/stderr
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn '{}': {e}", config.path))?;
        let pid = child.id();

        // Spawn log capture tasks
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<()>(1);

        if let Some(stdout) = child.stdout.take() {
            let log_path = Self::logs_dir(&config.name).join("stdout.log");
            tokio::spawn(capture_output(stdout, log_path));
        }
        if let Some(stderr) = child.stderr.take() {
            let log_path = Self::logs_dir(&config.name).join("stderr.log");
            tokio::spawn(capture_output(stderr, log_path));
        }

        info!(name = %config.name, pid = ?pid, "process started");

        Ok(ManagedProcess {
            config,
            child: Some(child),
            pid,
            status: ProcessStatus::Running,
            started_at: Some(Utc::now()),
            restart_count: 0,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    pub fn list(&self) -> Vec<ProcessInfo> {
        self.processes
            .values()
            .map(|p| {
                let uptime = p.started_at.map(|s| {
                    Utc::now().signed_duration_since(s).num_seconds().max(0) as u64
                });
                ProcessInfo {
                    name: p.config.name.clone(),
                    pid: p.pid,
                    status: p.status.clone(),
                    mode: p.config.mode.clone(),
                    path: p.config.path.clone(),
                    port_mapping: p.config.port_mapping.clone(),
                    started_at: p.started_at,
                    restart_count: p.restart_count,
                    uptime_secs: uptime,
                }
            })
            .collect()
    }

    pub async fn delete(&mut self, name: &str) -> Result<String, String> {
        let proc = self
            .processes
            .get_mut(name)
            .ok_or_else(|| format!("process '{}' not found", name))?;

        kill_child(proc).await;
        self.processes.remove(name);
        info!(name, "process deleted");
        Ok(format!("process '{}' deleted", name))
    }

    pub async fn restart(&mut self, name: &str) -> Result<String, String> {
        let proc = self
            .processes
            .get_mut(name)
            .ok_or_else(|| format!("process '{}' not found", name))?;

        kill_child(proc).await;

        let config = proc.config.clone();
        let restart_count = proc.restart_count + 1;

        let mut managed = self.start_process(config).await?;
        managed.restart_count = restart_count;
        let pid = managed.pid;

        self.processes.insert(name.to_string(), managed);
        info!(name, "process restarted");
        Ok(format!(
            "process '{}' restarted (pid: {}, restarts: {})",
            name,
            pid.unwrap_or(0),
            restart_count
        ))
    }

    pub async fn reload(&mut self, name: &str) -> Result<String, String> {
        // For now reload == restart; zero-downtime requires proxy support
        self.restart(name).await
    }

    pub async fn kill_all(&mut self) -> String {
        let names: Vec<String> = self.processes.keys().cloned().collect();
        for name in &names {
            if let Some(proc) = self.processes.get_mut(name) {
                kill_child(proc).await;
            }
        }
        self.processes.clear();
        info!("all processes killed");
        "all processes stopped".to_string()
    }

    pub fn get_logs(&self, name: &str, lines: usize) -> Result<String, String> {
        if !self.processes.contains_key(name) {
            return Err(format!("process '{}' not found", name));
        }

        let stdout_path = Self::logs_dir(name).join("stdout.log");
        let stderr_path = Self::logs_dir(name).join("stderr.log");

        let mut output = String::new();

        if let Ok(content) = std::fs::read_to_string(&stdout_path) {
            let stdout_lines: Vec<&str> = content.lines().collect();
            let start = stdout_lines.len().saturating_sub(lines);
            for line in &stdout_lines[start..] {
                output.push_str(line);
                output.push('\n');
            }
        }

        if let Ok(content) = std::fs::read_to_string(&stderr_path) {
            let stderr_lines: Vec<&str> = content.lines().collect();
            let start = stderr_lines.len().saturating_sub(lines);
            if !stderr_lines[start..].is_empty() {
                output.push_str("--- stderr ---\n");
                for line in &stderr_lines[start..] {
                    output.push_str(line);
                    output.push('\n');
                }
            }
        }

        if output.is_empty() {
            output = "(no logs yet)\n".to_string();
        }

        Ok(output)
    }

    pub fn get_configs(&self) -> Vec<ProcessConfig> {
        self.processes.values().map(|p| p.config.clone()).collect()
    }

    pub async fn restore_processes(&mut self, configs: Vec<ProcessConfig>) {
        for config in configs {
            let name = config.name.clone();
            match self.spawn_process(config).await {
                Ok(msg) => info!(%msg, "restored process"),
                Err(e) => error!(name = %name, error = %e, "failed to restore process"),
            }
        }
    }

    /// Check for crashed processes and auto-restart them
    pub async fn check_and_restart(&mut self) {
        let mut to_restart = Vec::new();

        for (name, proc) in &mut self.processes {
            if proc.status != ProcessStatus::Running {
                continue;
            }
            if let Some(ref mut child) = proc.child {
                match child.try_wait() {
                    Ok(Some(exit_status)) => {
                        warn!(
                            name = %name,
                            status = ?exit_status,
                            "process exited unexpectedly"
                        );
                        proc.status = ProcessStatus::Errored;
                        to_restart.push(name.clone());
                    }
                    Ok(None) => {} // still running
                    Err(e) => {
                        error!(name = %name, error = %e, "failed to check process status");
                    }
                }
            }
        }

        for name in to_restart {
            if let Err(e) = self.restart(&name).await {
                error!(name = %name, error = %e, "auto-restart failed");
            }
        }
    }
}

async fn kill_child(proc: &mut ManagedProcess) {
    if let Some(ref mut child) = proc.child {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    proc.child = None;
    proc.pid = None;
    proc.status = ProcessStatus::Stopped;
    proc.shutdown_tx = None;
}

async fn capture_output<R: tokio::io::AsyncRead + Unpin>(reader: R, log_path: PathBuf) {
    use tokio::fs::OpenOptions;
    use tokio::io::AsyncWriteExt;

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await;

    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            error!(path = %log_path.display(), error = %e, "failed to open log file");
            return;
        }
    };

    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let timestamped = format!("[{}] {}\n", Utc::now().format("%Y-%m-%d %H:%M:%S"), line);
        let _ = file.write_all(timestamped.as_bytes()).await;
    }
}
