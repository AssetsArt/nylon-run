use crate::protocol::ProcessConfig;
use slatedb::Db;
use slatedb::object_store::local::LocalFileSystem;
use slatedb::object_store::ObjectStore;
use std::sync::Arc;
use tracing::{error, info};

const STATE_DIR: &str = "/var/run/nyrun/state";

pub struct StateStore {
    db: Db,
}

impl StateStore {
    pub async fn open() -> Result<Self, String> {
        let object_store: Arc<dyn ObjectStore> =
            Arc::new(LocalFileSystem::new_with_prefix(STATE_DIR).map_err(|e| {
                format!("failed to create local object store at {STATE_DIR}: {e}")
            })?);
        let db = Db::open("/", object_store)
            .await
            .map_err(|e| format!("failed to open SlateDB: {e}"))?;
        Ok(Self { db })
    }

    pub async fn save(&self, configs: &[ProcessConfig]) -> Result<(), String> {
        // Delete all existing keys first
        let mut iter = self
            .db
            .scan_prefix(b"config:")
            .await
            .map_err(|e| format!("scan error: {e}"))?;
        let mut old_keys = Vec::new();
        while let Some(kv) = iter.next().await.map_err(|e| format!("iter error: {e}"))? {
            old_keys.push(kv.key.to_vec());
        }
        drop(iter);
        for key in &old_keys {
            self.db
                .delete(key)
                .await
                .map_err(|e| format!("delete error: {e}"))?;
        }

        // Write all current configs
        for config in configs {
            let key = format!("config:{}", config.name);
            let value =
                serde_json::to_vec(config).map_err(|e| format!("serialize error: {e}"))?;
            self.db
                .put(key.as_bytes(), &value)
                .await
                .map_err(|e| format!("put error: {e}"))?;
        }

        self.db
            .flush()
            .await
            .map_err(|e| format!("flush error: {e}"))?;
        info!(count = configs.len(), "state saved to SlateDB");
        Ok(())
    }

    pub async fn load(&self) -> Vec<ProcessConfig> {
        let mut configs = Vec::new();
        let mut iter = match self.db.scan_prefix(b"config:").await {
            Ok(it) => it,
            Err(e) => {
                error!(error = %e, "failed to scan SlateDB");
                return configs;
            }
        };

        loop {
            match iter.next().await {
                Ok(Some(kv)) => match serde_json::from_slice(&kv.value) {
                    Ok(config) => configs.push(config),
                    Err(e) => {
                        error!(
                            key = %String::from_utf8_lossy(&kv.key),
                            error = %e,
                            "failed to deserialize config"
                        );
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    error!(error = %e, "SlateDB iteration error");
                    break;
                }
            }
        }

        if !configs.is_empty() {
            info!(count = configs.len(), "state loaded from SlateDB");
        }
        configs
    }

    pub async fn put(&self, key: &str, value: &str) -> Result<(), String> {
        self.db
            .put(key.as_bytes(), value.as_bytes())
            .await
            .map_err(|e| format!("put error: {e}"))?;
        self.db.flush().await.map_err(|e| format!("flush error: {e}"))?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Option<String> {
        match self.db.get(key.as_bytes()).await {
            Ok(Some(val)) => String::from_utf8(val.to_vec()).ok(),
            _ => None,
        }
    }

    pub async fn delete(&self, key: &str) -> Result<(), String> {
        self.db
            .delete(key.as_bytes())
            .await
            .map_err(|e| format!("delete error: {e}"))?;
        Ok(())
    }

    pub async fn close(&self) {
        if let Err(e) = self.db.close().await {
            error!(error = %e, "failed to close SlateDB");
        }
    }
}

/// Migrate from old JSON state file to SlateDB if it exists.
pub async fn migrate_json_to_slatedb(store: &StateStore) {
    let json_path = std::path::Path::new("/var/run/nyrun/state.json");
    if !json_path.exists() {
        return;
    }

    let json = match std::fs::read_to_string(json_path) {
        Ok(j) => j,
        Err(e) => {
            error!(error = %e, "failed to read old state.json for migration");
            return;
        }
    };

    let configs: Vec<ProcessConfig> = match serde_json::from_str(&json) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "failed to parse old state.json for migration");
            return;
        }
    };

    if configs.is_empty() {
        // Nothing to migrate, just remove old file
        let _ = std::fs::remove_file(json_path);
        info!("removed empty state.json");
        return;
    }

    match store.save(&configs).await {
        Ok(()) => {
            let _ = std::fs::remove_file(json_path);
            info!(
                count = configs.len(),
                "migrated state.json to SlateDB and removed old file"
            );
        }
        Err(e) => {
            error!(error = %e, "failed to migrate state.json to SlateDB — keeping old file");
        }
    }
}
