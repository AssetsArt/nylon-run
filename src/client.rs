use crate::protocol::{self, ProcessInfo, Request, Response};
use tokio::net::UnixStream;

const SOCK_PATH: &str = "/var/run/nyrun/nyrun.sock";

/// Send a request to the daemon via Unix socket (for use within daemon process, e.g. cloud agent).
pub async fn send_request_local(request: Request) -> Result<Response, String> {
    let mut stream = UnixStream::connect(SOCK_PATH)
        .await
        .map_err(|e| format!("failed to connect to daemon: {e}"))?;

    protocol::write_message(&mut stream, &request)
        .await
        .map_err(|e| format!("failed to send request: {e}"))?;

    protocol::read_message(&mut stream)
        .await
        .map_err(|e| format!("failed to read response: {e}"))
}

pub async fn send_request(request: Request) -> Result<Response, String> {
    crate::daemon::ensure_daemon()?;

    let mut stream = UnixStream::connect(SOCK_PATH)
        .await
        .map_err(|e| format!("failed to connect to daemon: {e}"))?;

    protocol::write_message(&mut stream, &request)
        .await
        .map_err(|e| format!("failed to send request: {e}"))?;

    protocol::read_message(&mut stream)
        .await
        .map_err(|e| format!("failed to read response: {e}"))
}

fn print_response(response: Response) {
    match response {
        Response::Ok(msg) => println!("{msg}"),
        Response::ProcessList(procs) => print_process_list(&procs),
        Response::ConfigList(_) => {} // handled directly by caller
        Response::Logs(logs) => print!("{logs}"),
        Response::Error(e) => eprintln!("error: {e}"),
    }
}

fn print_process_list(procs: &[ProcessInfo]) {
    if procs.is_empty() {
        println!("no processes running");
        return;
    }

    println!(
        "{:<20} {:<8} {:<10} {:<8} {:<15} {:<10}",
        "NAME", "PID", "STATUS", "MODE", "PORT", "UPTIME"
    );
    println!("{}", "-".repeat(75));

    for p in procs {
        let pid = p.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let status = format!("{:?}", p.status);
        let mode = format!("{:?}", p.mode);
        let port = p
            .port_mapping
            .as_ref()
            .map(|pm| {
                if let Some(ref host) = pm.host {
                    format!(
                        "{}:{}:{}",
                        host,
                        pm.public_port,
                        pm.app_port.unwrap_or(pm.public_port)
                    )
                } else if let Some(app) = pm.app_port {
                    format!("{}:{}", pm.public_port, app)
                } else {
                    pm.public_port.to_string()
                }
            })
            .unwrap_or_else(|| "-".into());

        let uptime = p
            .uptime_secs
            .map(format_uptime)
            .unwrap_or_else(|| "-".into());

        println!(
            "{:<20} {:<8} {:<10} {:<8} {:<15} {:<10}",
            p.name, pid, status, mode, port, uptime
        );
    }
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    }
}

pub async fn execute(request: Request) {
    match send_request(request).await {
        Ok(response) => print_response(response),
        Err(e) => eprintln!("error: {e}"),
    }
}
