mod cli;
mod client;
mod daemon;
mod process;
mod protocol;
mod proxy;
mod server;
mod state;

use clap::Parser;
use cli::{Cli, Command};
use protocol::{ProcessConfig, ProcessMode, PortMapping, Request, SslConfig};
use std::collections::HashMap;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn parse_env_file(path: &str) -> Result<HashMap<String, String>, String> {
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
            let value = value.trim().trim_matches('"').trim_matches('\'').to_string();
            vars.insert(key, value);
        }
    }
    Ok(vars)
}

fn parse_args_string(args: &Option<String>) -> Vec<String> {
    match args {
        Some(s) => shlex::split(s).unwrap_or_default(),
        None => Vec::new(),
    }
}

fn parse_deny(deny: &Option<String>) -> Vec<String> {
    match deny {
        Some(s) => s.split(',').map(|s| s.trim().to_string()).collect(),
        None => Vec::new(),
    }
}

fn parse_allow(allow: &Option<String>) -> Vec<String> {
    match allow {
        Some(s) => s.split(',').map(|s| s.trim().to_string()).collect(),
        None => Vec::new(),
    }
}

fn parse_port_mapping(port: &str) -> Result<PortMapping, String> {
    let parts: Vec<&str> = port.split(':').collect();
    match parts.len() {
        1 => {
            let p: u16 = parts[0].parse().map_err(|_| format!("invalid port: {}", parts[0]))?;
            Ok(PortMapping {
                host: None,
                public_port: p,
                app_port: None,
            })
        }
        2 => {
            let public: u16 = parts[0].parse().map_err(|_| format!("invalid port: {}", parts[0]))?;
            let app: u16 = parts[1].parse().map_err(|_| format!("invalid port: {}", parts[1]))?;
            Ok(PortMapping {
                host: None,
                public_port: public,
                app_port: Some(app),
            })
        }
        3 => {
            let host = parts[0].to_string();
            let public: u16 = parts[1].parse().map_err(|_| format!("invalid port: {}", parts[1]))?;
            let app: u16 = parts[2].parse().map_err(|_| format!("invalid port: {}", parts[2]))?;
            Ok(PortMapping {
                host: Some(host),
                public_port: public,
                app_port: Some(app),
            })
        }
        _ => Err(format!("invalid port mapping: {port}")),
    }
}

fn derive_name(path: &str, name: &Option<String>) -> String {
    if let Some(n) = name {
        return n.clone();
    }
    std::path::Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("unknown")
        .to_string()
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Daemon => {
            // Set up logging for daemon
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .with_target(false)
                .init();

            daemon::run_daemon().await;
        }

        Command::Bin {
            path,
            name,
            args,
            env_file,
            deny,
            allow,
        } => {
            let env_vars = match &env_file {
                Some(ef) => match parse_env_file(ef) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                },
                None => HashMap::new(),
            };

            let config = ProcessConfig {
                name: derive_name(&path, &name),
                path,
                args: parse_args_string(&args),
                env_vars,
                env_file,
                mode: ProcessMode::Bin,
                port_mapping: None,
                spa: false,
                ssl: None,
                acme: None,
                deny: parse_deny(&deny),
                allow: parse_allow(&allow),
            };

            client::execute(Request::Bin { config }).await;
        }

        Command::Run {
            path,
            name,
            port,
            args,
            env_file,
            spa,
            ssl,
            acme,
            deny,
            allow,
        } => {
            let port_mapping = match parse_port_mapping(&port) {
                Ok(pm) => pm,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            let env_vars = match &env_file {
                Some(ef) => match parse_env_file(ef) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                },
                None => HashMap::new(),
            };

            let ssl_config = ssl.map(|s| SslConfig {
                cert_path: s[0].clone(),
                key_path: s[1].clone(),
            });

            let config = ProcessConfig {
                name: derive_name(&path, &name),
                path,
                args: parse_args_string(&args),
                env_vars,
                env_file,
                mode: ProcessMode::Run,
                port_mapping: Some(port_mapping),
                spa,
                ssl: ssl_config,
                acme,
                deny: parse_deny(&deny),
                allow: parse_allow(&allow),
            };

            client::execute(Request::Run { config }).await;
        }

        Command::Ls => {
            client::execute(Request::Ls).await;
        }

        Command::Del { name } => {
            client::execute(Request::Del { name }).await;
        }

        Command::Restart { name } => {
            client::execute(Request::Restart { name }).await;
        }

        Command::Reload { name } => {
            client::execute(Request::Reload { name }).await;
        }

        Command::Update {
            name,
            port,
            ssl,
            acme,
            env_file,
            args,
        } => {
            client::execute(Request::Update {
                name,
                port,
                ssl,
                acme,
                env_file,
                args,
            })
            .await;
        }

        Command::Logs { name, lines } => {
            client::execute(Request::Logs { name, lines }).await;
        }

        Command::Save => {
            client::execute(Request::Save).await;
        }

        Command::Kill => {
            client::execute(Request::Kill).await;
            // Also clean up socket/pid files
            daemon::cleanup();
            println!("daemon stopped");
        }

        Command::Startup => {
            match generate_systemd_unit() {
                Ok(()) => println!("systemd unit installed and enabled"),
                Err(e) => eprintln!("error: {e}"),
            }
        }

        Command::Unstartup => {
            match remove_systemd_unit() {
                Ok(()) => println!("systemd unit removed"),
                Err(e) => eprintln!("error: {e}"),
            }
        }

        Command::Backup { o } => {
            println!("backup not yet implemented (output: {o})");
        }

        Command::Restore { file } => {
            println!("restore not yet implemented (file: {file})");
        }

        Command::Link { api_key } => {
            println!("link not yet implemented (key: {api_key})");
        }

        Command::Unlink => {
            println!("unlink not yet implemented");
        }
    }
}

fn generate_systemd_unit() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find executable: {e}"))?
        .to_string_lossy()
        .to_string();

    let unit = format!(
        r#"[Unit]
Description=nyrun process manager
After=network.target

[Service]
Type=forking
ExecStart={exe} daemon
PIDFile=/tmp/nyrun/nyrun.pid
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
"#
    );

    let unit_path = "/etc/systemd/system/nyrun.service";
    std::fs::write(unit_path, unit)
        .map_err(|e| format!("failed to write {unit_path}: {e} (try with sudo)"))?;

    std::process::Command::new("systemctl")
        .args(["daemon-reload"])
        .status()
        .map_err(|e| format!("systemctl daemon-reload failed: {e}"))?;

    std::process::Command::new("systemctl")
        .args(["enable", "nyrun"])
        .status()
        .map_err(|e| format!("systemctl enable failed: {e}"))?;

    Ok(())
}

fn remove_systemd_unit() -> Result<(), String> {
    std::process::Command::new("systemctl")
        .args(["disable", "nyrun"])
        .status()
        .map_err(|e| format!("systemctl disable failed: {e}"))?;

    let unit_path = "/etc/systemd/system/nyrun.service";
    std::fs::remove_file(unit_path)
        .map_err(|e| format!("failed to remove {unit_path}: {e}"))?;

    std::process::Command::new("systemctl")
        .args(["daemon-reload"])
        .status()
        .map_err(|e| format!("systemctl daemon-reload failed: {e}"))?;

    Ok(())
}
