use crate::metrics::Metrics;
use crate::protocol::{PortMapping, ProcessConfig, ProcessInfo, ProcessStatus, SslConfig};
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use sysinfo::{Pid, System};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

const NYRUN_DIR: &str = "/var/run/nyrun";
const LOG_MAX_SIZE: u64 = 10 * 1024 * 1024; // 10MB
const LOG_MAX_ROTATED: usize = 5;

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
    metrics: Option<Metrics>,
    sys: System,
}

impl ProcessManager {
    pub fn new(metrics: Option<Metrics>) -> Self {
        Self {
            processes: HashMap::new(),
            metrics,
            sys: System::new(),
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

        // Write PID file if configured
        if let Some(ref pid_path) = managed.config.pid_file {
            if let Some(p) = pid {
                if let Err(e) = std::fs::write(pid_path, p.to_string()) {
                    warn!(path = %pid_path, error = %e, "failed to write pid file");
                }
            }
        }

        self.processes.insert(name.clone(), managed);
        if let Some(m) = &self.metrics {
            m.managed_processes.inc();
        }
        Ok(format!(
            "process '{}' started (pid: {})",
            name,
            pid.unwrap_or(0)
        ))
    }

    async fn start_process(&mut self, config: ProcessConfig) -> Result<ManagedProcess, String> {
        let mut config = config;

        // OCI: pull and extract if needed
        if config.is_oci {
            let reference = config.oci_reference.as_deref().unwrap_or(&config.path);
            let extract_dir = match crate::oci::pull_and_extract(reference, &config.name).await {
                Ok(dir) => {
                    if let Some(m) = &self.metrics {
                        m.oci_pulls_total.inc();
                    }
                    dir
                }
                Err(e) => {
                    if let Some(m) = &self.metrics {
                        m.oci_pull_errors_total.inc();
                    }
                    return Err(e);
                }
            };

            let (entrypoint, extra_args) = crate::oci::find_entrypoint(&extract_dir)?;
            config.path = entrypoint;

            // Prepend image entrypoint args before user args
            if !extra_args.is_empty() {
                let mut merged = extra_args;
                merged.extend(config.args.clone());
                config.args = merged;
            }

            // OCI processes are isolated by default (deny io to their own folder)
            // unless --allow all is specified
            if !config.allow.iter().any(|a| a == "all") && !config.deny.contains(&"io".to_string())
            {
                config.deny.push("io".to_string());
                config.allow.push(extract_dir.to_string_lossy().to_string());
            }

            // If --allow all, clear deny io so sandbox doesn't restrict
            if config.allow.iter().any(|a| a == "all") {
                config.deny.retain(|d| d != "io");
                config.allow.retain(|a| a != "all");
            }

            info!(
                name = %config.name,
                binary = %config.path,
                "OCI image ready"
            );
        }

        let mut cmd = Command::new(&config.path);
        cmd.args(&config.args);

        for (k, v) in &config.env_vars {
            cmd.env(k, v);
        }

        // Capture stdout/stderr
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Apply sandbox restrictions in child process before exec
        if !config.deny.is_empty() {
            let deny = config.deny.clone();
            let allow = config.allow.clone();
            let working_dir = std::path::Path::new(&config.path)
                .parent()
                .unwrap_or(std::path::Path::new("/"))
                .to_string_lossy()
                .to_string();

            // For OCI processes, use the OCI extraction dir as working dir
            let working_dir = if config.is_oci {
                format!("{}/{}", "/var/run/nyrun/oci", config.name)
            } else {
                working_dir
            };

            unsafe {
                cmd.pre_exec(move || {
                    crate::sandbox::apply_sandbox(&deny, &allow, &working_dir)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))
                });
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn '{}': {e}", config.path))?;
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

    /// Register a SPA entry (no child process, just tracked for ls/del)
    pub async fn register_spa(&mut self, config: ProcessConfig) -> Result<String, String> {
        if self.processes.contains_key(&config.name) {
            return Err(format!("process '{}' already exists", config.name));
        }
        let name = config.name.clone();
        self.processes.insert(
            name.clone(),
            ManagedProcess {
                config,
                child: None,
                pid: None,
                status: ProcessStatus::Running,
                started_at: Some(Utc::now()),
                restart_count: 0,
                shutdown_tx: None,
            },
        );
        if let Some(m) = &self.metrics {
            m.managed_processes.inc();
        }
        Ok(format!("SPA '{}' registered", name))
    }

    pub fn list(&self) -> Vec<ProcessInfo> {
        self.processes
            .values()
            .map(|p| {
                let uptime = p
                    .started_at
                    .map(|s| Utc::now().signed_duration_since(s).num_seconds().max(0) as u64);
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

        // Clean up PID file
        if let Some(ref pid_path) = proc.config.pid_file {
            let _ = std::fs::remove_file(pid_path);
        }

        kill_child(proc).await;
        self.processes.remove(name);
        if let Some(m) = &self.metrics {
            m.managed_processes.dec();
        }
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

        // Update PID file on restart
        if let Some(ref pid_path) = managed.config.pid_file {
            if let Some(p) = pid {
                let _ = std::fs::write(pid_path, p.to_string());
            }
        }

        self.processes.insert(name.to_string(), managed);
        if let Some(m) = &self.metrics {
            m.process_restarts_total
                .get_or_create(&crate::metrics::ProcessLabels {
                    name: name.to_string(),
                })
                .inc();
        }
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
        if let Some(m) = &self.metrics {
            m.managed_processes.set(0);
        }
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

    /// Update a process config in-place. Returns the old config for diffing.
    #[allow(clippy::too_many_arguments)]
    pub fn update_config(
        &mut self,
        name: &str,
        port_mapping: Option<PortMapping>,
        ssl: Option<SslConfig>,
        acme: Option<String>,
        env_file: Option<String>,
        env_vars: Option<HashMap<String, String>>,
        args: Option<Vec<String>>,
    ) -> Result<ProcessConfig, String> {
        let proc = self
            .processes
            .get_mut(name)
            .ok_or_else(|| format!("process '{}' not found", name))?;

        let old_config = proc.config.clone();

        if let Some(pm) = port_mapping {
            proc.config.port_mapping = Some(pm);
        }
        if let Some(s) = ssl {
            proc.config.ssl = Some(s);
        }
        if let Some(a) = acme {
            proc.config.acme = Some(a);
        }
        if let Some(ef) = &env_file {
            proc.config.env_file = Some(ef.clone());
        }
        if let Some(ev) = env_vars {
            proc.config.env_vars = ev;
        }
        if let Some(a) = args {
            proc.config.args = a;
        }

        Ok(old_config)
    }

    pub fn metrics(&self) -> Option<&Metrics> {
        self.metrics.as_ref()
    }

    /// Update OCI-specific fields after a new image has been pulled.
    pub fn update_oci_config(&mut self, name: &str, oci_reference: &str, entrypoint: &str) {
        if let Some(proc) = self.processes.get_mut(name) {
            proc.config.oci_reference = Some(oci_reference.to_string());
            proc.config.path = entrypoint.to_string();
            proc.config.is_oci = true;
        }
    }

    /// Rotate logs for all processes if they exceed the size limit
    pub fn rotate_logs(&self) {
        for name in self.processes.keys() {
            let log_dir = Self::logs_dir(name);
            for log_name in &["stdout.log", "stderr.log"] {
                let log_path = log_dir.join(log_name);
                if let Ok(meta) = std::fs::metadata(&log_path)
                    && meta.len() > LOG_MAX_SIZE
                {
                    rotate_log_file(&log_path);
                }
            }
        }
    }

    /// Collect CPU and memory stats for all running processes
    pub fn collect_process_stats(&mut self) {
        let metrics = match &self.metrics {
            Some(m) => m,
            None => return,
        };

        let pids: Vec<(String, Pid)> = self
            .processes
            .iter()
            .filter_map(|(name, proc)| proc.pid.map(|pid| (name.clone(), Pid::from_u32(pid))))
            .collect();

        if pids.is_empty() {
            return;
        }

        self.sys.refresh_processes(
            sysinfo::ProcessesToUpdate::Some(&pids.iter().map(|(_, pid)| *pid).collect::<Vec<_>>()),
            true,
        );

        for (name, pid) in &pids {
            if let Some(proc) = self.sys.process(*pid) {
                let labels = crate::metrics::ProcessLabels { name: name.clone() };
                metrics
                    .process_cpu_usage
                    .get_or_create(&labels)
                    .set(proc.cpu_usage() as i64);
                metrics
                    .process_memory_bytes
                    .get_or_create(&labels)
                    .set(proc.memory() as i64);
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

fn rotate_log_file(path: &PathBuf) {
    // Remove oldest rotated file
    let oldest = format!("{}.{}", path.display(), LOG_MAX_ROTATED);
    let _ = std::fs::remove_file(&oldest);

    // Shift existing rotated files: .4 -> .5, .3 -> .4, etc.
    for i in (1..LOG_MAX_ROTATED).rev() {
        let from = format!("{}.{}", path.display(), i);
        let to = format!("{}.{}", path.display(), i + 1);
        let _ = std::fs::rename(&from, &to);
    }

    // Move current log to .1
    let rotated = format!("{}.1", path.display());
    let _ = std::fs::rename(path, &rotated);

    // Create fresh empty log file
    let _ = std::fs::File::create(path);

    info!(path = %path.display(), "log rotated");
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
