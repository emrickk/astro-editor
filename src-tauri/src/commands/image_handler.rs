use log::{error, info};
use std::env;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_COMMAND_LENGTH: usize = 16 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const IMAGE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Compute an augmented PATH so user commands (node, npm, etc.) resolve in
/// production builds, where the GUI app inherits a minimal environment.
pub(crate) fn get_augmented_path() -> OsString {
    let current_path = env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = env::split_paths(&current_path).collect();

    #[cfg(target_os = "macos")]
    let common_paths = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
    #[cfg(all(unix, not(target_os = "macos")))]
    let common_paths = ["/usr/local/bin", "/usr/bin", "/bin"];
    #[cfg(windows)]
    let common_paths = [r"C:\Program Files\nodejs"];

    for common_path in common_paths {
        let common_path = PathBuf::from(common_path);
        if !paths.contains(&common_path) {
            paths.push(common_path);
        }
    }

    env::join_paths(paths).unwrap_or(current_path)
}

fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

fn read_limited<R: Read>(mut reader: R) -> (Vec<u8>, bool) {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut exceeded = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let available = MAX_OUTPUT_BYTES.saturating_sub(retained.len());
                let keep = read.min(available);
                retained.extend_from_slice(&buffer[..keep]);
                if keep < read {
                    exceeded = true;
                }
            }
        }
    }
    (retained, exceeded)
}

fn terminate_process_tree(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("/bin/kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .output();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
}

#[tauri::command]
#[specta::specta]
pub async fn run_image_drop_command(
    command: String,
    image_path: String,
    project_path: String,
) -> Result<String, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("Image drop command is empty".to_string());
    }
    if command.len() > MAX_COMMAND_LENGTH || command.contains('\0') {
        return Err("Image drop command is invalid or too long".to_string());
    }
    let image = Path::new(&image_path)
        .canonicalize()
        .map_err(|_| "Dropped image was not found".to_string())?;
    if !image.is_file() {
        return Err("Dropped image was not found".to_string());
    }
    let project = Path::new(&project_path)
        .canonicalize()
        .map_err(|_| "Project path was not found".to_string())?;
    if !project.is_dir() || !project.join("package.json").is_file() {
        return Err("Project path is not an Astro project directory".to_string());
    }

    let command_line = format!("{} {}", command, shell_quote(&image.to_string_lossy()));
    info!(
        "Running the configured image command in {}",
        project.display()
    );
    let augmented_path = get_augmented_path();
    let output = tauri::async_runtime::spawn_blocking(move || {
        let _mutation_guard =
            super::project_mutation::try_lock_for_write(&project, "run the image command")?;
        #[cfg(unix)]
        let mut process = {
            let mut process = Command::new("/bin/sh");
            process.arg("-c").arg(&command_line);
            use std::os::unix::process::CommandExt;
            process.process_group(0);
            process
        };
        #[cfg(windows)]
        let mut process = {
            let mut process = Command::new("cmd.exe");
            process.arg("/C").arg(&command_line);
            process
        };
        let mut child = process
            .current_dir(&project)
            .env("PATH", augmented_path)
            .env_remove("CLAUDECODE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to run image command: {e}"))?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let stdout_reader = std::thread::spawn(move || read_limited(stdout));
        let stderr_reader = std::thread::spawn(move || read_limited(stderr));

        let started = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| format!("Failed to wait for image command: {e}"))?
            {
                break status;
            }
            if started.elapsed() >= IMAGE_COMMAND_TIMEOUT {
                terminate_process_tree(child.id());
                let _ = child.wait();
                return Err("Image command timed out after 5 minutes".to_string());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (stdout, stdout_exceeded) = stdout_reader.join().unwrap_or_default();
        let (stderr, stderr_exceeded) = stderr_reader.join().unwrap_or_default();
        if stdout_exceeded || stderr_exceeded {
            return Err("Image command output exceeded the 1 MB safety limit".to_string());
        }
        Ok((status, stdout, stderr))
    })
    .await
    .map_err(|e| format!("Failed to run image command: {e}"))??;

    let (status, stdout, stderr) = output;
    let stdout = String::from_utf8_lossy(&stdout).to_string();
    let stderr = String::from_utf8_lossy(&stderr).to_string();
    if !status.success() {
        let code = status
            .code()
            .map_or("signal".to_string(), |code| code.to_string());
        error!("Image drop command failed (exit {code})");
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        let tail = detail
            .lines()
            .rev()
            .take(8)
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

    fn test_project() -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("package.json"), "{}").unwrap();
        project
    }

    #[test]
    fn shell_quote_wraps_and_escapes() {
        assert_eq!(shell_quote("/a/b.jpg"), "'/a/b.jpg'");
        assert_eq!(shell_quote("/a/it's.jpg"), r"'/a/it'\''s.jpg'");
    }

    #[tokio::test]
    async fn runs_command_and_returns_stdout() {
        let project = test_project();
        let image = project.path().join("drop.png");
        std::fs::write(&image, b"fake").unwrap();
        let result = run_image_drop_command(
            "echo got:".to_string(),
            image.to_string_lossy().to_string(),
            project.path().to_string_lossy().to_string(),
        )
        .await
        .unwrap();
        assert!(result.trim().starts_with("got:"));
        assert!(result.trim().ends_with("drop.png"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_with_stderr() {
        let project = test_project();
        let image = project.path().join("drop.png");
        std::fs::write(&image, b"fake").unwrap();
        let error = run_image_drop_command(
            "sh -c 'echo boom >&2; exit 3' --".to_string(),
            image.to_string_lossy().to_string(),
            project.path().to_string_lossy().to_string(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("exit 3"), "unexpected error: {error}");
        assert!(error.contains("boom"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let project = test_project();
        let error = run_image_drop_command(
            "echo".to_string(),
            project
                .path()
                .join("missing.png")
                .to_string_lossy()
                .to_string(),
            project.path().to_string_lossy().to_string(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("not found"));
    }
}
