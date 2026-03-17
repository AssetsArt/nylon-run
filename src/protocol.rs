use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// --- Process types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProcessMode {
    Bin,
    Run,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub host: Option<String>,
    pub public_port: u16,
    pub app_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SslConfig {
    pub cert_path: String,
    pub key_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessConfig {
    pub name: String,
    pub path: String,
    pub args: Vec<String>,
    pub env_vars: HashMap<String, String>,
    pub env_file: Option<String>,
    pub mode: ProcessMode,
    pub port_mapping: Option<PortMapping>,
    pub spa: bool,
    pub ssl: Option<SslConfig>,
    pub acme: Option<String>,
    pub deny: Vec<String>,
    pub allow: Vec<String>,
    #[serde(default)]
    pub is_oci: bool,
    #[serde(default)]
    pub oci_reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProcessStatus {
    Running,
    Stopped,
    Errored,
    Starting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub name: String,
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    pub mode: ProcessMode,
    pub path: String,
    pub port_mapping: Option<PortMapping>,
    pub started_at: Option<DateTime<Utc>>,
    pub restart_count: u32,
    pub uptime_secs: Option<u64>,
}

// --- IPC messages ---

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Bin {
        config: ProcessConfig,
    },
    Run {
        config: ProcessConfig,
    },
    Ls,
    Del {
        name: String,
    },
    Restart {
        name: String,
    },
    Reload {
        name: String,
    },
    Update {
        name: String,
        port: Option<String>,
        ssl: Option<Vec<String>>,
        acme: Option<String>,
        env_file: Option<String>,
        args: Option<String>,
        image: Option<String>,
    },
    Logs {
        name: String,
        lines: usize,
    },
    Save,
    Kill,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ok(String),
    ProcessList(Vec<ProcessInfo>),
    Logs(String),
    Error(String),
}

// --- Wire protocol: 4-byte length prefix + JSON ---

pub async fn write_message<W: AsyncWriteExt + Unpin, T: Serialize>(
    writer: &mut W,
    msg: &T,
) -> std::io::Result<()> {
    let data = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    let len = data.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&data).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_message<R: AsyncReadExt + Unpin, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> std::io::Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 10 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf).map_err(std::io::Error::other)
}
