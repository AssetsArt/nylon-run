use crate::protocol::ProcessConfig;
use std::path::PathBuf;
use tracing::{error, info};

const NYRUN_DIR: &str = "/var/run/nyrun";

fn state_path() -> PathBuf {
    PathBuf::from(NYRUN_DIR).join("state.json")
}

pub fn save_state(configs: &[ProcessConfig]) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(configs).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(state_path(), json).map_err(|e| format!("write error: {e}"))?;
    info!(count = configs.len(), "state saved");
    Ok(())
}

pub fn load_state() -> Vec<ProcessConfig> {
    let path = state_path();
    if !path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(json) => match serde_json::from_str(&json) {
            Ok(configs) => {
                info!("state loaded");
                configs
            }
            Err(e) => {
                error!(error = %e, "failed to parse state file");
                Vec::new()
            }
        },
        Err(e) => {
            error!(error = %e, "failed to read state file");
            Vec::new()
        }
    }
}
