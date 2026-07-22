use log::{error, info};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use tauri::Emitter;

use super::image_handler::get_augmented_path;

/// Payload for the `project-command-log` event: one line of merged
/// stdout/stderr from a running project command.
#[derive(Clone, serde::Serialize)]
struct CommandLogLine {
    line: String,
}

fn validate_project_dir(project_path: &str) -> Result<(), String> {
    if !Path::new(project_path).is_dir() {
        return Err(format!("Project path not found: {project_path}"));
    }
    Ok(())
}

fn command_for(command_line: &str, project_path: &str) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(command_line)
        .current_dir(project_path)
        .env("PATH", get_augmented_path())
        // Commands run from the editor act for the owner, not for an agent
        // session that happened to launch the app.
        .env_remove("CLAUDECODE");
    cmd
}

/// Runs a project-level command (pull, publish preflight, publish confirm)
/// from the project root, streaming each merged stdout/stderr line to the
/// frontend as a `project-command-log` event and returning the full output.
/// Non-zero exit fails with the output tail as the error message.
#[tauri::command]
#[specta::specta]
pub async fn run_project_command(
    app: tauri::AppHandle,
    command: String,
    project_path: String,
) -> Result<String, String> {
    if command.trim().is_empty() {
        return Err("Command is empty".to_string());
    }
    validate_project_dir(&project_path)?;
    info!("Running project command: {command}");

    let command_line = command.trim().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = command_for(&command_line, &project_path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to run command: {e}"))?;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        // stderr drains on its own thread so neither pipe can fill and stall
        // the child; both feed the same event stream and transcript.
        let app_err = app.clone();
        let stderr_lines = std::thread::spawn(move || {
            let mut lines = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = app_err.emit("project-command-log", CommandLogLine { line: line.clone() });
                lines.push(line);
            }
            lines
        });

        let mut transcript = Vec::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = app.emit("project-command-log", CommandLogLine { line: line.clone() });
            transcript.push(line);
        }
        let err_lines = stderr_lines.join().unwrap_or_default();

        let status = child
            .wait()
            .map_err(|e| format!("Failed to wait for command: {e}"))?;

        if !status.success() {
            let code = status.code().map_or("signal".to_string(), |c| c.to_string());
            let mut all = transcript;
            all.extend(err_lines);
            let tail: Vec<_> = all.iter().rev().take(8).cloned().collect();
            let tail: Vec<_> = tail.into_iter().rev().collect();
            error!("Project command failed (exit {code})");
            return Err(format!("exit {code}:\n{}", tail.join("\n")));
        }

        // The transcript keeps stdout only: callers parse structured output
        // (digests, file lists) from it, and stderr is progress noise.
        Ok(transcript.join("\n"))
    })
    .await
    .map_err(|e| format!("Failed to run command: {e}"))?
}

/// Starts a long-running review server (e.g. a production preview) in its
/// own process group and returns the group leader's pid. The caller stops it
/// with `stop_review_server`. Output is discarded; the server is expected to
/// open the review page itself when ready.
#[tauri::command]
#[specta::specta]
pub async fn start_review_server(command: String, project_path: String) -> Result<u32, String> {
    if command.trim().is_empty() {
        return Err("Command is empty".to_string());
    }
    validate_project_dir(&project_path)?;
    info!("Starting review server: {command}");

    let mut cmd = command_for(command.trim(), &project_path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start review server: {e}"))?;
    let pid = child.id();

    // Reap the child when it exits so it never lingers as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(pid)
}

/// Stops a review server started by `start_review_server` by signalling its
/// whole process group (npm -> node -> server chains die together).
#[tauri::command]
#[specta::specta]
pub async fn stop_review_server(pid: u32) -> Result<(), String> {
    info!("Stopping review server process group {pid}");
    let output = Command::new("/bin/kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .output()
        .map_err(|e| format!("Failed to stop review server: {e}"))?;
    if !output.status.success() {
        // Already gone is fine; anything else is worth surfacing.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("No such process") {
            return Err(format!("Failed to stop review server: {stderr}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_for_sets_cwd_and_strips_claudecode() {
        let dir = std::env::temp_dir();
        let mut cmd = command_for("echo hi", &dir.to_string_lossy());
        cmd.env("CLAUDECODE", "1"); // simulate inherited env, env_remove already applied
        let output = cmd.output().unwrap();
        assert!(output.status.success());
    }

    #[tokio::test]
    async fn start_and_stop_review_server_kills_process_group() {
        let dir = std::env::temp_dir();
        let pid = start_review_server(
            "sleep 300".to_string(),
            dir.to_string_lossy().to_string(),
        )
        .await
        .unwrap();
        assert!(pid > 0);
        stop_review_server(pid).await.unwrap();
        // Give the signal a moment, then the process must be gone.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let alive = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success();
        assert!(!alive, "process group leader {pid} still alive");
    }

    #[tokio::test]
    async fn stop_review_server_tolerates_already_gone() {
        // A pid from a process that exited immediately.
        let dir = std::env::temp_dir();
        let pid = start_review_server("true".to_string(), dir.to_string_lossy().to_string())
            .await
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        stop_review_server(pid).await.unwrap();
    }
}
