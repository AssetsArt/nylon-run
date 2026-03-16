use flate2::read::GzDecoder;
use oci_distribution::client::{ClientConfig, ClientProtocol};
use oci_distribution::manifest;
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use std::path::{Path, PathBuf};
use tar::Archive;
use tracing::{info, warn};

const OCI_DIR: &str = "/var/run/nyrun/oci";

/// Check if a path looks like an OCI image reference (not a local filesystem path).
pub fn is_oci_reference(path: &str) -> bool {
    // Local paths: start with /, ./, ../, or are simple filenames without /
    if path.starts_with('/')
        || path.starts_with("./")
        || path.starts_with("../")
        || !path.contains('/')
    {
        return false;
    }

    // OCI references contain a registry hostname with a dot or colon (port)
    // e.g. ghcr.io/org/image:tag, docker.io/library/alpine, localhost:5000/app
    let first_segment = path.split('/').next().unwrap_or("");
    first_segment.contains('.') || first_segment.contains(':')
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

    // Pull entire image (manifest + config + layers)
    let accepted = vec![
        manifest::IMAGE_LAYER_MEDIA_TYPE,
        manifest::IMAGE_LAYER_GZIP_MEDIA_TYPE,
        manifest::IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE,
    ];

    let image_data = client
        .pull(&img_ref, &auth, accepted)
        .await
        .map_err(|e| format!("failed to pull image: {e}"))?;

    // Create extraction directory
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("failed to create dir {}: {e}", dest_dir.display()))?;

    // Save config for entrypoint discovery
    let config_path = dest_dir.join(".oci-config.json");
    let _ = std::fs::write(&config_path, &image_data.config.data);

    // Extract layers
    for (i, layer) in image_data.layers.iter().enumerate() {
        info!(
            layer = i + 1,
            total = image_data.layers.len(),
            media_type = %layer.media_type,
            size = layer.data.len(),
            "extracting layer"
        );

        extract_layer(&layer.data, &layer.media_type, &dest_dir)?;
    }

    info!(
        reference,
        dest = %dest_dir.display(),
        "OCI image extracted"
    );

    Ok(dest_dir)
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
