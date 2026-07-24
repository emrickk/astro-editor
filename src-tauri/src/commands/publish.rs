use log::{error, info};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Emitter;

use super::image_handler::get_augmented_path;

const MAX_COMMAND_LENGTH: usize = 16 * 1024;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[cfg(not(test))]
const REVIEW_STOP_GRACE: Duration = Duration::from_secs(3);
#[cfg(test)]
const REVIEW_STOP_GRACE: Duration = Duration::from_millis(250);
const REVIEW_KILL_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct ReviewServerProcess {
    pid: u32,
    project: PathBuf,
    exited: Arc<(Mutex<bool>, Condvar)>,
}

static REVIEW_SERVERS: OnceLock<Mutex<HashMap<String, ReviewServerProcess>>> = OnceLock::new();

fn review_servers() -> &'static Mutex<HashMap<String, ReviewServerProcess>> {
    REVIEW_SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn ensure_no_review_server_for_project(project: &Path) -> Result<(), String> {
    let project = project
        .canonicalize()
        .map_err(|error| format!("Project path not found: {error}"))?;
    let running = review_servers()
        .lock()
        .map_err(|_| "Review server registry is unavailable".to_string())?
        .values()
        .any(|process| process.project == project);
    if running {
        return Err(
            "Pull stopped because this project's review server is still running. Stop the review and try again."
                .to_string(),
        );
    }
    Ok(())
}

#[derive(Clone, serde::Serialize)]
struct CommandLogLine {
    line: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewServerLogLine {
    id: String,
    line: String,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewServerExit {
    id: String,
    success: bool,
    code: Option<i32>,
}

fn validate_command(command: &str) -> Result<&str, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("Command is empty".to_string());
    }
    if command.len() > MAX_COMMAND_LENGTH || command.contains('\0') {
        return Err("Command is invalid or too long".to_string());
    }
    Ok(command)
}

fn validate_project_dir(project_path: &str) -> Result<PathBuf, String> {
    let project = Path::new(project_path)
        .canonicalize()
        .map_err(|e| format!("Project path not found: {e}"))?;
    if !project.is_dir() || !project.join("package.json").is_file() {
        return Err("Project path is not an Astro project directory".to_string());
    }
    Ok(project)
}

fn command_for(command_line: &str, project_path: &Path) -> Command {
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(command_line);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        command.arg("/C").arg(command_line);
        command
    };

    command
        .current_dir(project_path)
        .env("PATH", get_augmented_path())
        .env_remove("CLAUDECODE");
    command
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
}

fn signal_process_tree(pid: u32, force: bool) -> Result<(), String> {
    #[cfg(unix)]
    let output = Command::new("/bin/kill")
        .args([
            if force { "-KILL" } else { "-TERM" },
            "--",
            &format!("-{pid}"),
        ])
        .output()
        .map_err(|e| format!("Failed to stop process: {e}"))?;

    #[cfg(windows)]
    let _ = force;
    #[cfg(windows)]
    let output = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .map_err(|e| format!("Failed to stop process: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let already_gone = stderr.contains("No such process")
            || stderr.contains("not found")
            || stderr.contains("not running");
        if !already_gone {
            return Err(format!("Failed to stop process: {}", stderr.trim()));
        }
    }
    Ok(())
}

fn terminate_process_tree(pid: u32) -> Result<(), String> {
    signal_process_tree(pid, false)
}

fn force_terminate_process_tree(pid: u32) -> Result<(), String> {
    signal_process_tree(pid, true)
}

fn read_bounded_lines<R: std::io::Read + Send + 'static>(
    reader: R,
    app: tauri::AppHandle,
    event: &'static str,
    used_bytes: Arc<AtomicUsize>,
    exceeded: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut lines = Vec::new();
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            let next = used_bytes.fetch_add(line.len() + 1, Ordering::Relaxed) + line.len() + 1;
            if next > MAX_OUTPUT_BYTES {
                exceeded.store(true, Ordering::Relaxed);
                continue;
            }
            let _ = app.emit(event, CommandLogLine { line: line.clone() });
            lines.push(line);
        }
        lines
    })
}

fn read_bounded_review_lines<R: std::io::Read + Send + 'static>(
    reader: R,
    app: tauri::AppHandle,
    id: String,
    used_bytes: Arc<AtomicUsize>,
    exceeded: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut lines = Vec::new();
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            let next = used_bytes.fetch_add(line.len() + 1, Ordering::Relaxed) + line.len() + 1;
            if next > MAX_OUTPUT_BYTES {
                exceeded.store(true, Ordering::Relaxed);
                continue;
            }
            let _ = app.emit(
                "review-server-log",
                ReviewServerLogLine {
                    id: id.clone(),
                    line: line.clone(),
                },
            );
            lines.push(line);
        }
        lines
    })
}

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<ExitStatus, String> {
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("Failed to wait for command: {e}"))?
        {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = terminate_process_tree(child.id());
            let _ = child.wait();
            return Err(format!(
                "Command timed out after {} minutes",
                timeout.as_secs() / 60
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Runs a configured project command with bounded output and a timeout.
#[tauri::command]
#[specta::specta]
pub async fn run_project_command(
    app: tauri::AppHandle,
    command: String,
    project_path: String,
) -> Result<String, String> {
    let command_line = validate_command(&command)?.to_string();
    let project = validate_project_dir(&project_path)?;
    info!(
        "Running a configured project command in {}",
        project.display()
    );

    tauri::async_runtime::spawn_blocking(move || {
        let _mutation_guard =
            super::project_mutation::try_lock_for_write(&project, "run the project command")?;
        let mut cmd = command_for(&command_line, &project);
        configure_process_group(&mut cmd);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to run command: {e}"))?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let used_bytes = Arc::new(AtomicUsize::new(0));
        let exceeded = Arc::new(AtomicBool::new(false));
        let stdout_lines = read_bounded_lines(
            stdout,
            app.clone(),
            "project-command-log",
            used_bytes.clone(),
            exceeded.clone(),
        );
        let stderr_lines = read_bounded_lines(
            stderr,
            app,
            "project-command-log",
            used_bytes,
            exceeded.clone(),
        );

        let status = wait_with_timeout(&mut child, COMMAND_TIMEOUT)?;
        let stdout_lines = stdout_lines.join().unwrap_or_default();
        let stderr_lines = stderr_lines.join().unwrap_or_default();
        if exceeded.load(Ordering::Relaxed) {
            return Err("Command output exceeded the 2 MB safety limit".to_string());
        }
        if !status.success() {
            let code = status
                .code()
                .map_or("signal".to_string(), |code| code.to_string());
            let mut all = stdout_lines;
            all.extend(stderr_lines);
            let noise = |line: &&String| {
                let line = line.trim_start();
                !(line.starts_with("npm warn")
                    || line.starts_with("npm WARN")
                    || line.starts_with("npm notice"))
            };
            let tail: Vec<_> = all.iter().filter(noise).rev().take(20).cloned().collect();
            let tail: Vec<_> = tail.into_iter().rev().collect();
            error!("Project command failed (exit {code})");
            return Err(format!("exit {code}:\n{}", tail.join("\n")));
        }
        Ok(stdout_lines.join("\n"))
    })
    .await
    .map_err(|e| format!("Failed to run command: {e}"))?
}

#[tauri::command]
#[specta::specta]
pub async fn start_review_server(
    app: tauri::AppHandle,
    command: String,
    project_path: String,
) -> Result<String, String> {
    start_review_server_inner(Some(app), command, project_path)
}

fn start_review_server_inner(
    app: Option<tauri::AppHandle>,
    command: String,
    project_path: String,
) -> Result<String, String> {
    let command_line = validate_command(&command)?;
    let project = validate_project_dir(&project_path)?;
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(&project, "start the review server")?;
    info!(
        "Starting a configured review server in {}",
        project.display()
    );

    let mut cmd = command_for(command_line, &project);
    configure_process_group(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to start review server: {e}"))?;
    let pid = child.id();
    let id = uuid::Uuid::new_v4().to_string();
    let exited = Arc::new((Mutex::new(false), Condvar::new()));
    let process = ReviewServerProcess {
        pid,
        project: project.clone(),
        exited: exited.clone(),
    };
    match review_servers().lock() {
        Ok(mut servers) => {
            servers.insert(id.clone(), process);
        }
        Err(_) => {
            let _ = force_terminate_process_tree(pid);
            let _ = child.wait();
            return Err("Review server registry is unavailable".to_string());
        }
    }

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let used_bytes = Arc::new(AtomicUsize::new(0));
    let exceeded = Arc::new(AtomicBool::new(false));
    if let Some(app) = &app {
        read_bounded_review_lines(
            stdout,
            app.clone(),
            id.clone(),
            used_bytes.clone(),
            exceeded.clone(),
        );
        read_bounded_review_lines(stderr, app.clone(), id.clone(), used_bytes, exceeded);
    } else {
        std::thread::spawn(move || for _ in BufReader::new(stdout).lines() {});
        std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
    }

    let exit_id = id.clone();
    std::thread::spawn(move || {
        let status = child.wait();
        let (exited_lock, exited_signal) = &*exited;
        if let Ok(mut has_exited) = exited_lock.lock() {
            *has_exited = true;
            exited_signal.notify_all();
        }
        if let Ok(mut servers) = review_servers().lock() {
            if servers
                .get(&exit_id)
                .is_some_and(|registered| registered.pid == pid)
            {
                servers.remove(&exit_id);
            }
        }
        if let Some(app) = app {
            let payload = match status {
                Ok(status) => ReviewServerExit {
                    id: exit_id,
                    success: status.success(),
                    code: status.code(),
                },
                Err(_) => ReviewServerExit {
                    id: exit_id,
                    success: false,
                    code: None,
                },
            };
            let _ = app.emit("review-server-exit", payload);
        }
    });

    Ok(id)
}

fn wait_for_review_exit(process: &ReviewServerProcess, timeout: Duration) -> Result<bool, String> {
    let (exited_lock, exited_signal) = &*process.exited;
    let has_exited = exited_lock
        .lock()
        .map_err(|_| "Review server exit state is unavailable".to_string())?;
    if *has_exited {
        return Ok(true);
    }
    let (has_exited, _) = exited_signal
        .wait_timeout_while(has_exited, timeout, |exited| !*exited)
        .map_err(|_| "Review server exit state is unavailable".to_string())?;
    Ok(*has_exited)
}

fn stop_review_server_inner(id: &str) -> Result<(), String> {
    let process = review_servers()
        .lock()
        .map_err(|_| "Review server registry is unavailable".to_string())?
        .get(id)
        .cloned();
    let Some(process) = process else {
        // The waiter removes only after child.wait(), so an absent owned id is
        // already confirmed stopped.
        return Ok(());
    };

    info!("Stopping an owned review server");
    terminate_process_tree(process.pid)?;
    if wait_for_review_exit(&process, REVIEW_STOP_GRACE)? {
        return Ok(());
    }

    info!("Review server ignored termination; escalating to a force kill");
    force_terminate_process_tree(process.pid)?;
    if wait_for_review_exit(&process, REVIEW_KILL_GRACE)? {
        return Ok(());
    }

    Err("Could not confirm that the review server stopped after force termination".to_string())
}

/// Stops only a review process created by this application instance. This
/// returns only after child.wait() confirms exit, escalating from TERM to KILL.
#[tauri::command]
#[specta::specta]
pub async fn stop_review_server(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || stop_review_server_inner(&id))
        .await
        .map_err(|error| format!("Failed to wait for the review server: {error}"))?
}

pub fn stop_all_review_servers() {
    let processes = review_servers()
        .lock()
        .map(|servers| servers.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    for process in processes {
        let _ = force_terminate_process_tree(process.pid);
    }
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
    fn command_for_sets_the_requested_working_directory() {
        let project = test_project();
        let output = command_for("pwd", project.path()).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            project.path().canonicalize().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn project_validation_requires_a_package_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let error = validate_project_dir(directory.path().to_str().unwrap()).unwrap_err();
        assert!(error.contains("Astro project"));
    }

    #[tokio::test]
    async fn stop_review_server_only_accepts_owned_ids() {
        stop_review_server("not-owned".to_string()).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn start_and_stop_review_server_kills_process_group() {
        let project = test_project();
        let id = start_review_server_inner(
            None,
            "sleep 300".to_string(),
            project.path().to_string_lossy().to_string(),
        )
        .unwrap();
        let pid = review_servers().lock().unwrap().get(&id).unwrap().pid;
        assert!(ensure_no_review_server_for_project(project.path()).is_err());
        stop_review_server(id).await.unwrap();
        ensure_no_review_server_for_project(project.path()).unwrap();
        let alive = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success();
        assert!(!alive, "process group leader {pid} still alive");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_review_server_escalates_and_waits_for_term_resistant_processes() {
        let project = test_project();
        let id = start_review_server_inner(
            None,
            "trap '' TERM; while true; do sleep 1; done".to_string(),
            project.path().to_string_lossy().to_string(),
        )
        .unwrap();
        let pid = review_servers().lock().unwrap().get(&id).unwrap().pid;

        stop_review_server(id).await.unwrap();

        let alive = Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success();
        assert!(
            !alive,
            "force-killed process group leader {pid} still alive"
        );
    }
}
