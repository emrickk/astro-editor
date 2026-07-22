use log::{error, info};
use std::env;
use std::path::Path;
use std::process::Command;

/// Compute an augmented PATH so user commands (node, npm, etc.) resolve in
/// production builds, where the GUI app inherits a minimal environment.
fn get_augmented_path() -> String {
    let current_path = env::var("PATH").unwrap_or_default();
    let mut paths: Vec<&str> = current_path.split(':').collect();

    #[cfg(target_os = "macos")]
    let common_paths = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
    #[cfg(not(target_os = "macos"))]
    let common_paths = ["/usr/local/bin", "/usr/bin", "/bin"];

    for common_path in &common_paths {
        if !paths.contains(common_path) {
            paths.push(common_path);
        }
    }

    paths.join(":")
}

/// Quote a path for safe interpolation into a POSIX shell command line.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// Runs the project's configured image drop command for a dropped image.
///
/// The command runs via `sh -c` with the project root as working directory
/// and the image path appended as a single quoted argument. Whatever the
/// command prints to stdout is returned verbatim; the frontend inserts it
/// into the editor (typically a markdown image snippet pointing at a CDN).
/// A non-zero exit fails the drop, with stderr as the error message.
#[tauri::command]
#[specta::specta]
pub async fn run_image_drop_command(
    command: String,
    image_path: String,
    project_path: String,
) -> Result<String, String> {
    if command.trim().is_empty() {
        return Err("Image drop command is empty".to_string());
    }
    if !Path::new(&image_path).is_file() {
        return Err(format!("Dropped file not found: {image_path}"));
    }
    if !Path::new(&project_path).is_dir() {
        return Err(format!("Project path not found: {project_path}"));
    }

    let command_line = format!("{} {}", command.trim(), shell_quote(&image_path));
    info!("Running image drop command: {command_line}");

    let augmented_path = get_augmented_path();
    let output = tauri::async_runtime::spawn_blocking(move || {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(&command_line)
            .current_dir(&project_path)
            .env("PATH", augmented_path)
            .output()
    })
    .await
    .map_err(|e| format!("Failed to run image drop command: {e}"))?
    .map_err(|e| format!("Failed to run image drop command: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map_or("signal".to_string(), |c| c.to_string());
        error!("Image drop command failed (exit {code}): {stderr}");
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        // Keep the tail: pipeline logs can be long, the cause is usually last.
        let tail: String = detail
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("Image command failed (exit {code}): {tail}"));
    }

    if stdout.trim().is_empty() {
        return Err("Image command produced no output to insert".to_string());
    }

    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_and_escapes() {
        assert_eq!(shell_quote("/a/b.jpg"), "'/a/b.jpg'");
        assert_eq!(shell_quote("/a/it's.jpg"), r"'/a/it'\''s.jpg'");
    }

    #[tokio::test]
    async fn runs_command_and_returns_stdout() {
        let dir = std::env::temp_dir();
        let img = dir.join("ae-test-drop.png");
        std::fs::write(&img, b"fake").unwrap();

        let result = run_image_drop_command(
            "echo got:".to_string(),
            img.to_string_lossy().to_string(),
            dir.to_string_lossy().to_string(),
        )
        .await
        .unwrap();
        assert!(result.trim().starts_with("got:"));
        assert!(result.trim().ends_with("ae-test-drop.png"));
        std::fs::remove_file(&img).ok();
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_with_stderr() {
        let dir = std::env::temp_dir();
        let img = dir.join("ae-test-drop2.png");
        std::fs::write(&img, b"fake").unwrap();

        let err = run_image_drop_command(
            "sh -c 'echo boom >&2; exit 3' --".to_string(),
            img.to_string_lossy().to_string(),
            dir.to_string_lossy().to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("exit 3"), "unexpected error: {err}");
        assert!(err.contains("boom"), "unexpected error: {err}");
        std::fs::remove_file(&img).ok();
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let err = run_image_drop_command(
            "echo".to_string(),
            "/nonexistent/nope.png".to_string(),
            std::env::temp_dir().to_string_lossy().to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("not found"));
    }
}
