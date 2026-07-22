use log::info;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// Downloads an image over https to a destination inside the project and
/// returns the absolute destination path. Used by the cover picker to bring
/// one of a post's remote (CDN) images into the repo's assets, where a
/// build-time-optimized cover must live.
#[tauri::command]
#[specta::specta]
pub async fn download_image_to_project(
    url: String,
    dest_path: String,
    project_path: String,
) -> Result<String, String> {
    if !url.starts_with("https://") {
        return Err("Only https image URLs can be downloaded".to_string());
    }
    let project = Path::new(&project_path)
        .canonicalize()
        .map_err(|e| format!("Project path not found: {e}"))?;
    let dest = PathBuf::from(&dest_path);
    let dest_abs = if dest.is_absolute() {
        dest
    } else {
        project.join(dest)
    };
    if dest_abs
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("Destination path may not contain ..".to_string());
    }
    if !dest_abs.starts_with(&project) {
        return Err("Destination must be inside the project".to_string());
    }

    info!("Downloading image {url} -> {}", dest_abs.display());
    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    const MAX_BYTES: usize = 50 * 1024 * 1024;
    if bytes.len() > MAX_BYTES {
        return Err("Image is larger than 50 MB".to_string());
    }

    if let Some(parent) = dest_abs.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    let mut file = std::fs::File::create(&dest_abs)
        .map_err(|e| format!("Could not write file: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("Could not write file: {e}"))?;

    Ok(dest_abs.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_https_urls() {
        let dir = std::env::temp_dir();
        let err = download_image_to_project(
            "http://example.com/x.png".to_string(),
            "x.png".to_string(),
            dir.to_string_lossy().to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("https"));
    }

    #[tokio::test]
    async fn rejects_destinations_outside_the_project() {
        let dir = std::env::temp_dir();
        let err = download_image_to_project(
            "https://example.com/x.png".to_string(),
            "/etc/x.png".to_string(),
            dir.to_string_lossy().to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("inside the project"), "got: {err}");

        let err = download_image_to_project(
            "https://example.com/x.png".to_string(),
            "a/../../x.png".to_string(),
            dir.to_string_lossy().to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.contains(".."), "got: {err}");
    }
}
