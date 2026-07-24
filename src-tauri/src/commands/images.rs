use log::info;
use reqwest::{header, redirect, Client, Response, Url};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const MAX_BYTES: usize = 50 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113))
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    // Unique-local fc00::/7, link-local fe80::/10, and documentation 2001:db8::/32.
    if (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
    {
        return false;
    }
    ip.to_ipv4_mapped().map_or(true, is_public_ipv4)
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

async fn validate_remote_url(url: &Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("Only https image URLs can be downloaded".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Image URLs may not contain credentials".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "Image URL has no host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err("Private network image URLs are not allowed".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err("Private network image URLs are not allowed".to_string());
        }
        return Ok(());
    }

    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("Could not resolve image host: {e}"))?;
    if addresses
        .into_iter()
        .any(|address| !is_public_ip(address.ip()))
    {
        return Err("Private network image URLs are not allowed".to_string());
    }
    Ok(())
}

async fn follow_safe_redirects(client: &Client, initial: Url) -> Result<Response, String> {
    let mut current = initial;
    for redirect_count in 0..=MAX_REDIRECTS {
        validate_remote_url(&current).await?;
        let response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|e| format!("Download failed: {e}"))?;

        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err("Download failed: too many redirects".to_string());
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .ok_or_else(|| "Download redirect has no location".to_string())?
                .to_str()
                .map_err(|_| "Download redirect location is invalid".to_string())?;
            current = current
                .join(location)
                .map_err(|_| "Download redirect location is invalid".to_string())?;
            continue;
        }

        return Ok(response);
    }
    Err("Download failed: too many redirects".to_string())
}

fn looks_like_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
        || (bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && &bytes[8..12] == b"avif")
}

fn resolve_safe_destination(project_path: &str, dest_path: &str) -> Result<PathBuf, String> {
    let requested_project = Path::new(project_path);
    let project = requested_project
        .canonicalize()
        .map_err(|e| format!("Project path not found: {e}"))?;
    let requested = Path::new(dest_path);
    let relative = if requested.is_absolute() {
        requested
            .strip_prefix(&project)
            .or_else(|_| requested.strip_prefix(requested_project))
            .map_err(|_| "Destination must be inside the project".to_string())?
            .to_path_buf()
    } else {
        requested.to_path_buf()
    };

    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("Destination must be a file inside the project".to_string());
    }

    let parent = relative
        .parent()
        .ok_or_else(|| "Destination has no parent directory".to_string())?;
    let mut current = project.clone();
    for component in parent.components() {
        if matches!(component, Component::CurDir) {
            continue;
        }
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("Destination may not pass through a symlink".to_string());
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "Destination parent is not a directory: {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .map_err(|e| format!("Could not create {}: {e}", current.display()))?;
            }
            Err(error) => {
                return Err(format!("Could not inspect {}: {error}", current.display()));
            }
        }
    }

    let destination = project.join(relative);
    if fs::symlink_metadata(&destination).is_ok() {
        return Err("Destination already exists".to_string());
    }
    Ok(destination)
}

fn write_atomic_no_overwrite(destination: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "Destination has no parent directory".to_string())?;
    let temporary = parent.join(format!(
        ".astro-editor-download-{}.tmp",
        uuid::Uuid::new_v4()
    ));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| format!("Could not create temporary image: {e}"))?;
        file.write_all(bytes)
            .map_err(|e| format!("Could not write image: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("Could not finish image write: {e}"))?;
        fs::hard_link(&temporary, destination).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                "Destination already exists".to_string()
            } else {
                format!("Could not install downloaded image: {e}")
            }
        })?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

/// Downloads a verified image over public HTTPS to a new destination inside
/// the project. Redirects, response size, content, symlinks, and overwrites
/// are all checked before an atomic install.
#[tauri::command]
#[specta::specta]
pub async fn download_image_to_project(
    url: String,
    dest_path: String,
    project_path: String,
) -> Result<String, String> {
    let initial = Url::parse(&url).map_err(|_| "Image URL is invalid".to_string())?;
    let destination = resolve_safe_destination(&project_path, &dest_path)?;
    info!("Downloading a remote image to {}", destination.display());

    let client = Client::builder()
        .redirect(redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| format!("Could not initialize downloader: {e}"))?;
    let mut response = follow_safe_redirects(&client, initial).await?;
    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type.to_ascii_lowercase().starts_with("image/") {
        return Err("Download did not return an image".to_string());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES as u64)
    {
        return Err("Image is larger than 50 MB".to_string());
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("Download failed: {e}"))?
    {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err("Image is larger than 50 MB".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    if !looks_like_image(&bytes) {
        return Err("Downloaded content is not a supported image".to_string());
    }

    let destination_for_write = destination.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _mutation_guard = super::project_mutation::try_lock_for_write(
            Path::new(&project_path),
            "install the downloaded image",
        )?;
        write_atomic_no_overwrite(&destination_for_write, &bytes)
    })
    .await
    .map_err(|e| format!("Image write task failed: {e}"))??;

    Ok(destination.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_and_special_ip_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "192.168.1.1",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "accepted {ip}");
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn validates_supported_image_signatures() {
        assert!(looks_like_image(b"\x89PNG\r\n\x1a\nrest"));
        assert!(looks_like_image(b"\xff\xd8\xffrest"));
        assert!(looks_like_image(b"RIFFxxxxWEBPrest"));
        assert!(!looks_like_image(b"<html>not an image</html>"));
    }

    #[test]
    fn refuses_to_overwrite_existing_destination() {
        let project = tempfile::tempdir().unwrap();
        let destination = project.path().join("cover.webp");
        fs::write(&destination, b"original").unwrap();
        let error = resolve_safe_destination(
            project.path().to_str().unwrap(),
            destination.to_str().unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("already exists"));
        assert_eq!(fs::read(&destination).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_parent_directories() {
        use std::os::unix::fs::symlink;

        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), project.path().join("hero")).unwrap();
        let error = resolve_safe_destination(project.path().to_str().unwrap(), "hero/cover.webp")
            .unwrap_err();
        assert!(error.contains("symlink"));
    }

    #[test]
    fn atomic_write_creates_new_file_without_partial_temp() {
        let project = tempfile::tempdir().unwrap();
        let destination = project.path().join("cover.webp");
        write_atomic_no_overwrite(&destination, b"RIFFxxxxWEBPdata").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"RIFFxxxxWEBPdata");
        assert_eq!(fs::read_dir(project.path()).unwrap().count(), 1);
    }
}
