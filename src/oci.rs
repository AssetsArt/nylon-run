use flate2::read::GzDecoder;
use oci_distribution::client::{ClientConfig, ClientProtocol};
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use std::path::{Path, PathBuf};
use tar::Archive;
use tracing::{info, warn};

const OCI_DIR: &str = "/var/run/nyrun/oci";
const DEFAULT_REGISTRY: &str = "docker.io";
const SETTINGS_FILE: &str = "/var/run/nyrun/settings.json";

/// Check if a path looks like an OCI image reference (not a local filesystem path).
/// Accepts both full references (ghcr.io/org/app:tag) and short names (traefik:v3.6, nginx).
pub fn is_oci_reference(path: &str) -> bool {
    // Local paths: start with /, ./, ../
    if path.starts_with('/') || path.starts_with("./") || path.starts_with("../") {
        return false;
    }

    // Full reference: contains a registry hostname with a dot or colon (port)
    // e.g. ghcr.io/org/image:tag, docker.io/library/alpine, localhost:5000/app
    if path.contains('/') {
        let first_segment = path.split('/').next().unwrap_or("");
        if first_segment.contains('.') || first_segment.contains(':') {
            return true;
        }
    }

    // Short name with tag: e.g. "traefik:v3.6", "nginx:latest", "redis:7"
    // Must not look like a local file path (no path separators, has a colon for tag)
    if !path.contains('/') && path.contains(':') {
        let parts: Vec<&str> = path.splitn(2, ':').collect();
        let name = parts[0];
        // Bare name before colon should be a simple alphanumeric image name
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return true;
        }
    }

    false
}

/// Normalize a short OCI reference by prepending the default registry.
/// e.g. "traefik:v3.6" -> "docker.io/library/traefik:v3.6"
/// e.g. "ghcr.io/org/app:v1" -> "ghcr.io/org/app:v1" (unchanged)
pub fn normalize_reference(reference: &str) -> String {
    // Already a full reference with registry
    if reference.contains('/') {
        let first_segment = reference.split('/').next().unwrap_or("");
        if first_segment.contains('.') || first_segment.contains(':') {
            return reference.to_string();
        }
    }

    // Short name — prepend default registry
    let registry = load_default_registry();
    if registry == "docker.io" {
        // Docker Hub uses "library/" namespace for official images
        format!("{}/library/{}", registry, reference)
    } else {
        format!("{}/{}", registry, reference)
    }
}

fn load_default_registry() -> String {
    if let Ok(data) = std::fs::read_to_string(SETTINGS_FILE)
        && let Ok(settings) = serde_json::from_str::<serde_json::Value>(&data)
        && let Some(reg) = settings.get("default-registry").and_then(|v| v.as_str())
    {
        return reg.to_string();
    }
    DEFAULT_REGISTRY.to_string()
}

pub fn save_default_registry(registry: &str) -> Result<(), String> {
    let mut settings = if let Ok(data) = std::fs::read_to_string(SETTINGS_FILE) {
        serde_json::from_str::<serde_json::Value>(&data).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    settings["default-registry"] = serde_json::Value::String(registry.to_string());
    let dir = std::path::Path::new(SETTINGS_FILE).parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create dir: {e}"))?;
    std::fs::write(
        SETTINGS_FILE,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .map_err(|e| format!("failed to write settings: {e}"))?;
    Ok(())
}

/// Extract image name from an OCI reference for use as default process name.
/// e.g. "ghcr.io/org/myapp:v1.2" -> "myapp"
pub fn image_name_from_ref(reference: &str) -> String {
    // Split off tag/digest
    let without_tag = reference.split(':').next().unwrap_or(reference);
    let without_digest = without_tag.split('@').next().unwrap_or(without_tag);

    // Get last path segment
    without_digest
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// Pull an OCI image and extract it to /var/run/nyrun/oci/<name>/
/// Returns the path to the extraction directory.
pub async fn pull_and_extract(reference: &str, name: &str) -> Result<PathBuf, String> {
    let dest_dir = PathBuf::from(OCI_DIR).join(name);

    // Parse reference
    let img_ref: Reference = reference
        .parse()
        .map_err(|e| format!("invalid OCI reference '{}': {}", reference, e))?;

    info!(reference, name, "pulling OCI image");

    let config = ClientConfig {
        protocol: ClientProtocol::Https,
        ..Default::default()
    };
    let client = Client::new(config);
    let auth = RegistryAuth::Anonymous;

    // Step 1: Pull manifest
    info!("  Pulling manifest...");
    let (img_manifest, _digest) = client
        .pull_image_manifest(&img_ref, &auth)
        .await
        .map_err(|e| format!("failed to pull manifest: {e}"))?;

    // Create extraction directory
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("failed to create dir {}: {e}", dest_dir.display()))?;

    // Step 2: Pull config blob
    info!("  Pulling config...");
    let mut config_data = Vec::new();
    client
        .pull_blob(&img_ref, &img_manifest.config, &mut config_data)
        .await
        .map_err(|e| format!("failed to pull config: {e}"))?;

    let config_path = dest_dir.join(".oci-config.json");
    std::fs::write(&config_path, &config_data)
        .map_err(|e| format!("failed to write config: {e}"))?;

    // Step 3: Pull and extract each layer with progress
    let total_layers = img_manifest.layers.len();
    for (i, layer) in img_manifest.layers.iter().enumerate() {
        info!(
            "  Pulling layer {}/{}: {} ({})",
            i + 1,
            total_layers,
            layer.digest,
            format_size(layer.size as u64),
        );

        let mut layer_data = Vec::new();
        client
            .pull_blob(&img_ref, layer, &mut layer_data)
            .await
            .map_err(|e| format!("failed to pull layer {}: {e}", layer.digest))?;

        info!("  Extracting layer {}/{}...", i + 1, total_layers,);
        extract_layer(&layer_data, &layer.media_type, &dest_dir)?;
    }

    info!(
        reference,
        dest = %dest_dir.display(),
        layers = total_layers,
        "OCI image pulled and extracted"
    );

    Ok(dest_dir)
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn extract_layer(data: &[u8], media_type: &str, dest_dir: &Path) -> Result<(), String> {
    if media_type.contains("gzip") {
        let decoder = GzDecoder::new(data);
        let mut archive = Archive::new(decoder);
        archive.set_preserve_permissions(true);
        archive.set_overwrite(true);
        extract_tar_entries(&mut archive, dest_dir)
    } else if media_type.contains("tar") {
        let mut archive = Archive::new(data);
        archive.set_preserve_permissions(true);
        archive.set_overwrite(true);
        extract_tar_entries(&mut archive, dest_dir)
    } else {
        warn!(media_type, "unknown layer media type, skipping");
        Ok(())
    }
}

fn extract_tar_entries<R: std::io::Read>(
    archive: &mut Archive<R>,
    dest_dir: &Path,
) -> Result<(), String> {
    for entry in archive
        .entries()
        .map_err(|e| format!("tar entries error: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("tar entry error: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("tar path error: {e}"))?
            .into_owned();

        // Skip whiteout files (.wh.*)
        if let Some(name) = path.file_name().and_then(|n: &std::ffi::OsStr| n.to_str())
            && name.starts_with(".wh.")
        {
            // OCI whiteout: delete the corresponding file
            let target_name = name.strip_prefix(".wh.").unwrap();
            let target = dest_dir
                .join(path.parent().unwrap_or(Path::new("")))
                .join(target_name);
            let _ = std::fs::remove_file(&target);
            let _ = std::fs::remove_dir_all(&target);
            continue;
        }

        // Security: prevent path traversal
        let full_path = dest_dir.join(&path);
        if !full_path.starts_with(dest_dir) {
            warn!(path = %path.display(), "skipping path traversal in tar");
            continue;
        }

        let _ = entry.unpack_in(dest_dir);
    }

    Ok(())
}

/// Find the entrypoint binary in an extracted OCI image.
/// Reads the OCI config JSON and looks for Entrypoint or Cmd.
pub fn find_entrypoint(extract_dir: &Path) -> Result<(String, Vec<String>), String> {
    let config_path = extract_dir.join(".oci-config.json");

    if let Ok(data) = std::fs::read_to_string(&config_path)
        && let Ok(config) = serde_json::from_str::<serde_json::Value>(&data)
        && let Some(cfg) = config.get("config")
    {
        // Try Entrypoint first
        if let Some(entrypoint) = cfg.get("Entrypoint").and_then(|e| e.as_array()) {
            let parts: Vec<String> = entrypoint
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if !parts.is_empty() {
                let bin = resolve_binary(extract_dir, &parts[0]);
                let args = parts[1..].to_vec();
                return Ok((bin, args));
            }
        }
        // Then Cmd
        if let Some(cmd) = cfg.get("Cmd").and_then(|c| c.as_array()) {
            let parts: Vec<String> = cmd
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if !parts.is_empty() {
                let bin = resolve_binary(extract_dir, &parts[0]);
                let args = parts[1..].to_vec();
                return Ok((bin, args));
            }
        }
    }

    // Fallback: search common bin directories for executables
    for dir in ["usr/local/bin", "usr/bin", "bin", "app", "."] {
        let search_dir = extract_dir.join(dir);
        if let Ok(entries) = std::fs::read_dir(&search_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && is_executable(&path) {
                    return Ok((path.to_string_lossy().to_string(), Vec::new()));
                }
            }
        }
    }

    Err(format!(
        "no entrypoint found in OCI image at {}",
        extract_dir.display()
    ))
}

/// Resolve a binary path relative to the extraction directory.
fn resolve_binary(extract_dir: &Path, bin: &str) -> String {
    if bin.starts_with('/') {
        let relative = bin.strip_prefix('/').unwrap_or(bin);
        let resolved = extract_dir.join(relative);
        if resolved.exists() {
            return resolved.to_string_lossy().to_string();
        }
    }

    let in_dir = extract_dir.join(bin);
    if in_dir.exists() {
        return in_dir.to_string_lossy().to_string();
    }

    // Return original (might be a system binary like /bin/sh)
    bin.to_string()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
