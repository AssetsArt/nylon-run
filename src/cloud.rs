use crate::protocol::{ProcessInfo, Request, Response};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, sleep};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

// --- Messages exchanged with cloud ---

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CloudMessage {
    /// Agent → Cloud: authentication
    #[serde(rename = "auth")]
    Auth { api_key: String },
    /// Agent → Cloud: heartbeat
    #[serde(rename = "heartbeat")]
    Heartbeat,
    /// Agent → Cloud: process list status
    #[serde(rename = "status")]
    Status { processes: Vec<ProcessInfo> },
    /// Agent → Cloud: logs push
    #[serde(rename = "logs")]
    Logs { name: String, data: String },
    /// Cloud → Agent: command
    #[serde(rename = "command")]
    Command { action: CloudAction },
    /// Cloud → Agent: auth accepted
    #[serde(rename = "auth_ok")]
    AuthOk,
    /// Cloud → Agent: error
    #[serde(rename = "error")]
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum CloudAction {
    #[serde(rename = "restart")]
    Restart { name: String },
    #[serde(rename = "reload")]
    Reload { name: String },
    #[serde(rename = "del")]
    Del { name: String },
    #[serde(rename = "update")]
    Update {
        name: String,
        port: Option<String>,
        ssl: Option<Vec<String>>,
        acme: Option<String>,
        env_file: Option<String>,
        args: Option<String>,
        image: Option<String>,
    },
}

// --- Cloud Agent ---

pub struct CloudAgent {
    api_key: String,
    server_url: String,
    shutdown_tx: mpsc::Sender<()>,
    shutdown_rx: Option<mpsc::Receiver<()>>,
}

impl CloudAgent {
    pub fn new(api_key: String, server_url: String) -> Self {
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        Self {
            api_key,
            server_url,
            shutdown_tx,
            shutdown_rx: Some(shutdown_rx),
        }
    }

    pub fn shutdown_handle(&self) -> mpsc::Sender<()> {
        self.shutdown_tx.clone()
    }

    /// Start the cloud agent loop. This takes ownership and runs until shutdown.
    pub async fn run(mut self, daemon_state: Arc<Mutex<crate::server::DaemonState>>) {
        let mut shutdown_rx = self.shutdown_rx.take().unwrap();
        let mut backoff = INITIAL_BACKOFF;

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("cloud agent shutting down");
                    return;
                }
                result = self.connect_and_run(&daemon_state) => {
                    match result {
                        Ok(()) => {
                            info!("cloud connection closed normally");
                            backoff = INITIAL_BACKOFF;
                        }
                        Err(e) => {
                            warn!(error = %e, backoff_secs = backoff.as_secs(), "cloud connection failed, reconnecting");
                        }
                    }
                }
            }

            // Wait with backoff before reconnecting
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("cloud agent shutting down during backoff");
                    return;
                }
                _ = sleep(backoff) => {}
            }

            // Exponential backoff with cap
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    async fn connect_and_run(
        &self,
        daemon_state: &Arc<Mutex<crate::server::DaemonState>>,
    ) -> Result<(), String> {
        info!(url = %self.server_url, "connecting to cloud");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.server_url)
            .await
            .map_err(|e| format!("WebSocket connect failed: {e}"))?;

        let (mut write, mut read) = ws_stream.split();

        // Authenticate
        let auth_msg = CloudMessage::Auth {
            api_key: self.api_key.clone(),
        };
        let json = serde_json::to_string(&auth_msg).unwrap();
        write
            .send(Message::Text(json.into()))
            .await
            .map_err(|e| format!("failed to send auth: {e}"))?;

        // Wait for auth response
        match read.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<CloudMessage>(&text) {
                Ok(CloudMessage::AuthOk) => {
                    info!("cloud authentication successful");
                }
                Ok(CloudMessage::Error { message }) => {
                    return Err(format!("cloud auth rejected: {message}"));
                }
                _ => {
                    return Err(format!("unexpected auth response: {text}"));
                }
            },
            Some(Ok(msg)) => {
                return Err(format!("unexpected message type during auth: {msg:?}"));
            }
            Some(Err(e)) => {
                return Err(format!("WebSocket error during auth: {e}"));
            }
            None => {
                return Err("connection closed during auth".to_string());
            }
        }

        info!("cloud agent connected");

        // Main loop: heartbeat, status push, and receive commands
        let mut heartbeat_interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        let mut status_interval = tokio::time::interval(Duration::from_secs(10));

        loop {
            tokio::select! {
                // Send heartbeat
                _ = heartbeat_interval.tick() => {
                    let msg = serde_json::to_string(&CloudMessage::Heartbeat).unwrap();
                    if let Err(e) = write.send(Message::Text(msg.into())).await {
                        return Err(format!("heartbeat send failed: {e}"));
                    }
                }

                // Push status periodically
                _ = status_interval.tick() => {
                    let processes = {
                        let st = daemon_state.lock().await;
                        st.process_mgr.list()
                    };
                    let msg = serde_json::to_string(&CloudMessage::Status { processes }).unwrap();
                    if let Err(e) = write.send(Message::Text(msg.into())).await {
                        return Err(format!("status push failed: {e}"));
                    }
                }

                // Receive commands from cloud
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<CloudMessage>(&text) {
                                Ok(CloudMessage::Command { action }) => {
                                    let response = execute_cloud_command(action).await;
                                    let resp_json = serde_json::to_string(&response).unwrap();
                                    if let Err(e) = write.send(Message::Text(resp_json.into())).await {
                                        return Err(format!("command response send failed: {e}"));
                                    }
                                }
                                Ok(_) => {} // ignore other messages
                                Err(e) => {
                                    warn!(error = %e, text = %text, "failed to parse cloud message");
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            let _ = write.send(Message::Pong(data)).await;
                        }
                        Some(Ok(Message::Close(_))) => {
                            info!("cloud server sent close");
                            return Ok(());
                        }
                        Some(Err(e)) => {
                            return Err(format!("WebSocket error: {e}"));
                        }
                        None => {
                            return Ok(()); // stream ended
                        }
                        _ => {} // ignore binary, pong, etc.
                    }
                }
            }
        }
    }
}

/// Execute a command received from the cloud by sending it through the Unix socket IPC.
/// This avoids Send issues by going through the same path as CLI commands.
async fn execute_cloud_command(action: CloudAction) -> Response {
    info!(action = ?action, "executing cloud command");

    let request = match action {
        CloudAction::Restart { name } => Request::Restart { name },
        CloudAction::Reload { name } => Request::Reload { name },
        CloudAction::Del { name } => Request::Del { name },
        CloudAction::Update {
            name,
            port,
            ssl,
            acme,
            env_file,
            args,
            image,
        } => Request::Update {
            name,
            port,
            ssl,
            acme,
            env_file,
            args,
            image,
        },
    };

    match crate::client::send_request_local(request).await {
        Ok(resp) => resp,
        Err(e) => Response::Error(format!("IPC error: {e}")),
    }
}

// --- State persistence helpers ---

const CLOUD_API_KEY: &str = "cloud:api_key";
const CLOUD_SERVER_URL: &str = "cloud:server_url";
pub const DEFAULT_CLOUD_URL: &str = "wss://cloud.nyrun.dev/agent/ws";

pub async fn save_cloud_config(
    state_store: &crate::state::StateStore,
    api_key: &str,
) -> Result<(), String> {
    state_store.put(CLOUD_API_KEY, api_key).await?;
    state_store.put(CLOUD_SERVER_URL, DEFAULT_CLOUD_URL).await?;
    Ok(())
}

pub async fn remove_cloud_config(state_store: &crate::state::StateStore) -> Result<(), String> {
    state_store.delete(CLOUD_API_KEY).await?;
    state_store.delete(CLOUD_SERVER_URL).await?;
    Ok(())
}

pub async fn load_cloud_config(state_store: &crate::state::StateStore) -> Option<(String, String)> {
    let api_key = state_store.get(CLOUD_API_KEY).await?;
    let url = state_store
        .get(CLOUD_SERVER_URL)
        .await
        .unwrap_or_else(|| DEFAULT_CLOUD_URL.to_string());
    Some((api_key, url))
}
