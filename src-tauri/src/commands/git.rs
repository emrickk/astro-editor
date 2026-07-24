use super::image_handler::get_augmented_path;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use rustix::fd::OwnedFd;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use rustix::fs::{
    fstat, fsync, mkdirat, open, openat, readlinkat, renameat_with, statat, unlinkat, AtFlags, Dir,
    FileType, Mode, OFlags, RenameFlags,
};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use rustix::io::Errno;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::io::{BufRead, BufReader};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::fs::PermissionsExt;

const MAX_GIT_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const RECOVERY_REF_PREFIX: &str = "refs/astro-editor/pull-recovery/";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const RECOVERY_ID_HEX_LENGTH: usize = 32;
const UNSAFE_DRAFT_ATTRIBUTES: [&str; 7] = [
    "merge",
    "filter",
    "working-tree-encoding",
    "text",
    "eol",
    "ident",
    "crlf",
];

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum GitIoResult {
    Stdin(Result<(), String>),
    Stdout(Result<Vec<u8>, String>),
    Stderr(Result<Vec<u8>, String>),
}

struct TemporaryIndex {
    directory: PathBuf,
    hooks: PathBuf,
    index: PathBuf,
}

impl TemporaryIndex {
    fn new() -> Result<Self, String> {
        let directory =
            std::env::temp_dir().join(format!("astro-editor-pull-{}", Uuid::new_v4().simple()));
        fs::create_dir(&directory).map_err(|e| format!("Could not create Pull workspace: {e}"))?;
        let hooks = directory.join("hooks");
        fs::create_dir(&hooks)
            .map_err(|e| format!("Could not create empty Pull hooks directory: {e}"))?;
        Ok(Self {
            hooks,
            index: directory.join("index"),
            directory,
        })
    }
}

impl Drop for TemporaryIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[derive(Debug)]
struct TreeChange {
    status: char,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckoutState {
    branch: String,
    head: String,
    upstream_ref: String,
    upstream: String,
}

/// One retained filesystem version in a Pull recovery.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PullRecoveryFile {
    pub relative_path: String,
    pub source_tree: String,
    pub size: u32,
}

/// Metadata needed to inspect, restore, or explicitly delete a Pull recovery.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PullRecovery {
    pub id: String,
    pub status: PullRecoveryStatus,
    pub created_at: String,
    pub recovery_ref: String,
    pub snapshot: String,
    pub files: Vec<PullRecoveryFile>,
}

/// Whether a Pull recovery was cleanly sealed or needs explicit attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum PullRecoveryStatus {
    Completed,
    NeedsAttention,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug)]
struct RecoveryManifestSource {
    name: OsString,
    relative_path: String,
    source_tree: String,
    size: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug)]
struct RecoveryManifest {
    created_at: String,
    recovery_ref: String,
    snapshot: String,
    sources: Vec<RecoveryManifestSource>,
    completed: bool,
}

fn command_error(args: &[String], output: &GitOutput) -> String {
    let code = output
        .status
        .code()
        .map_or_else(|| "signal".to_string(), |code| code.to_string());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if detail.is_empty() {
        format!("git {} failed (exit {code})", args.join(" "))
    } else {
        format!("git {} failed (exit {code}): {detail}", args.join(" "))
    }
}

fn read_git_stream(mut stream: impl Read, total_bytes: &AtomicUsize) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|e| format!("Could not read git output: {e}"))?;
        if count == 0 {
            break;
        }
        let previous = total_bytes.fetch_add(count, Ordering::Relaxed);
        if previous.saturating_add(count) > MAX_GIT_OUTPUT_BYTES {
            return Err("Git output exceeded the 16 MB safety limit".to_string());
        }
        output.extend_from_slice(&buffer[..count]);
    }
    Ok(output)
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn run_git(
    root: &Path,
    args: &[String],
    environment: &[(&str, &OsStr)],
    input: Option<&[u8]>,
) -> Result<GitOutput, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .env("PATH", get_augmented_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in environment {
        command.env(key, value);
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not run git: {e}"))?;

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            stop_child(&mut child);
            return Err("Could not open git output".to_string());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            stop_child(&mut child);
            return Err("Could not open git error output".to_string());
        }
    };

    let total_bytes = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::channel();
    let stdout_sender = sender.clone();
    let stdout_bytes = Arc::clone(&total_bytes);
    thread::spawn(move || {
        let _ = stdout_sender.send(GitIoResult::Stdout(read_git_stream(stdout, &stdout_bytes)));
    });
    let stderr_sender = sender.clone();
    let stderr_bytes = Arc::clone(&total_bytes);
    thread::spawn(move || {
        let _ = stderr_sender.send(GitIoResult::Stderr(read_git_stream(stderr, &stderr_bytes)));
    });

    let mut stdin_finished = input.is_none();
    if let Some(input) = input {
        let mut stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                stop_child(&mut child);
                return Err("Could not open git input".to_string());
            }
        };
        let stdin_sender = sender.clone();
        let input = input.to_vec();
        thread::spawn(move || {
            let result = stdin
                .write_all(&input)
                .map_err(|e| format!("Could not write git input: {e}"));
            let _ = stdin_sender.send(GitIoResult::Stdin(result));
        });
    }
    drop(sender);

    let started = Instant::now();
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    loop {
        loop {
            match receiver.try_recv() {
                Ok(GitIoResult::Stdin(result)) => {
                    if let Err(error) = result {
                        stop_child(&mut child);
                        return Err(error);
                    }
                    stdin_finished = true;
                }
                Ok(GitIoResult::Stdout(result)) => match result {
                    Ok(bytes) => stdout = Some(bytes),
                    Err(error) => {
                        stop_child(&mut child);
                        return Err(error);
                    }
                },
                Ok(GitIoResult::Stderr(result)) => match result {
                    Ok(bytes) => stderr = Some(bytes),
                    Err(error) => {
                        stop_child(&mut child);
                        return Err(error);
                    }
                },
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if stdout.is_none() || stderr.is_none() || !stdin_finished {
                        stop_child(&mut child);
                        return Err("Git output reader stopped unexpectedly".to_string());
                    }
                    break;
                }
            }
        }

        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit_status)) => status = Some(exit_status),
                Ok(None) => {}
                Err(error) => {
                    stop_child(&mut child);
                    return Err(format!("Could not wait for git: {error}"));
                }
            }
        }
        if status.is_some() && stdout.is_some() && stderr.is_some() && stdin_finished {
            return Ok(GitOutput {
                status: status.take().expect("status checked above"),
                stdout: stdout.take().expect("stdout checked above"),
                stderr: stderr.take().expect("stderr checked above"),
            });
        }
        if started.elapsed() >= GIT_COMMAND_TIMEOUT {
            stop_child(&mut child);
            return Err(format!(
                "git {} timed out after {} seconds",
                args.join(" "),
                GIT_COMMAND_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let owned = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let output = run_git(root, &owned, &[], None)?;
    if !output.status.success() {
        return Err(command_error(&owned, &output));
    }
    Ok(output.stdout)
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git(root, args)?)
        .map(|text| text.trim().to_string())
        .map_err(|_| format!("git {} returned non-UTF-8 text", args.join(" ")))
}

fn ensure_safe_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("Pull found an unsafe repository path and stopped".to_string());
    }
    Ok(())
}

fn worktree_path_without_symlink_parents(root: &Path, relative: &str) -> Result<PathBuf, String> {
    ensure_safe_path(relative)?;
    let relative_path = Path::new(relative);
    let components = relative_path.components().collect::<Vec<_>>();
    let mut current = root.to_path_buf();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Pull stopped because a parent of {relative} is a symbolic link"
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "Pull stopped because a parent of {relative} is not a directory"
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!("Could not inspect a parent of {relative}: {error}"));
            }
        }
    }
    Ok(root.join(relative_path))
}

fn parse_tree_changes(bytes: &[u8]) -> Result<Vec<TreeChange>, String> {
    let fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() % 2 != 0 {
        return Err("Git returned an invalid change list".to_string());
    }

    let mut changes = Vec::with_capacity(fields.len() / 2);
    for pair in fields.chunks_exact(2) {
        let status_text = std::str::from_utf8(pair[0])
            .map_err(|_| "Git returned an invalid change status".to_string())?;
        let status = status_text
            .chars()
            .next()
            .ok_or_else(|| "Git returned an empty change status".to_string())?;
        if !matches!(status, 'A' | 'D' | 'M' | 'T') {
            return Err(format!(
                "Pull does not support repository change status {status_text}"
            ));
        }
        let path = std::str::from_utf8(pair[1])
            .map_err(|_| "Pull does not support non-UTF-8 file names".to_string())?
            .to_string();
        ensure_safe_path(&path)?;
        changes.push(TreeChange { status, path });
    }
    Ok(changes)
}

fn tree_changes(root: &Path, from: &str, to: &str) -> Result<Vec<TreeChange>, String> {
    let args = vec![
        "diff".to_string(),
        "--name-status".to_string(),
        "-z".to_string(),
        "--no-renames".to_string(),
        from.to_string(),
        to.to_string(),
        "--".to_string(),
    ];
    let output = run_git(root, &args, &[], None)?;
    if !output.status.success() {
        return Err(command_error(&args, &output));
    }
    parse_tree_changes(&output.stdout)
}

#[cfg(test)]
fn remove_worktree_path(root: &Path, relative: &str) -> Result<(), String> {
    let path = worktree_path_without_symlink_parents(root, relative)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Could not inspect {relative}: {error}")),
    };

    if metadata.file_type().is_dir() {
        fs::remove_dir(&path).map_err(|_| {
            format!("Pull stopped because {relative} contains files Git does not track")
        })?;
    } else {
        fs::remove_file(&path).map_err(|error| format!("Could not remove {relative}: {error}"))?;
    }
    Ok(())
}

fn workspace_path_matches_tree(
    root: &Path,
    index: &Path,
    tree: &str,
    relative: &str,
    workspace: &Path,
) -> Result<bool, String> {
    let expected_tree = git_text(root, &["rev-parse", &format!("{tree}^{{tree}}")])?;
    if index.exists() {
        fs::remove_file(index).map_err(|e| format!("Could not reset Pull workspace: {e}"))?;
    }
    let mut environment = temporary_index_environment(index).to_vec();
    environment.push(("GIT_WORK_TREE", workspace.as_os_str()));
    let read_args = vec!["read-tree".to_string(), expected_tree.clone()];
    let read = run_git(root, &read_args, &environment, None)?;
    if !read.status.success() {
        return Err(command_error(&read_args, &read));
    }

    let live_path = workspace.join(relative);
    match fs::symlink_metadata(live_path) {
        Ok(_) => {
            let add_args = vec![
                "add".to_string(),
                "-A".to_string(),
                "-f".to_string(),
                "--".to_string(),
                relative.to_string(),
            ];
            let add = run_git(root, &add_args, &environment, None)?;
            if !add.status.success() {
                return Err(command_error(&add_args, &add));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let remove_args = vec![
                "update-index".to_string(),
                "--force-remove".to_string(),
                "--".to_string(),
                relative.to_string(),
            ];
            let remove = run_git(root, &remove_args, &environment, None)?;
            if !remove.status.success() {
                return Err(command_error(&remove_args, &remove));
            }
        }
        Err(error) => return Err(format!("Could not inspect {relative}: {error}")),
    }

    let write_args = vec!["write-tree".to_string()];
    let write = run_git(root, &write_args, &environment, None)?;
    if !write.status.success() {
        return Err(command_error(&write_args, &write));
    }
    let live_tree = String::from_utf8(write.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| "Git returned an invalid working-tree id".to_string())?;
    Ok(live_tree == expected_tree)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeEntryKind {
    File,
    Symlink,
    Tree,
    Gitlink,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug)]
struct QuarantinedSource {
    name: OsString,
    source_tree: String,
    path: String,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct PullQuarantine {
    base_fd: OwnedFd,
    directory_fd: OwnedFd,
    directory_name: OsString,
    path: PathBuf,
    manifest: fs::File,
    sources: Vec<QuarantinedSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QuarantineDisposition {
    Removed,
    Retained(String),
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl PullQuarantine {
    #[cfg(test)]
    fn new(root: &Path) -> Result<Self, String> {
        Self::new_internal(root, None)
    }

    fn new_with_recovery(root: &Path, recovery_ref: &str, snapshot: &str) -> Result<Self, String> {
        Self::new_internal(root, Some((recovery_ref, snapshot)))
    }

    fn new_internal(root: &Path, recovery: Option<(&str, &str)>) -> Result<Self, String> {
        let git_directory = PathBuf::from(git_text(root, &["rev-parse", "--absolute-git-dir"])?)
            .canonicalize()
            .map_err(|error| format!("Could not locate the Git directory: {error}"))?;
        let git_fd = open(&git_directory, directory_open_flags(), Mode::empty())
            .map_err(|error| format!("Could not safely open the Git directory: {error}"))?;
        let astro_editor_fd = open_or_create_directory(&git_fd, OsStr::new("astro-editor"))?;
        let base_fd = open_or_create_directory(&astro_editor_fd, OsStr::new("pull-quarantine"))?;
        let directory_name = OsString::from(format!("pull-{}", Uuid::new_v4().simple()));
        mkdirat(&base_fd, &directory_name, Mode::from_raw_mode(0o700))
            .map_err(|error| format!("Could not create the Pull quarantine directory: {error}"))?;
        sync_directory(&base_fd, "the Pull quarantine directory")?;
        let directory_fd = openat(
            &base_fd,
            &directory_name,
            directory_open_flags(),
            Mode::empty(),
        )
        .map_err(|error| format!("Could not open the Pull quarantine directory: {error}"))?;
        let manifest_fd = openat(
            &directory_fd,
            OsStr::new("manifest.jsonl"),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| format!("Could not create the Pull recovery manifest: {error}"))?;
        let manifest = fs::File::from(manifest_fd);
        let root_fd = open(root, directory_open_flags(), Mode::empty())
            .map_err(|error| format!("Could not safely open the repository root: {error}"))?;
        if fstat(&root_fd)
            .map_err(|error| format!("Could not inspect the repository: {error}"))?
            .st_dev
            != fstat(&directory_fd)
                .map_err(|error| format!("Could not inspect the Pull quarantine: {error}"))?
                .st_dev
        {
            return Err(
                "Pull cannot safely reconcile drafts because the Git directory is on another filesystem"
                    .to_string(),
            );
        }

        let path = git_directory
            .join("astro-editor")
            .join("pull-quarantine")
            .join(&directory_name);
        let mut quarantine = Self {
            base_fd,
            directory_fd,
            directory_name,
            path,
            manifest,
            sources: Vec::new(),
        };
        let (recovery_ref, snapshot) = recovery.unwrap_or(("", ""));
        quarantine.append_manifest(serde_json::json!({
            "type": "header",
            "version": 1,
            "createdAt": chrono::Utc::now().to_rfc3339(),
            "recoveryRef": recovery_ref,
            "snapshot": snapshot,
        }))?;
        sync_directory(&quarantine.directory_fd, "the Pull recovery manifest")?;
        quarantine.probe_no_replace()?;
        Ok(quarantine)
    }

    fn location(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    fn unique_name(&self, prefix: &str) -> OsString {
        OsString::from(format!("{prefix}-{}", Uuid::new_v4().simple()))
    }

    fn append_manifest(&mut self, entry: serde_json::Value) -> Result<(), String> {
        serde_json::to_writer(&mut self.manifest, &entry)
            .map_err(|error| format!("Could not write the Pull recovery manifest: {error}"))?;
        self.manifest
            .write_all(b"\n")
            .and_then(|_| self.manifest.sync_all())
            .map_err(|error| format!("Could not save the Pull recovery manifest: {error}"))
    }

    fn prepare_source(
        &mut self,
        name: &OsStr,
        source_tree: &str,
        path: &str,
    ) -> Result<(), String> {
        self.append_manifest(serde_json::json!({
            "type": "source",
            "phase": "prepared",
            "quarantineName": name.to_string_lossy(),
            "relativePath": path,
            "sourceTree": source_tree,
        }))
    }

    fn probe_no_replace(&self) -> Result<(), String> {
        let source = self.unique_name("probe-source");
        let target = self.unique_name("probe-target");
        let _probe = openat(
            &self.directory_fd,
            &source,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| format!("Could not prepare the Pull safety probe: {error}"))?;
        renameat_with(
            &self.directory_fd,
            &source,
            &self.directory_fd,
            &target,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            format!("This filesystem does not support safe Pull replacement: {error}")
        })?;
        unlinkat(&self.directory_fd, &target, AtFlags::empty())
            .map_err(|error| format!("Could not finish the Pull safety probe: {error}"))?;
        sync_directory(&self.directory_fd, "the Pull safety probe")?;
        Ok(())
    }

    fn create_workspace(&self, prefix: &str) -> Result<(OsString, OwnedFd, PathBuf), String> {
        let name = self.unique_name(prefix);
        mkdirat(&self.directory_fd, &name, Mode::from_raw_mode(0o700))
            .map_err(|error| format!("Could not create a Pull staging directory: {error}"))?;
        let fd = openat(
            &self.directory_fd,
            &name,
            directory_open_flags(),
            Mode::empty(),
        )
        .map_err(|error| format!("Could not open a Pull staging directory: {error}"))?;
        Ok((name.clone(), fd, self.path.join(name)))
    }

    fn register_source(&mut self, name: OsString, source_tree: &str, path: &str) {
        self.sources.push(QuarantinedSource {
            name,
            source_tree: source_tree.to_string(),
            path: path.to_string(),
        });
    }

    fn finalize(&mut self, root: &Path, index: &Path) -> Result<QuarantineDisposition, String> {
        for source in &self.sources {
            if !quarantined_path_matches_tree(
                root,
                index,
                &source.source_tree,
                &source.path,
                self,
                Some(&source.name),
            )? {
                return Err(format!(
                    "a quarantined copy of {} received a later write",
                    source.path
                ));
            }
        }
        if self.sources.is_empty() {
            remove_directory_contents(&self.directory_fd)?;
            sync_directory(&self.directory_fd, "the empty Pull quarantine")?;
            unlinkat(&self.base_fd, &self.directory_name, AtFlags::REMOVEDIR)
                .map_err(|error| format!("Could not remove the Pull quarantine: {error}"))?;
            sync_directory(&self.base_fd, "the removed Pull quarantine")?;
            return Ok(QuarantineDisposition::Removed);
        }

        self.append_manifest(serde_json::json!({
            "type": "state",
            "phase": "completed",
        }))?;
        let source_names = self
            .sources
            .iter()
            .map(|source| source.name.clone())
            .collect::<BTreeSet<_>>();
        for entry in directory_entries(&self.directory_fd)? {
            if entry != OsStr::new("manifest.jsonl") && !source_names.contains(&entry) {
                remove_entry_tree(&self.directory_fd, &entry)?;
            }
        }
        sync_directory(&self.directory_fd, "the sealed Pull recovery data")?;

        let completed_name = OsString::from(format!("completed-{}", Uuid::new_v4().simple()));
        renameat_with(
            &self.base_fd,
            &self.directory_name,
            &self.base_fd,
            &completed_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| format!("Could not seal the Pull recovery data: {error}"))?;
        sync_directory(&self.base_fd, "the sealed Pull recovery directory")?;
        self.directory_name = completed_name.clone();
        self.path.set_file_name(completed_name);
        Ok(QuarantineDisposition::Retained(self.location()))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn directory_open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn sync_directory(directory: &OwnedFd, context: &str) -> Result<(), String> {
    fsync(directory).map_err(|error| format!("Could not save {context}: {error}"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_or_create_directory(parent: &OwnedFd, name: &OsStr) -> Result<OwnedFd, String> {
    let created = match mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) => true,
        Err(Errno::EXIST) => false,
        Err(error) => {
            return Err(format!(
                "Could not create a Pull support directory: {error}"
            ));
        }
    };
    if created {
        sync_directory(parent, "the Pull support directory")?;
    }
    openat(parent, name, directory_open_flags(), Mode::empty())
        .map_err(|error| format!("Could not safely open a Pull support directory: {error}"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_parent_directory(
    root: &OwnedFd,
    relative: &str,
    create: bool,
) -> Result<Option<(OwnedFd, OsString)>, String> {
    ensure_safe_path(relative)?;
    let components = Path::new(relative)
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect::<Vec<_>>();
    let leaf = components
        .last()
        .cloned()
        .ok_or_else(|| "Pull found an empty repository path".to_string())?;
    let mut current = openat(root, OsStr::new("."), directory_open_flags(), Mode::empty())
        .map_err(|error| format!("Could not open the repository root: {error}"))?;
    for component in components.iter().take(components.len() - 1) {
        match openat(&current, component, directory_open_flags(), Mode::empty()) {
            Ok(next) => current = next,
            Err(Errno::NOENT) if !create => return Ok(None),
            Err(Errno::NOENT) => {
                let created = match mkdirat(&current, component, Mode::from_raw_mode(0o755)) {
                    Ok(()) => true,
                    Err(Errno::EXIST) => false,
                    Err(error) => {
                        return Err(format!(
                            "Could not create a parent directory of {relative}: {error}"
                        ));
                    }
                };
                if created {
                    sync_directory(&current, "a live Pull parent directory")?;
                }
                current = openat(&current, component, directory_open_flags(), Mode::empty())
                    .map_err(|error| {
                        format!("Could not safely open a parent of {relative}: {error}")
                    })?;
            }
            Err(error) => {
                return Err(format!(
                    "Pull stopped because a parent of {relative} is not a safe directory: {error}"
                ));
            }
        }
    }
    Ok(Some((current, leaf)))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn revalidate_parent_binding(
    root: &OwnedFd,
    relative: &str,
    expected_parent: &OwnedFd,
) -> Result<(OwnedFd, OsString), String> {
    let (current_parent, leaf) = open_parent_directory(root, relative, false)?
        .ok_or_else(|| format!("Pull stopped because a parent of {relative} disappeared"))?;
    let expected = fstat(expected_parent)
        .map_err(|error| format!("Could not inspect a parent of {relative}: {error}"))?;
    let current = fstat(&current_parent)
        .map_err(|error| format!("Could not inspect a parent of {relative}: {error}"))?;
    if expected.st_dev != current.st_dev || expected.st_ino != current.st_ino {
        return Err(format!(
            "Pull stopped because a parent of {relative} changed during reconciliation"
        ));
    }
    Ok((current_parent, leaf))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn restore_quarantined_source(
    root: &OwnedFd,
    relative: &str,
    expected_parent: &OwnedFd,
    quarantine: &PullQuarantine,
    name: &OsStr,
) -> Result<(), String> {
    let (current_parent, leaf) = revalidate_parent_binding(root, relative, expected_parent)?;
    renameat_with(
        &quarantine.directory_fd,
        name,
        &current_parent,
        &leaf,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| format!("Could not restore {relative} safely: {error}"))?;
    sync_directory(&current_parent, "the restored live Pull source")?;
    sync_directory(
        &quarantine.directory_fd,
        "the restored Pull recovery source",
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn restore_registered_target(
    root: &Path,
    root_fd: &OwnedFd,
    index: &Path,
    target_tree: &str,
    relative: &str,
    quarantine: &mut PullQuarantine,
) -> Result<bool, String> {
    if let Some((parent, leaf)) = open_parent_directory(root_fd, relative, false)? {
        match statat(&parent, &leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => return Ok(false),
            Err(Errno::NOENT) => {}
            Err(error) => {
                return Err(format!("Could not inspect {relative} safely: {error}"));
            }
        }
    }
    let Some(source_index) = quarantine
        .sources
        .iter()
        .rposition(|source| source.source_tree == target_tree && source.path == relative)
    else {
        return Ok(false);
    };
    let source_name = quarantine.sources[source_index].name.clone();
    if !quarantined_path_matches_tree(
        root,
        index,
        target_tree,
        relative,
        quarantine,
        Some(&source_name),
    )? {
        return Err(format!(
            "Pull stopped because the recovery copy of {relative} received a later write"
        ));
    }

    let (parent, leaf) = open_parent_directory(root_fd, relative, true)?
        .ok_or_else(|| format!("Could not open a parent of {relative}"))?;
    if fstat(&parent)
        .map_err(|error| format!("Could not inspect a parent of {relative}: {error}"))?
        .st_dev
        != fstat(&quarantine.directory_fd)
            .map_err(|error| format!("Could not inspect the Pull quarantine: {error}"))?
            .st_dev
    {
        return Err(format!(
            "Pull cannot safely restore {relative} across filesystem boundaries"
        ));
    }
    renameat_with(
        &quarantine.directory_fd,
        &source_name,
        &parent,
        &leaf,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        format!("Pull stopped before overwriting a later version of {relative}: {error}")
    })?;
    quarantine.sources.remove(source_index);
    sync_directory(&parent, "the restored live Pull target")?;
    sync_directory(
        &quarantine.directory_fd,
        "the restored Pull recovery target",
    )?;
    Ok(true)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn directory_entries(directory: &OwnedFd) -> Result<Vec<OsString>, String> {
    let mut entries = Vec::new();
    let iterator = Dir::read_from(directory)
        .map_err(|error| format!("Could not inspect a quarantined directory: {error}"))?;
    for entry in iterator {
        let entry =
            entry.map_err(|error| format!("Could not inspect a quarantined directory: {error}"))?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            entries.push(OsString::from_vec(name.to_vec()));
        }
    }
    Ok(entries)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn remove_entry_tree(parent: &OwnedFd, name: &OsStr) -> Result<(), String> {
    let metadata = match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => return Err(format!("Could not inspect Pull safety data: {error}")),
    };
    if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
        let directory = openat(parent, name, directory_open_flags(), Mode::empty())
            .map_err(|error| format!("Could not open Pull safety data: {error}"))?;
        remove_directory_contents(&directory)?;
        unlinkat(parent, name, AtFlags::REMOVEDIR)
            .map_err(|error| format!("Could not remove Pull safety data: {error}"))?;
    } else {
        unlinkat(parent, name, AtFlags::empty())
            .map_err(|error| format!("Could not remove Pull safety data: {error}"))?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn remove_directory_contents(directory: &OwnedFd) -> Result<(), String> {
    for entry in directory_entries(directory)? {
        remove_entry_tree(directory, &entry)?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn copy_entry_from_fd(parent: &OwnedFd, name: &OsStr, destination: &Path) -> Result<(), String> {
    let metadata = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| format!("Could not inspect quarantined Pull data: {error}"))?;
    match FileType::from_raw_mode(metadata.st_mode) {
        FileType::RegularFile => {
            if let Some(destination_parent) = destination.parent() {
                fs::create_dir_all(destination_parent).map_err(|error| {
                    format!("Could not create a Pull validation directory: {error}")
                })?;
            }
            let source = openat(
                parent,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| format!("Could not open quarantined Pull data: {error}"))?;
            let mut source = fs::File::from(source);
            let mut target = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(|error| format!("Could not copy Pull validation data: {error}"))?;
            std::io::copy(&mut source, &mut target)
                .map_err(|error| format!("Could not copy Pull validation data: {error}"))?;
            fs::set_permissions(
                destination,
                fs::Permissions::from_mode((metadata.st_mode as u32) & 0o777),
            )
            .map_err(|error| format!("Could not preserve Pull file permissions: {error}"))?;
        }
        FileType::Directory => {
            fs::create_dir(destination)
                .map_err(|error| format!("Could not copy a Pull directory: {error}"))?;
            let directory = openat(parent, name, directory_open_flags(), Mode::empty())
                .map_err(|error| format!("Could not open a quarantined directory: {error}"))?;
            for entry in directory_entries(&directory)? {
                copy_entry_from_fd(&directory, &entry, &destination.join(&entry))?;
            }
        }
        FileType::Symlink => {
            if let Some(destination_parent) = destination.parent() {
                fs::create_dir_all(destination_parent).map_err(|error| {
                    format!("Could not create a Pull validation directory: {error}")
                })?;
            }
            let target = readlinkat(parent, name, Vec::new())
                .map_err(|error| format!("Could not read quarantined symbolic link: {error}"))?;
            std::os::unix::fs::symlink(OsStr::from_bytes(target.to_bytes()), destination)
                .map_err(|error| format!("Could not copy a Pull symbolic link: {error}"))?;
        }
        _ => {
            return Err("Pull does not support this filesystem object type".to_string());
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn quarantined_path_matches_tree(
    root: &Path,
    index: &Path,
    tree: &str,
    relative: &str,
    quarantine: &PullQuarantine,
    entry: Option<&OsStr>,
) -> Result<bool, String> {
    let (workspace_name, _workspace_fd, workspace_path) =
        quarantine.create_workspace("validate")?;
    let result = (|| {
        if let Some(entry) = entry {
            copy_entry_from_fd(
                &quarantine.directory_fd,
                entry,
                &workspace_path.join(relative),
            )?;
        }
        workspace_path_matches_tree(root, index, tree, relative, &workspace_path)
    })();
    let cleanup = remove_entry_tree(&quarantine.directory_fd, &workspace_name);
    match (result, cleanup) {
        (Ok(matches), Ok(())) => Ok(matches),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn live_path_matches_tree(
    root: &Path,
    root_fd: &OwnedFd,
    index: &Path,
    tree: &str,
    relative: &str,
    quarantine: &PullQuarantine,
) -> Result<bool, String> {
    let (workspace_name, _workspace_fd, workspace_path) =
        quarantine.create_workspace("live-check")?;
    let result = (|| {
        if let Some((parent, leaf)) = open_parent_directory(root_fd, relative, false)? {
            match statat(&parent, &leaf, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => copy_entry_from_fd(&parent, &leaf, &workspace_path.join(relative))?,
                Err(Errno::NOENT) => {}
                Err(error) => {
                    return Err(format!("Could not inspect {relative} safely: {error}"));
                }
            }
        }
        workspace_path_matches_tree(root, index, tree, relative, &workspace_path)
    })();
    let cleanup = remove_entry_tree(&quarantine.directory_fd, &workspace_name);
    match (result, cleanup) {
        (Ok(matches), Ok(())) => Ok(matches),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn tree_entry_kind(
    root: &Path,
    tree: &str,
    relative: &str,
) -> Result<Option<TreeEntryKind>, String> {
    let output = git(root, &["ls-tree", "-z", tree, "--", relative])?;
    let mut records = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    let Some(record) = records.next() else {
        return Ok(None);
    };
    if records.next().is_some() {
        return Err(format!("Git returned multiple entries for {relative}"));
    }
    let tab = record
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or_else(|| "Git returned an invalid tree entry".to_string())?;
    let metadata = std::str::from_utf8(&record[..tab])
        .map_err(|_| "Git returned an invalid tree entry".to_string())?;
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3 {
        return Err("Git returned an invalid tree entry".to_string());
    }
    match (fields[0], fields[1]) {
        (_, "tree") => Ok(Some(TreeEntryKind::Tree)),
        ("120000", "blob") => Ok(Some(TreeEntryKind::Symlink)),
        (_, "blob") => Ok(Some(TreeEntryKind::File)),
        (_, "commit") => Ok(Some(TreeEntryKind::Gitlink)),
        _ => Err(format!(
            "Pull does not support the tree entry at {relative}"
        )),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_same_or_descendant(path: &str, ancestor: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn transition_paths(
    root: &Path,
    from: &str,
    to: &str,
    changes: &[TreeChange],
) -> Result<Vec<String>, String> {
    let mut candidates = BTreeSet::new();
    for change in changes {
        let mut path = PathBuf::new();
        for component in Path::new(&change.path).components() {
            path.push(component.as_os_str());
            candidates.insert(path.to_string_lossy().into_owned());
        }
    }

    let mut structural = Vec::new();
    for candidate in candidates {
        let from_kind = tree_entry_kind(root, from, &candidate)?;
        let to_kind = tree_entry_kind(root, to, &candidate)?;
        if from_kind == Some(TreeEntryKind::Gitlink) || to_kind == Some(TreeEntryKind::Gitlink) {
            return Err(format!(
                "Pull does not support changing the submodule path {candidate}"
            ));
        }
        let boundary = matches!(
            (from_kind, to_kind),
            (Some(TreeEntryKind::Tree), Some(kind)) if kind != TreeEntryKind::Tree
        ) || matches!(
            (from_kind, to_kind),
            (Some(kind), Some(TreeEntryKind::Tree)) if kind != TreeEntryKind::Tree
        );
        if boundary {
            structural.push(candidate);
        }
    }
    structural.sort_by_key(|path| Path::new(path).components().count());
    let mut boundaries: Vec<String> = Vec::new();
    for path in structural {
        if !boundaries
            .iter()
            .any(|ancestor| is_same_or_descendant(&path, ancestor))
        {
            boundaries.push(path);
        }
    }

    let mut paths = boundaries.clone();
    for change in changes {
        if !boundaries
            .iter()
            .any(|boundary| is_same_or_descendant(&change.path, boundary))
        {
            paths.push(change.path.clone());
        }
    }
    paths.sort();
    paths.dedup();
    let mut ordered_paths = Vec::with_capacity(paths.len());
    for path in paths {
        let deleted = tree_entry_kind(root, to, &path)?.is_none();
        ordered_paths.push((path, deleted));
    }
    ordered_paths.sort_by(|(left, left_deleted), (right, right_deleted)| {
        left_deleted
            .cmp(right_deleted)
            .reverse()
            .then_with(|| {
                let left_depth = Path::new(left).components().count();
                let right_depth = Path::new(right).components().count();
                if *left_deleted {
                    right_depth.cmp(&left_depth)
                } else {
                    left_depth.cmp(&right_depth)
                }
            })
            .then_with(|| left.cmp(right))
    });
    Ok(ordered_paths
        .into_iter()
        .map(|(path, _deleted)| path)
        .collect())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn prepare_target_entry(
    root: &Path,
    to: &str,
    relative: &str,
    quarantine: &PullQuarantine,
) -> Result<OsString, String> {
    let (workspace_name, workspace_fd, workspace_path) = quarantine.create_workspace("stage")?;
    let args = vec![
        "restore".to_string(),
        "--worktree".to_string(),
        format!("--source={to}"),
        "--".to_string(),
        relative.to_string(),
    ];
    let output = run_git(
        root,
        &args,
        &[("GIT_WORK_TREE", workspace_path.as_os_str())],
        None,
    )?;
    if !output.status.success() {
        return Err(command_error(&args, &output));
    }
    let (staged_parent, leaf) = open_parent_directory(&workspace_fd, relative, false)?
        .ok_or_else(|| format!("Git did not stage {relative} for Pull"))?;
    let staged_name = quarantine.unique_name("target");
    renameat_with(
        &staged_parent,
        &leaf,
        &quarantine.directory_fd,
        &staged_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| format!("Could not quarantine the staged version of {relative}: {error}"))?;
    remove_entry_tree(&quarantine.directory_fd, &workspace_name)?;
    Ok(staged_name)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionHookPoint {
    ParentOpened,
    Quarantined,
    TargetPrepared,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
fn transition_worktree_path(
    root: &Path,
    root_fd: &OwnedFd,
    index: &Path,
    from: &str,
    to: &str,
    relative: &str,
    quarantine: &mut PullQuarantine,
    hook: &mut dyn FnMut(TransitionHookPoint, &str),
) -> Result<(), String> {
    let from_kind = tree_entry_kind(root, from, relative)?;
    let to_kind = tree_entry_kind(root, to, relative)?;
    if from_kind == Some(TreeEntryKind::Gitlink) || to_kind == Some(TreeEntryKind::Gitlink) {
        return Err(format!(
            "Pull does not support changing submodule path {relative}"
        ));
    }

    // A failed multi-path transition can leave some entries at the target
    // tree already. Treat those entries as complete so rollback is idempotent
    // and can safely resume from partial progress.
    if live_path_matches_tree(root, root_fd, index, to, relative, quarantine)? {
        return Ok(());
    }
    if restore_registered_target(root, root_fd, index, to, relative, quarantine)? {
        return Ok(());
    }

    let staged_name = if to_kind.is_some() {
        Some(prepare_target_entry(root, to, relative, quarantine)?)
    } else {
        None
    };

    let mut live_parent = open_parent_directory(root_fd, relative, false)?;
    if live_parent.is_some() {
        hook(TransitionHookPoint::ParentOpened, relative);
    }
    if let Some((parent, _)) = live_parent.as_ref() {
        live_parent = Some(revalidate_parent_binding(root_fd, relative, parent)?);
    }
    let quarantined_name = if let Some((parent, leaf)) = live_parent.as_ref() {
        if fstat(parent)
            .map_err(|error| format!("Could not inspect a parent of {relative}: {error}"))?
            .st_dev
            != fstat(&quarantine.directory_fd)
                .map_err(|error| format!("Could not inspect the Pull quarantine: {error}"))?
                .st_dev
        {
            return Err(format!(
                "Pull cannot safely reconcile {relative} across filesystem boundaries"
            ));
        }
        let name = quarantine.unique_name("source");
        quarantine.prepare_source(&name, from, relative)?;
        match renameat_with(
            parent,
            leaf,
            &quarantine.directory_fd,
            &name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                if let Err(error) = sync_directory(parent, "the live Pull source").and_then(|_| {
                    sync_directory(&quarantine.directory_fd, "the Pull recovery source")
                }) {
                    quarantine.register_source(name.clone(), from, relative);
                    return Err(format!(
                        "Pull preserved {relative} but could not durably record the move: {error}"
                    ));
                }
                hook(TransitionHookPoint::Quarantined, relative);
                Some(name)
            }
            Err(Errno::NOENT) => None,
            Err(error) => {
                return Err(format!("Could not quarantine {relative} safely: {error}"));
            }
        }
    } else {
        None
    };

    let matches = match quarantined_path_matches_tree(
        root,
        index,
        from,
        relative,
        quarantine,
        quarantined_name.as_deref(),
    ) {
        Ok(matches) => matches,
        Err(validation_error) => {
            if let (Some(name), Some((parent, _))) =
                (quarantined_name.as_ref(), live_parent.as_ref())
            {
                if let Err(restore_error) =
                    restore_quarantined_source(root_fd, relative, parent, quarantine, name)
                {
                    quarantine.register_source(name.clone(), from, relative);
                    return Err(format!(
                        "{validation_error}. The original {relative} remains in {} because it could not be restored safely: {restore_error}",
                        quarantine.location()
                    ));
                }
            }
            return Err(validation_error);
        }
    };
    if !matches {
        if let (Some(name), Some((parent, _))) = (quarantined_name.as_ref(), live_parent.as_ref()) {
            if let Err(error) =
                restore_quarantined_source(root_fd, relative, parent, quarantine, name)
            {
                quarantine.register_source(name.clone(), from, relative);
                return Err(format!(
                    "Pull stopped because {relative} changed; both versions are preserved at the live path and {} ({error})",
                    quarantine.location()
                ));
            }
        }
        return Err(format!(
            "Pull stopped because {relative} changed during checkout reconciliation"
        ));
    }

    if let Some(name) = quarantined_name.as_ref() {
        quarantine.register_source(name.clone(), from, relative);
    }
    let Some(staged_name) = staged_name else {
        return Ok(());
    };

    hook(TransitionHookPoint::TargetPrepared, relative);
    if live_parent.is_none() {
        live_parent = open_parent_directory(root_fd, relative, true)?;
        hook(TransitionHookPoint::ParentOpened, relative);
    }
    let (bound_parent, _) = live_parent
        .as_ref()
        .ok_or_else(|| format!("Could not open a parent of {relative}"))?;
    let (parent, leaf) = revalidate_parent_binding(root_fd, relative, bound_parent)?;
    if fstat(&parent)
        .map_err(|error| format!("Could not inspect a parent of {relative}: {error}"))?
        .st_dev
        != fstat(&quarantine.directory_fd)
            .map_err(|error| format!("Could not inspect the Pull quarantine: {error}"))?
            .st_dev
    {
        return Err(format!(
            "Pull cannot safely restore {relative} across filesystem boundaries"
        ));
    }
    renameat_with(
        &quarantine.directory_fd,
        &staged_name,
        &parent,
        &leaf,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        format!("Pull stopped before overwriting a later version of {relative}: {error}")
    })?;
    sync_directory(&parent, "the installed live Pull target")?;
    sync_directory(
        &quarantine.directory_fd,
        "the installed Pull recovery target",
    )?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn materialize_tree_delta_with_hook(
    root: &Path,
    from: &str,
    to: &str,
    temporary_index: &Path,
    quarantine: &mut PullQuarantine,
    hook: &mut dyn FnMut(TransitionHookPoint, &str),
) -> Result<usize, String> {
    let changes = tree_changes(root, from, to)?;
    let paths = transition_paths(root, from, to, &changes)?;
    let root_fd = open(root, directory_open_flags(), Mode::empty())
        .map_err(|error| format!("Could not safely open the repository root: {error}"))?;
    for path in paths {
        transition_worktree_path(
            root,
            &root_fd,
            temporary_index,
            from,
            to,
            &path,
            quarantine,
            hook,
        )?;
    }
    Ok(changes.len())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn materialize_tree_delta_in_quarantine(
    root: &Path,
    from: &str,
    to: &str,
    temporary_index: &Path,
    quarantine: &mut PullQuarantine,
) -> Result<usize, String> {
    materialize_tree_delta_with_hook(root, from, to, temporary_index, quarantine, &mut |_, _| {})
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg(test)]
fn materialize_tree_delta(
    root: &Path,
    from: &str,
    to: &str,
    temporary_index: &Path,
) -> Result<usize, String> {
    let mut quarantine = PullQuarantine::new(root)?;
    let result =
        materialize_tree_delta_in_quarantine(root, from, to, temporary_index, &mut quarantine);
    match result {
        Ok(count) => {
            let _ = quarantine.finalize(root, temporary_index)?;
            Ok(count)
        }
        Err(error) => Err(format!("{error}. Quarantine: {}", quarantine.location())),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
struct PullQuarantine;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl PullQuarantine {
    #[cfg(test)]
    fn new(_root: &Path) -> Result<Self, String> {
        Err("Safe Pull draft reconciliation is unavailable on this platform".to_string())
    }

    fn new_with_recovery(
        _root: &Path,
        _recovery_ref: &str,
        _snapshot: &str,
    ) -> Result<Self, String> {
        Err("Safe Pull draft reconciliation is unavailable on this platform".to_string())
    }

    fn location(&self) -> String {
        "unavailable".to_string()
    }

    fn finalize(&mut self, _root: &Path, _index: &Path) -> Result<QuarantineDisposition, String> {
        Ok(QuarantineDisposition::Removed)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn materialize_tree_delta_in_quarantine(
    _root: &Path,
    _from: &str,
    _to: &str,
    _temporary_index: &Path,
    _quarantine: &mut PullQuarantine,
) -> Result<usize, String> {
    Err("Safe Pull draft reconciliation is unavailable on this platform".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[cfg(test)]
fn materialize_tree_delta(
    _root: &Path,
    _from: &str,
    _to: &str,
    _temporary_index: &Path,
) -> Result<usize, String> {
    Err("Safe Pull draft reconciliation is unavailable on this platform".to_string())
}

fn temporary_index_environment(index: &Path) -> [(&str, &OsStr); 5] {
    [
        ("GIT_INDEX_FILE", index.as_os_str()),
        (
            "GIT_AUTHOR_NAME",
            OsStr::new("Nevertheless Editor Recovery"),
        ),
        ("GIT_AUTHOR_EMAIL", OsStr::new("recovery@localhost")),
        (
            "GIT_COMMITTER_NAME",
            OsStr::new("Nevertheless Editor Recovery"),
        ),
        ("GIT_COMMITTER_EMAIL", OsStr::new("recovery@localhost")),
    ]
}

fn working_tree(root: &Path, index: &Path, base: &str) -> Result<String, String> {
    if index.exists() {
        fs::remove_file(index).map_err(|e| format!("Could not reset Pull workspace: {e}"))?;
    }
    let environment = temporary_index_environment(index);
    let read_args = vec!["read-tree".to_string(), base.to_string()];
    let read = run_git(root, &read_args, &environment, None)?;
    if !read.status.success() {
        return Err(command_error(&read_args, &read));
    }
    let add_args = vec![
        "add".to_string(),
        "-A".to_string(),
        "--".to_string(),
        ".".to_string(),
    ];
    let add = run_git(root, &add_args, &environment, None)?;
    if !add.status.success() {
        return Err(command_error(&add_args, &add));
    }
    let write_args = vec!["write-tree".to_string()];
    let write = run_git(root, &write_args, &environment, None)?;
    if !write.status.success() {
        return Err(command_error(&write_args, &write));
    }
    String::from_utf8(write.stdout)
        .map(|tree| tree.trim().to_string())
        .map_err(|_| "Git returned an invalid working-tree id".to_string())
}

fn synthetic_commit(root: &Path, index: &Path, tree: &str, parent: &str) -> Result<String, String> {
    let args = vec![
        "commit-tree".to_string(),
        tree.to_string(),
        "-p".to_string(),
        parent.to_string(),
        "-m".to_string(),
        "Nevertheless Editor safe Pull recovery".to_string(),
    ];
    let environment = temporary_index_environment(index);
    let output = run_git(root, &args, &environment, None)?;
    if !output.status.success() {
        return Err(command_error(&args, &output));
    }
    String::from_utf8(output.stdout)
        .map(|commit| commit.trim().to_string())
        .map_err(|_| "Git returned an invalid recovery commit id".to_string())
}

fn update_ref(root: &Path, reference: &str, value: &str) -> Result<(), String> {
    git(root, &["update-ref", reference, value]).map(|_| ())
}

fn delete_ref(root: &Path, reference: &str, expected: &str) -> Result<(), String> {
    git(root, &["update-ref", "-d", reference, expected]).map(|_| ())
}

fn is_index_clean(root: &Path) -> Result<bool, String> {
    let args = vec![
        "diff".to_string(),
        "--cached".to_string(),
        "--quiet".to_string(),
        "--exit-code".to_string(),
        "--".to_string(),
    ];
    let output = run_git(root, &args, &[], None)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(command_error(&args, &output)),
    }
}

fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let args = vec![
        "merge-base".to_string(),
        "--is-ancestor".to_string(),
        ancestor.to_string(),
        descendant.to_string(),
    ];
    let output = run_git(root, &args, &[], None)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(command_error(&args, &output)),
    }
}

fn checked_out_branch(root: &Path) -> Result<String, String> {
    git_text(root, &["symbolic-ref", "--quiet", "HEAD"])
        .map_err(|_| "Pull requires a checked-out branch".to_string())
}

fn configured_upstream(root: &Path) -> Result<String, String> {
    git_text(
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .map_err(|_| "Pull requires the current branch to have an upstream".to_string())
}

fn checkout_state(
    root: &Path,
    branch: String,
    upstream_ref: String,
) -> Result<CheckoutState, String> {
    Ok(CheckoutState {
        branch,
        head: git_text(root, &["rev-parse", "--verify", "HEAD^{commit}"])?,
        upstream: git_text(
            root,
            &[
                "rev-parse",
                "--verify",
                &format!("{upstream_ref}^{{commit}}"),
            ],
        )?,
        upstream_ref,
    })
}

fn ensure_checkout_state(root: &Path, expected: &CheckoutState) -> Result<(), String> {
    let current_branch = checked_out_branch(root)?;
    let current_upstream_ref = configured_upstream(root)?;
    let current = checkout_state(root, current_branch, current_upstream_ref)?;
    if current != *expected {
        return Err(
            "Pull stopped because the checked-out branch, HEAD, or upstream changed during Pull"
                .to_string(),
        );
    }
    Ok(())
}

fn parse_nul_paths(bytes: &[u8], context: &str) -> Result<Vec<String>, String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path)
                .map_err(|_| format!("Pull does not support non-UTF-8 {context} paths"))?
                .to_string();
            ensure_safe_path(&path)?;
            Ok(path)
        })
        .collect()
}

fn dirty_worktree_paths(root: &Path) -> Result<Vec<String>, String> {
    let mut paths = BTreeSet::new();
    for args in [
        ["diff", "--name-only", "-z", "--"].as_slice(),
        ["ls-files", "--others", "--exclude-standard", "-z", "--"].as_slice(),
    ] {
        for path in parse_nul_paths(&git(root, args)?, "working-tree")? {
            paths.insert(path);
        }
    }
    Ok(paths.into_iter().collect())
}

fn core_autocrlf(root: &Path) -> Result<Option<String>, String> {
    let args = vec![
        "config".to_string(),
        "--get".to_string(),
        "core.autocrlf".to_string(),
    ];
    let output = run_git(root, &args, &[], None)?;
    match output.status.code() {
        Some(0) => String::from_utf8(output.stdout)
            .map(|value| Some(value.trim().to_string()))
            .map_err(|_| "Git returned an invalid core.autocrlf value".to_string()),
        Some(1) => Ok(None),
        _ => Err(command_error(&args, &output)),
    }
}

fn ensure_attributes_are_safe(
    root: &Path,
    paths: &[String],
    source: Option<&str>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec![
        "check-attr".to_string(),
        "-z".to_string(),
        "--stdin".to_string(),
    ];
    if let Some(source) = source {
        args.push(format!("--source={source}"));
    }
    args.extend(
        UNSAFE_DRAFT_ATTRIBUTES
            .iter()
            .map(|name| (*name).to_string()),
    );
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let output = run_git(root, &args, &[], Some(&input))?;
    if !output.status.success() {
        return Err(command_error(&args, &output));
    }
    let fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() % 3 != 0 {
        return Err("Git returned an invalid attribute list".to_string());
    }
    for field in fields.chunks_exact(3) {
        let path = String::from_utf8_lossy(field[0]);
        let attribute = String::from_utf8_lossy(field[1]);
        let value = String::from_utf8_lossy(field[2]);
        if !matches!(value.as_ref(), "unspecified" | "unset") {
            return Err(format!(
                "Pull stopped because draft {path} uses the active {attribute}={value} Git attribute"
            ));
        }
    }
    Ok(())
}

fn ensure_dirty_paths_are_safe(
    root: &Path,
    paths: &[String],
    head: &str,
    upstream: &str,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    if core_autocrlf(root)?.is_some_and(|value| !value.eq_ignore_ascii_case("false")) {
        return Err(
            "Pull stopped because core.autocrlf can transform local drafts during recovery"
                .to_string(),
        );
    }
    ensure_attributes_are_safe(root, paths, None)?;
    ensure_attributes_are_safe(root, paths, Some(head))?;
    ensure_attributes_are_safe(root, paths, Some(upstream))?;
    Ok(())
}

fn is_ignored(root: &Path, path: &str) -> Result<bool, String> {
    let args = vec![
        "check-ignore".to_string(),
        "--no-index".to_string(),
        "--stdin".to_string(),
        "-z".to_string(),
    ];
    let mut input = path.as_bytes().to_vec();
    input.push(0);
    let output = run_git(
        root,
        &args,
        &[("GIT_LITERAL_PATHSPECS", OsStr::new("0"))],
        Some(&input),
    )?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(command_error(&args, &output)),
    }
}

fn ensure_no_ignored_incoming_collisions(
    root: &Path,
    head: &str,
    upstream: &str,
) -> Result<(), String> {
    for change in tree_changes(root, head, upstream)? {
        if change.status != 'A' {
            continue;
        }
        let path = worktree_path_without_symlink_parents(root, &change.path)?;
        match fs::symlink_metadata(path) {
            Ok(_) if is_ignored(root, &change.path)? => {
                return Err(format!(
                    "Pull stopped because incoming path {} would overwrite an ignored local file",
                    change.path
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("Could not inspect {}: {error}", change.path));
            }
        }
    }
    Ok(())
}

fn ensure_no_operation_in_progress(root: &Path) -> Result<(), String> {
    if !git(root, &["ls-files", "-u", "-z"])?.is_empty() {
        return Err(
            "Pull stopped because the repository already has unresolved conflicts".to_string(),
        );
    }
    let git_dir = PathBuf::from(git_text(root, &["rev-parse", "--absolute-git-dir"])?);
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
    ] {
        if git_dir.join(marker).exists() {
            return Err(format!(
                "Pull stopped because another Git operation is in progress ({marker})"
            ));
        }
    }
    Ok(())
}

fn validate_repository(project_path: &str) -> Result<PathBuf, String> {
    let project = Path::new(project_path)
        .canonicalize()
        .map_err(|e| format!("Project path not found: {e}"))?;
    if !project.is_dir() || !project.join("package.json").is_file() {
        return Err("Project path is not an Astro project directory".to_string());
    }
    let root = PathBuf::from(git_text(&project, &["rev-parse", "--show-toplevel"])?);
    let root = root
        .canonicalize()
        .map_err(|e| format!("Git repository path not found: {e}"))?;
    if root != project {
        return Err(
            "Safe Pull requires the project folder to be the Git repository root".to_string(),
        );
    }
    Ok(root)
}

fn merge_tree(root: &Path, upstream: &str, snapshot: &str) -> Result<String, String> {
    let args = vec![
        "-c".to_string(),
        "merge.default=text".to_string(),
        "-c".to_string(),
        "merge.renormalize=false".to_string(),
        "merge-tree".to_string(),
        "--write-tree".to_string(),
        "--name-only".to_string(),
        upstream.to_string(),
        snapshot.to_string(),
    ];
    let output = run_git(root, &args, &[], None)?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let paths = text
            .lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .collect::<Vec<_>>();
        let detail = if paths.is_empty() {
            "the same lines".to_string()
        } else {
            paths.join(", ")
        };
        return Err(format!(
            "Pull stopped before changing your files because local and incoming edits conflict in {detail}. Your drafts and branch are unchanged."
        ));
    }
    text.lines()
        .next()
        .map(str::trim)
        .filter(|tree| !tree.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Git did not return a merged tree".to_string())
}

fn fast_forward(root: &Path, upstream: &str, empty_hooks: &Path) -> Result<(), String> {
    let args = vec![
        "-c".to_string(),
        format!("core.hooksPath={}", empty_hooks.to_string_lossy()),
        "-c".to_string(),
        "merge.autostash=false".to_string(),
        "merge".to_string(),
        "--ff-only".to_string(),
        "--no-autostash".to_string(),
        "--no-overwrite-ignore".to_string(),
        "--no-verify".to_string(),
        upstream.to_string(),
    ];
    let output = run_git(root, &args, &[], None)?;
    if !output.status.success() {
        return Err(command_error(&args, &output));
    }
    Ok(())
}

fn rollback_before_fast_forward(
    root: &Path,
    head: &str,
    snapshot: &str,
    snapshot_tree: &str,
    temporary_index: &Path,
    quarantine: &mut PullQuarantine,
) -> Result<(), String> {
    materialize_tree_delta_in_quarantine(root, head, snapshot, temporary_index, quarantine)?;
    let restored = working_tree(root, temporary_index, head)?;
    if restored != snapshot_tree {
        return Err("the recovery verification did not match".to_string());
    }
    Ok(())
}

fn rollback_original_checkout(
    root: &Path,
    expected: &CheckoutState,
    snapshot: &str,
    snapshot_tree: &str,
    temporary_index: &Path,
    quarantine: &mut PullQuarantine,
) -> Result<(), String> {
    let branch = checked_out_branch(root)?;
    let head = git_text(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if branch != expected.branch || head != expected.head {
        return Err("the checkout changed, so automatic recovery was skipped".to_string());
    }
    rollback_before_fast_forward(
        root,
        &expected.head,
        snapshot,
        snapshot_tree,
        temporary_index,
        quarantine,
    )
}

fn stop_before_checkout_change(
    root: &Path,
    recovery_ref: &str,
    snapshot: &str,
    error: String,
) -> String {
    match delete_ref(root, recovery_ref, snapshot) {
        Ok(()) => error,
        Err(cleanup_error) => format!(
            "{error}. The unused recovery snapshot remains at {recovery_ref} because cleanup failed: {cleanup_error}"
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn recover_before_fast_forward(
    root: &Path,
    expected: &CheckoutState,
    snapshot: &str,
    snapshot_tree: &str,
    temporary_index: &Path,
    quarantine: &mut PullQuarantine,
    recovery_ref: &str,
    error: String,
) -> String {
    let quarantine_path = quarantine.location();
    match rollback_original_checkout(
        root,
        expected,
        snapshot,
        snapshot_tree,
        temporary_index,
        quarantine,
    ) {
        Err(rollback_error) => format!(
            "{error}. Recovery snapshot: {recovery_ref}. Quarantine: {quarantine_path}. Automatic recovery was not safe: {rollback_error}"
        ),
        Ok(()) => match quarantine.finalize(root, temporary_index) {
            Err(quarantine_error) => format!(
                "{error}. Your files were restored, but later safety data remains at {quarantine_path} and the recovery snapshot remains at {recovery_ref}: {quarantine_error}"
            ),
            Ok(QuarantineDisposition::Retained(path)) => format!(
                "{error}. Recovery: your files were restored. A safety backup remains at {path} with snapshot {recovery_ref}"
            ),
            Ok(QuarantineDisposition::Removed) => match delete_ref(root, recovery_ref, snapshot) {
                Ok(()) => format!("{error}. Recovery: your files were restored"),
                Err(cleanup_error) => format!(
                    "{error}. Your files were restored, but the recovery snapshot remains at {recovery_ref} because cleanup failed: {cleanup_error}"
                ),
            },
        },
    }
}

fn after_fast_forward_error(
    error: String,
    recovery_ref: &str,
    quarantine: &PullQuarantine,
) -> String {
    format!(
        "{error}. Recovery snapshot: {recovery_ref}. Quarantine: {}",
        quarantine.location()
    )
}

fn finish_successful_pull(
    root: &Path,
    temporary_index: &Path,
    quarantine: &mut PullQuarantine,
    recovery_ref: &str,
    snapshot: &str,
    message: String,
) -> Result<String, String> {
    let quarantine_path = quarantine.location();
    match quarantine.finalize(root, temporary_index) {
        Err(error) => Err(format!(
            "Pull updated the branch, but recovery needs attention. The snapshot remains at {recovery_ref} and safety data remains at {quarantine_path}: {error}"
        )),
        Ok(QuarantineDisposition::Retained(path)) => Ok(format!(
            "{message}. Safety backup retained at {path} with snapshot {recovery_ref}"
        )),
        Ok(QuarantineDisposition::Removed) => match delete_ref(root, recovery_ref, snapshot) {
            Ok(()) => Ok(message),
            Err(error) => Ok(format!(
                "{message}. Recovery cleanup warning: the snapshot remains at {recovery_ref}: {error}"
            )),
        },
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_pull_recovery_base(root: &Path) -> Result<Option<(OwnedFd, PathBuf)>, String> {
    let git_directory = PathBuf::from(git_text(root, &["rev-parse", "--absolute-git-dir"])?)
        .canonicalize()
        .map_err(|error| format!("Could not locate the Git directory: {error}"))?;
    let git_fd = open(&git_directory, directory_open_flags(), Mode::empty())
        .map_err(|error| format!("Could not safely open the Git directory: {error}"))?;
    let astro_editor_fd = match openat(
        &git_fd,
        OsStr::new("astro-editor"),
        directory_open_flags(),
        Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not safely open the Pull recovery directory: {error}"
            ));
        }
    };
    let base_fd = match openat(
        &astro_editor_fd,
        OsStr::new("pull-quarantine"),
        directory_open_flags(),
        Mode::empty(),
    ) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not safely open the Pull recovery directory: {error}"
            ));
        }
    };
    Ok(Some((
        base_fd,
        git_directory.join("astro-editor").join("pull-quarantine"),
    )))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn has_generated_hex_suffix(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == RECOVERY_ID_HEX_LENGTH
            && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn recovery_status_from_id(id: &str) -> Result<PullRecoveryStatus, String> {
    ensure_safe_path(id)?;
    if Path::new(id).components().count() != 1 {
        return Err("Invalid Pull recovery id".to_string());
    }
    if has_generated_hex_suffix(id, "completed-") {
        Ok(PullRecoveryStatus::Completed)
    } else if has_generated_hex_suffix(id, "pull-") {
        Ok(PullRecoveryStatus::NeedsAttention)
    } else {
        Err("Invalid Pull recovery id".to_string())
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_recovery_id(id: &str) -> Result<(), String> {
    recovery_status_from_id(id).map(|_| ())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_recovery_ref(reference: &str) -> Result<(), String> {
    if !has_generated_hex_suffix(reference, RECOVERY_REF_PREFIX) {
        return Err("Pull recovery reference is invalid".to_string());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_git_object_id(value: &str, context: &str) -> Result<(), String> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("Pull recovery {context} is invalid"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_recovery_source_name(name: &str) -> Result<(), String> {
    ensure_safe_path(name)?;
    if Path::new(name).components().count() != 1 || !has_generated_hex_suffix(name, "source-") {
        return Err("Pull recovery source name is invalid".to_string());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_pull_recovery(base_fd: &OwnedFd, id: &str) -> Result<OwnedFd, String> {
    validate_recovery_id(id)?;
    openat(
        base_fd,
        OsStr::new(id),
        directory_open_flags(),
        Mode::empty(),
    )
    .map_err(|error| format!("Could not open Pull recovery {id}: {error}"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read_pull_recovery_manifest(directory: &OwnedFd) -> Result<RecoveryManifest, String> {
    let manifest_fd = openat(
        directory,
        OsStr::new("manifest.jsonl"),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| format!("Could not open the Pull recovery manifest: {error}"))?;
    let reader = BufReader::new(fs::File::from(manifest_fd));
    let mut created_at = String::new();
    let mut recovery_ref = String::new();
    let mut snapshot = String::new();
    let mut sources = Vec::new();
    let mut seen_sources = BTreeSet::new();
    let mut header_seen = false;
    let mut completed = false;

    let mut reader = reader;
    loop {
        let mut line = Vec::new();
        let count = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("Could not read Pull recovery data: {error}"))?;
        if count == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            // append_manifest syncs only after writing its newline. A trailing
            // fragment therefore was never durably described and is ignored.
            break;
        }
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let entry: serde_json::Value = serde_json::from_slice(&line)
            .map_err(|error| format!("Pull recovery data is invalid: {error}"))?;
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("header") => {
                if header_seen {
                    return Err("Pull recovery contains more than one header".to_string());
                }
                header_seen = true;
                created_at = entry
                    .get("createdAt")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                recovery_ref = entry
                    .get("recoveryRef")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                snapshot = entry
                    .get("snapshot")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                validate_recovery_ref(&recovery_ref)?;
                validate_git_object_id(&snapshot, "snapshot")?;
            }
            Some("source") => {
                let name = entry
                    .get("quarantineName")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "Pull recovery source name is missing".to_string())?;
                let relative_path = entry
                    .get("relativePath")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "Pull recovery source path is missing".to_string())?;
                let source_tree = entry
                    .get("sourceTree")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "Pull recovery source tree is missing".to_string())?;
                validate_recovery_source_name(name)?;
                ensure_safe_path(relative_path)?;
                validate_git_object_id(source_tree, "source tree")?;
                if !seen_sources.insert(name.to_string()) {
                    return Err("Pull recovery contains a duplicate source".to_string());
                }
                let metadata = match statat(directory, OsStr::new(name), AtFlags::SYMLINK_NOFOLLOW)
                {
                    Ok(metadata) => metadata,
                    Err(Errno::NOENT) => continue,
                    Err(error) => {
                        return Err(format!("Could not inspect Pull recovery data: {error}"));
                    }
                };
                sources.push(RecoveryManifestSource {
                    name: OsString::from(name),
                    relative_path: relative_path.to_string(),
                    source_tree: source_tree.to_string(),
                    size: metadata.st_size.max(0) as u64,
                });
            }
            Some("state")
                if entry.get("phase").and_then(serde_json::Value::as_str) == Some("completed") =>
            {
                completed = true;
            }
            _ => {}
        }
    }

    if !header_seen {
        return Err("Pull recovery header is incomplete".to_string());
    }
    if sources.is_empty() {
        return Err("Pull recovery does not contain any retained files".to_string());
    }
    Ok(RecoveryManifest {
        created_at,
        recovery_ref,
        snapshot,
        sources,
        completed,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ensure_recovery_state(
    status: PullRecoveryStatus,
    manifest: &RecoveryManifest,
) -> Result<(), String> {
    if status == PullRecoveryStatus::Completed && !manifest.completed {
        return Err("Pull recovery is not marked complete".to_string());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn list_pull_recoveries_inner(project_path: &str) -> Result<Vec<PullRecovery>, String> {
    let root = validate_repository(project_path)?;
    let Some((base_fd, _)) = open_pull_recovery_base(&root)? else {
        return Ok(Vec::new());
    };
    let mut recoveries = Vec::new();
    for id in directory_entries(&base_fd)? {
        let Some(id) = id.to_str() else {
            continue;
        };
        let Ok(status) = recovery_status_from_id(id) else {
            continue;
        };
        let directory = match open_pull_recovery(&base_fd, id) {
            Ok(directory) => directory,
            Err(error) => {
                log::warn!("Skipping unreadable Pull recovery {id}: {error}");
                continue;
            }
        };
        let manifest = match read_pull_recovery_manifest(&directory).and_then(|manifest| {
            ensure_recovery_state(status, &manifest)?;
            Ok(manifest)
        }) {
            Ok(manifest) => manifest,
            Err(error) => {
                log::warn!("Skipping invalid Pull recovery {id}: {error}");
                continue;
            }
        };
        recoveries.push(PullRecovery {
            id: id.to_string(),
            status,
            created_at: manifest.created_at,
            recovery_ref: manifest.recovery_ref,
            snapshot: manifest.snapshot,
            files: manifest
                .sources
                .into_iter()
                .map(|source| PullRecoveryFile {
                    relative_path: source.relative_path,
                    source_tree: source.source_tree,
                    size: source.size.min(u32::MAX as u64) as u32,
                })
                .collect(),
        });
    }
    recoveries.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(recoveries)
}

fn list_pull_recoveries_serialized(project_path: &str) -> Result<Vec<PullRecovery>, String> {
    let root = validate_repository(project_path)?;
    let _guard = super::project_mutation::lock_for_pull(&root)?;
    list_pull_recoveries_inner(project_path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn create_recovery_copy_directory(destination: &Path, id: &str) -> Result<PathBuf, String> {
    let suffix = id
        .strip_prefix("completed-")
        .or_else(|| id.strip_prefix("pull-"))
        .ok_or_else(|| "Invalid Pull recovery id".to_string())?;
    for attempt in 0..100_u8 {
        let name = if attempt == 0 {
            format!("Nevertheless Editor Recovery {suffix}")
        } else {
            format!("Nevertheless Editor Recovery {suffix} {attempt}")
        };
        let path = destination.join(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Could not create the recovery copy folder: {error}"
                ));
            }
        }
    }
    Err("Could not find an available recovery copy folder name".to_string())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn restore_pull_recovery_inner(
    project_path: &str,
    recovery_id: &str,
    destination_directory: &str,
) -> Result<String, String> {
    let root = validate_repository(project_path)?;
    let destination = Path::new(destination_directory)
        .canonicalize()
        .map_err(|error| format!("Recovery destination not found: {error}"))?;
    if !destination.is_dir() {
        return Err("Recovery destination is not a directory".to_string());
    }
    let Some((base_fd, _)) = open_pull_recovery_base(&root)? else {
        return Err("Pull recovery data was not found".to_string());
    };
    let status = recovery_status_from_id(recovery_id)?;
    let directory = open_pull_recovery(&base_fd, recovery_id)?;
    let manifest = read_pull_recovery_manifest(&directory)?;
    ensure_recovery_state(status, &manifest)?;
    let output = create_recovery_copy_directory(&destination, recovery_id)?;
    for (index, source) in manifest.sources.iter().enumerate() {
        let tree = source.source_tree.chars().take(8).collect::<String>();
        let version = output.join(format!("version-{}-{tree}", index + 1));
        copy_entry_from_fd(
            &directory,
            &source.name,
            &version.join(&source.relative_path),
        )?;
    }
    Ok(output.to_string_lossy().into_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn optional_ref(root: &Path, reference: &str) -> Result<Option<String>, String> {
    let args = vec![
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        reference.to_string(),
    ];
    let output = run_git(root, &args, &[], None)?;
    match output.status.code() {
        Some(0) => String::from_utf8(output.stdout)
            .map(|value| Some(value.trim().to_string()))
            .map_err(|_| "Git returned an invalid recovery ref".to_string()),
        Some(1) => Ok(None),
        _ => Err(command_error(&args, &output)),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn delete_pull_recovery_inner(project_path: &str, recovery_id: &str) -> Result<(), String> {
    let root = validate_repository(project_path)?;
    let Some((base_fd, _)) = open_pull_recovery_base(&root)? else {
        return Err("Pull recovery data was not found".to_string());
    };
    let status = recovery_status_from_id(recovery_id)?;
    let directory = open_pull_recovery(&base_fd, recovery_id)?;
    let manifest = read_pull_recovery_manifest(&directory)?;
    ensure_recovery_state(status, &manifest)?;
    if let Some(current) = optional_ref(&root, &manifest.recovery_ref)? {
        if current != manifest.snapshot {
            return Err("The Pull recovery reference changed and was not deleted".to_string());
        }
        delete_ref(&root, &manifest.recovery_ref, &manifest.snapshot)?;
    }
    remove_entry_tree(&base_fd, OsStr::new(recovery_id))?;
    sync_directory(&base_fd, "the deleted Pull recovery")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn list_pull_recoveries_inner(_project_path: &str) -> Result<Vec<PullRecovery>, String> {
    Ok(Vec::new())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn restore_pull_recovery_inner(
    _project_path: &str,
    _recovery_id: &str,
    _destination_directory: &str,
) -> Result<String, String> {
    Err("Pull recovery is unavailable on this platform".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn delete_pull_recovery_inner(_project_path: &str, _recovery_id: &str) -> Result<(), String> {
    Err("Pull recovery is unavailable on this platform".to_string())
}

fn safe_git_pull_inner(project_path: &str) -> Result<String, String> {
    let root = validate_repository(project_path)?;
    let _pull_guard = super::project_mutation::lock_for_pull(&root)?;
    super::publish::ensure_no_review_server_for_project(&root)?;
    ensure_no_operation_in_progress(&root)?;
    let branch = checked_out_branch(&root)?;
    let upstream_ref = configured_upstream(&root)?;
    if !is_index_clean(&root)? {
        return Err(
            "Pull stopped because staged changes need to be committed or unstaged first"
                .to_string(),
        );
    }

    git(&root, &["fetch", "--quiet", "--no-tags"])?;
    let expected = checkout_state(&root, branch, upstream_ref)?;
    ensure_checkout_state(&root, &expected)?;
    let head = expected.head.clone();
    let upstream = expected.upstream.clone();
    if head == upstream {
        return Ok("Already up to date".to_string());
    }
    if is_ancestor(&root, &upstream, &head)? {
        return Ok("Local branch is ahead of upstream; nothing to pull".to_string());
    }
    if !is_ancestor(&root, &head, &upstream)? {
        return Err(
            "Pull stopped because local and remote history have diverged. Nothing was changed."
                .to_string(),
        );
    }
    ensure_no_ignored_incoming_collisions(&root, &head, &upstream)?;
    let dirty_paths = dirty_worktree_paths(&root)?;
    ensure_dirty_paths_are_safe(&root, &dirty_paths, &head, &upstream)?;

    let temporary = TemporaryIndex::new()?;
    let head_tree = git_text(&root, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let snapshot_tree = working_tree(&root, &temporary.index, &head)?;

    if snapshot_tree == head_tree {
        let unchanged_tree = working_tree(&root, &temporary.index, &head)?;
        if unchanged_tree != snapshot_tree {
            return Err(
                "Pull stopped because files changed while the fast-forward was being prepared"
                    .to_string(),
            );
        }
        ensure_checkout_state(&root, &expected)?;
        ensure_no_operation_in_progress(&root)?;
        if !is_index_clean(&root)? {
            return Err(
                "Pull stopped because staged changes appeared while the fast-forward was being prepared"
                    .to_string(),
            );
        }
        fast_forward(&root, &upstream, &temporary.hooks)?;
        let updated = CheckoutState {
            head: upstream.clone(),
            ..expected.clone()
        };
        ensure_checkout_state(&root, &updated)
            .map_err(|_| "Pull could not verify the fast-forwarded branch".to_string())?;
        if !is_index_clean(&root)? {
            return Err("Pull left unexpected staged changes".to_string());
        }
        return Ok(format!(
            "Updated {} to {}",
            &head[..head.len().min(8)],
            &upstream[..upstream.len().min(8)]
        ));
    }

    let snapshot = synthetic_commit(&root, &temporary.index, &snapshot_tree, &head)?;
    let recovery_ref = format!("{RECOVERY_REF_PREFIX}{}", Uuid::new_v4().simple());
    update_ref(&root, &recovery_ref, &snapshot)?;

    let merged_tree = match merge_tree(&root, &upstream, &snapshot) {
        Ok(tree) => tree,
        Err(error) => {
            return Err(stop_before_checkout_change(
                &root,
                &recovery_ref,
                &snapshot,
                error,
            ));
        }
    };

    let unchanged_snapshot = match working_tree(&root, &temporary.index, &head) {
        Ok(tree) => tree,
        Err(error) => {
            return Err(stop_before_checkout_change(
                &root,
                &recovery_ref,
                &snapshot,
                format!("Pull could not recheck local drafts: {error}"),
            ));
        }
    };
    if unchanged_snapshot != snapshot_tree {
        return Err(stop_before_checkout_change(
            &root,
            &recovery_ref,
            &snapshot,
            "Pull stopped because files changed while reconciliation was being prepared"
                .to_string(),
        ));
    }
    let dirty_safety = dirty_worktree_paths(&root)
        .and_then(|paths| ensure_dirty_paths_are_safe(&root, &paths, &head, &upstream));
    if let Err(error) = dirty_safety {
        return Err(stop_before_checkout_change(
            &root,
            &recovery_ref,
            &snapshot,
            error,
        ));
    }
    if let Err(error) = ensure_checkout_state(&root, &expected) {
        return Err(stop_before_checkout_change(
            &root,
            &recovery_ref,
            &snapshot,
            error,
        ));
    }
    match is_index_clean(&root) {
        Ok(true) => {}
        Ok(false) => {
            return Err(stop_before_checkout_change(
                &root,
                &recovery_ref,
                &snapshot,
                "Pull stopped because staged changes appeared while reconciliation was being prepared"
                    .to_string(),
            ));
        }
        Err(error) => {
            return Err(stop_before_checkout_change(
                &root,
                &recovery_ref,
                &snapshot,
                format!("Pull could not verify the staged state: {error}"),
            ));
        }
    }
    if let Err(error) = ensure_no_operation_in_progress(&root) {
        return Err(stop_before_checkout_change(
            &root,
            &recovery_ref,
            &snapshot,
            error,
        ));
    }

    let mut quarantine = match PullQuarantine::new_with_recovery(&root, &recovery_ref, &snapshot) {
        Ok(quarantine) => quarantine,
        Err(error) => {
            return Err(stop_before_checkout_change(
                &root,
                &recovery_ref,
                &snapshot,
                format!("Pull could not prepare recovery storage: {error}"),
            ));
        }
    };

    if let Err(clear_error) = materialize_tree_delta_in_quarantine(
        &root,
        &snapshot,
        &head,
        &temporary.index,
        &mut quarantine,
    ) {
        return Err(recover_before_fast_forward(
            &root,
            &expected,
            &snapshot,
            &snapshot_tree,
            &temporary.index,
            &mut quarantine,
            &recovery_ref,
            format!("Pull could not prepare the checkout: {clear_error}"),
        ));
    }

    let cleared_tree = match working_tree(&root, &temporary.index, &head) {
        Ok(tree) => tree,
        Err(error) => {
            return Err(recover_before_fast_forward(
                &root,
                &expected,
                &snapshot,
                &snapshot_tree,
                &temporary.index,
                &mut quarantine,
                &recovery_ref,
                format!("Pull could not verify the prepared checkout: {error}"),
            ));
        }
    };
    if cleared_tree != head_tree {
        return Err(recover_before_fast_forward(
            &root,
            &expected,
            &snapshot,
            &snapshot_tree,
            &temporary.index,
            &mut quarantine,
            &recovery_ref,
            "Pull could not verify the prepared checkout".to_string(),
        ));
    }

    if let Err(state_error) = ensure_checkout_state(&root, &expected) {
        return Err(recover_before_fast_forward(
            &root,
            &expected,
            &snapshot,
            &snapshot_tree,
            &temporary.index,
            &mut quarantine,
            &recovery_ref,
            format!("Pull state changed before fast-forward: {state_error}"),
        ));
    }
    if let Err(operation_error) = ensure_no_operation_in_progress(&root) {
        return Err(recover_before_fast_forward(
            &root,
            &expected,
            &snapshot,
            &snapshot_tree,
            &temporary.index,
            &mut quarantine,
            &recovery_ref,
            format!("Pull found another Git operation before fast-forward: {operation_error}"),
        ));
    }
    match is_index_clean(&root) {
        Ok(true) => {}
        Ok(false) => {
            return Err(recover_before_fast_forward(
                &root,
                &expected,
                &snapshot,
                &snapshot_tree,
                &temporary.index,
                &mut quarantine,
                &recovery_ref,
                "Pull found staged changes before fast-forward".to_string(),
            ));
        }
        Err(error) => {
            return Err(recover_before_fast_forward(
                &root,
                &expected,
                &snapshot,
                &snapshot_tree,
                &temporary.index,
                &mut quarantine,
                &recovery_ref,
                format!("Pull could not verify the staged state before fast-forward: {error}"),
            ));
        }
    }

    if let Err(merge_error) = fast_forward(&root, &upstream, &temporary.hooks) {
        return Err(recover_before_fast_forward(
            &root,
            &expected,
            &snapshot,
            &snapshot_tree,
            &temporary.index,
            &mut quarantine,
            &recovery_ref,
            format!("Pull could not fast-forward: {merge_error}"),
        ));
    }

    let updated = CheckoutState {
        head: upstream.clone(),
        ..expected
    };
    if let Err(error) = ensure_checkout_state(&root, &updated) {
        return Err(after_fast_forward_error(
            format!("Pull could not verify the updated branch: {error}"),
            &recovery_ref,
            &quarantine,
        ));
    }
    match is_index_clean(&root) {
        Ok(true) => {}
        Ok(false) => {
            return Err(after_fast_forward_error(
                "Pull left unexpected staged changes".to_string(),
                &recovery_ref,
                &quarantine,
            ));
        }
        Err(error) => {
            return Err(after_fast_forward_error(
                format!("Pull could not verify the staged state after fast-forward: {error}"),
                &recovery_ref,
                &quarantine,
            ));
        }
    }

    let preserved_count = match materialize_tree_delta_in_quarantine(
        &root,
        &upstream,
        &merged_tree,
        &temporary.index,
        &mut quarantine,
    ) {
        Ok(count) => count,
        Err(error) => {
            return Err(after_fast_forward_error(
                format!("Pull updated the branch but could not restore every draft: {error}"),
                &recovery_ref,
                &quarantine,
            ));
        }
    };
    let restored_tree = match working_tree(&root, &temporary.index, &upstream) {
        Ok(tree) => tree,
        Err(error) => {
            return Err(after_fast_forward_error(
                format!("Pull updated the branch but could not verify restored drafts: {error}"),
                &recovery_ref,
                &quarantine,
            ));
        }
    };
    if restored_tree != merged_tree {
        return Err(after_fast_forward_error(
            "Pull updated the branch but draft verification failed".to_string(),
            &recovery_ref,
            &quarantine,
        ));
    }
    match is_index_clean(&root) {
        Ok(true) => {}
        Ok(false) => {
            return Err(after_fast_forward_error(
                "Pull restored drafts as staged changes unexpectedly".to_string(),
                &recovery_ref,
                &quarantine,
            ));
        }
        Err(error) => {
            return Err(after_fast_forward_error(
                format!("Pull could not verify the restored staged state: {error}"),
                &recovery_ref,
                &quarantine,
            ));
        }
    }
    if let Err(error) = ensure_checkout_state(&root, &updated) {
        return Err(after_fast_forward_error(
            format!("Pull state changed after drafts were restored: {error}"),
            &recovery_ref,
            &quarantine,
        ));
    }

    let message = format!(
        "Updated {} to {}; preserved {preserved_count} local draft file(s)",
        &head[..head.len().min(8)],
        &upstream[..upstream.len().min(8)]
    );
    finish_successful_pull(
        &root,
        &temporary.index,
        &mut quarantine,
        &recovery_ref,
        &snapshot,
        message,
    )
}

#[tauri::command]
#[specta::specta]
pub async fn list_pull_recoveries(project_path: String) -> Result<Vec<PullRecovery>, String> {
    tauri::async_runtime::spawn_blocking(move || list_pull_recoveries_serialized(&project_path))
        .await
        .map_err(|error| format!("Could not list Pull recoveries: {error}"))?
}

#[tauri::command]
#[specta::specta]
pub async fn restore_pull_recovery(
    project_path: String,
    recovery_id: String,
    destination_directory: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = validate_repository(&project_path)?;
        let _guard =
            super::project_mutation::try_lock_for_write(&root, "restore the Pull recovery")?;
        restore_pull_recovery_inner(&project_path, &recovery_id, &destination_directory)
    })
    .await
    .map_err(|error| format!("Could not restore the Pull recovery: {error}"))?
}

#[tauri::command]
#[specta::specta]
pub async fn delete_pull_recovery(project_path: String, recovery_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = validate_repository(&project_path)?;
        let _guard =
            super::project_mutation::try_lock_for_write(&root, "delete the Pull recovery")?;
        delete_pull_recovery_inner(&project_path, &recovery_id)
    })
    .await
    .map_err(|error| format!("Could not delete the Pull recovery: {error}"))?
}

/// Safely fast-forwards the current project while preserving a dirty working
/// tree. Local changes are merged in memory first, so conflicts never reach
/// the checkout and successful reconciliation leaves drafts unstaged.
#[tauri::command]
#[specta::specta]
pub async fn safe_git_pull(project_path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || safe_git_pull_inner(&project_path))
        .await
        .map_err(|e| format!("Safe Pull failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};
    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};
    use tempfile::TempDir;

    struct TestRepo {
        _temporary: TempDir,
        origin: PathBuf,
        local: PathBuf,
        writer: PathBuf,
    }

    fn test_git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn configure(root: &Path) {
        test_git(root, &["config", "user.name", "Nevertheless Editor Test"]);
        test_git(root, &["config", "user.email", "test@localhost"]);
    }

    fn setup() -> TestRepo {
        let temporary = tempfile::tempdir().unwrap();
        let origin = temporary.path().join("origin.git");
        let seed = temporary.path().join("seed");
        let local = temporary.path().join("local");
        let writer = temporary.path().join("writer");

        fs::create_dir(&origin).unwrap();
        test_git(&origin, &["init", "--bare", "--initial-branch=main"]);
        fs::create_dir(&seed).unwrap();
        test_git(&seed, &["init", "--initial-branch=main"]);
        configure(&seed);
        write(&seed.join("package.json"), "{}\n");
        write(&seed.join("post.md"), "first\nbase\nlast\n");
        write(&seed.join("other.md"), "base\n");
        test_git(&seed, &["add", "."]);
        test_git(&seed, &["commit", "-m", "base"]);
        test_git(
            &seed,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        test_git(&seed, &["push", "-u", "origin", "main"]);
        test_git(
            temporary.path(),
            &["clone", origin.to_str().unwrap(), local.to_str().unwrap()],
        );
        test_git(
            temporary.path(),
            &["clone", origin.to_str().unwrap(), writer.to_str().unwrap()],
        );
        configure(&local);
        configure(&writer);

        TestRepo {
            _temporary: temporary,
            origin,
            local,
            writer,
        }
    }

    fn push_writer(repo: &TestRepo, path: &str, content: &str) {
        write(&repo.writer.join(path), content);
        test_git(&repo.writer, &["add", path]);
        test_git(&repo.writer, &["commit", "-m", "remote change"]);
        test_git(&repo.writer, &["push", "origin", "main"]);
    }

    fn status(root: &Path) -> String {
        test_git(root, &["status", "--porcelain=v1", "--untracked-files=all"])
    }

    fn exclude(root: &Path, pattern: &str) {
        write(&root.join(".git/info/exclude"), pattern);
    }

    fn commit_all(root: &Path, message: &str) -> String {
        test_git(root, &["add", "-A"]);
        test_git(root, &["commit", "-m", message]);
        test_git(root, &["rev-parse", "HEAD"])
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn create_completed_pull_recovery(repo: &TestRepo) -> PullRecovery {
        write(
            &repo.local.join("post.md"),
            "first\nbase\nlocal recovery draft\n",
        );
        push_writer(repo, "other.md", "remote recovery fixture\n");

        let result = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();
        assert!(result.contains("Safety backup retained"), "{result}");
        let mut recoveries = list_pull_recoveries_inner(repo.local.to_str().unwrap()).unwrap();
        assert_eq!(recoveries.len(), 1);
        recoveries.remove(0)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn create_incomplete_pull_recovery(repo: &TestRepo, late_write: Option<&str>) -> PullRecovery {
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let post = repo.local.join("post.md");
        write(&post, "first\nbase\nincomplete recovery draft\n");
        let temporary = TemporaryIndex::new().unwrap();
        let snapshot_tree = working_tree(&repo.local, &temporary.index, &head).unwrap();
        let snapshot =
            synthetic_commit(&repo.local, &temporary.index, &snapshot_tree, &head).unwrap();
        let recovery_ref = format!("{RECOVERY_REF_PREFIX}{}", Uuid::new_v4().simple());
        update_ref(&repo.local, &recovery_ref, &snapshot).unwrap();
        let mut quarantine =
            PullQuarantine::new_with_recovery(&repo.local, &recovery_ref, &snapshot).unwrap();
        let recovery_id = quarantine.directory_name.to_string_lossy().into_owned();

        if let Some(late_write) = late_write {
            let mut open_writer = fs::OpenOptions::new().write(true).open(&post).unwrap();
            let mut wrote = false;
            materialize_tree_delta_with_hook(
                &repo.local,
                &snapshot,
                &head,
                &temporary.index,
                &mut quarantine,
                &mut |point, path| {
                    if !wrote && point == TransitionHookPoint::TargetPrepared && path == "post.md" {
                        open_writer.set_len(0).unwrap();
                        open_writer.seek(SeekFrom::Start(0)).unwrap();
                        open_writer.write_all(late_write.as_bytes()).unwrap();
                        open_writer.sync_all().unwrap();
                        wrote = true;
                    }
                },
            )
            .unwrap();
            assert!(wrote);
        } else {
            materialize_tree_delta_in_quarantine(
                &repo.local,
                &snapshot,
                &head,
                &temporary.index,
                &mut quarantine,
            )
            .unwrap();
        }
        drop(quarantine);

        list_pull_recoveries_inner(repo.local.to_str().unwrap())
            .unwrap()
            .into_iter()
            .find(|recovery| recovery.id == recovery_id)
            .expect("incomplete Pull recovery should be discoverable")
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn recovery_directory(repo: &TestRepo, recovery: &PullRecovery) -> PathBuf {
        let (_, base_path) = open_pull_recovery_base(&repo.local).unwrap().unwrap();
        base_path.join(&recovery.id)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn rewrite_manifest_value(
        manifest_path: &Path,
        entry_type: &str,
        key: &str,
        replacement: &str,
    ) {
        let content = fs::read_to_string(manifest_path).unwrap();
        let mut replaced = false;
        let mut lines = Vec::new();
        for line in content.lines() {
            let mut entry: serde_json::Value = serde_json::from_str(line).unwrap();
            if !replaced
                && entry.get("type").and_then(serde_json::Value::as_str) == Some(entry_type)
            {
                entry[key] = serde_json::Value::String(replacement.to_string());
                replaced = true;
            }
            lines.push(serde_json::to_string(&entry).unwrap());
        }
        assert!(replaced, "manifest entry {entry_type} was not found");
        fs::write(manifest_path, format!("{}\n", lines.join("\n"))).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn restored_post_versions(output: &Path) -> Vec<String> {
        let mut versions = fs::read_dir(output)
            .unwrap()
            .map(|entry| entry.unwrap().path().join("post.md"))
            .filter(|path| path.is_file())
            .map(|path| fs::read_to_string(path).unwrap())
            .collect::<Vec<_>>();
        versions.sort();
        versions
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn lists_completed_pull_recovery() {
        let repo = setup();
        let recovery = create_completed_pull_recovery(&repo);

        assert!(has_generated_hex_suffix(&recovery.id, "completed-"));
        assert_eq!(recovery.status, PullRecoveryStatus::Completed);
        assert!(!recovery.created_at.is_empty());
        assert!(has_generated_hex_suffix(
            &recovery.recovery_ref,
            RECOVERY_REF_PREFIX
        ));
        assert_eq!(
            optional_ref(&repo.local, &recovery.recovery_ref).unwrap(),
            Some(recovery.snapshot.clone())
        );
        assert!(!recovery.files.is_empty());
        assert!(recovery
            .files
            .iter()
            .all(|file| file.relative_path == "post.md" && file.size > 0));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn lists_restores_and_deletes_incomplete_crash_recovery() {
        let repo = setup();
        let recovery = create_incomplete_pull_recovery(&repo, None);
        let directory = recovery_directory(&repo, &recovery);
        let destination = repo._temporary.path().join("incomplete-recovery-copy");
        fs::create_dir(&destination).unwrap();

        assert!(has_generated_hex_suffix(&recovery.id, "pull-"));
        assert_eq!(recovery.status, PullRecoveryStatus::NeedsAttention);
        assert!(!recovery.files.is_empty());
        assert_eq!(
            optional_ref(&repo.local, &recovery.recovery_ref).unwrap(),
            Some(recovery.snapshot.clone())
        );

        let output = PathBuf::from(
            restore_pull_recovery_inner(
                repo.local.to_str().unwrap(),
                &recovery.id,
                destination.to_str().unwrap(),
            )
            .unwrap(),
        );
        assert!(restored_post_versions(&output)
            .iter()
            .any(|content| content == "first\nbase\nincomplete recovery draft\n"));
        assert!(directory.is_dir());

        delete_pull_recovery_inner(repo.local.to_str().unwrap(), &recovery.id).unwrap();
        assert!(!directory.exists());
        assert_eq!(
            optional_ref(&repo.local, &recovery.recovery_ref).unwrap(),
            None
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn incomplete_recovery_restores_a_late_open_file_write() {
        let repo = setup();
        let late_write = "late open-file write requiring attention\n";
        let recovery = create_incomplete_pull_recovery(&repo, Some(late_write));
        let destination = repo._temporary.path().join("late-write-recovery-copy");
        fs::create_dir(&destination).unwrap();

        let output = PathBuf::from(
            restore_pull_recovery_inner(
                repo.local.to_str().unwrap(),
                &recovery.id,
                destination.to_str().unwrap(),
            )
            .unwrap(),
        );

        assert_eq!(recovery.status, PullRecoveryStatus::NeedsAttention);
        assert!(restored_post_versions(&output)
            .iter()
            .any(|content| content == late_write));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn incomplete_recovery_ignores_a_truncated_manifest_tail() {
        let repo = setup();
        let recovery = create_incomplete_pull_recovery(&repo, None);
        let manifest = recovery_directory(&repo, &recovery).join("manifest.jsonl");
        fs::OpenOptions::new()
            .append(true)
            .open(manifest)
            .unwrap()
            .write_all(b"{\"type\":\"source\"")
            .unwrap();

        let listed = list_pull_recoveries_inner(repo.local.to_str().unwrap()).unwrap();

        assert!(listed.iter().any(|item| {
            item.id == recovery.id
                && item.status == PullRecoveryStatus::NeedsAttention
                && !item.files.is_empty()
        }));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn newline_terminated_manifest_corruption_fails_closed() {
        let repo = setup();
        let recovery = create_incomplete_pull_recovery(&repo, None);
        let directory = recovery_directory(&repo, &recovery);
        let manifest = directory.join("manifest.jsonl");
        fs::OpenOptions::new()
            .append(true)
            .open(manifest)
            .unwrap()
            .write_all(b"{\"type\":\"source\"\n")
            .unwrap();
        let destination = repo._temporary.path().join("invalid-manifest-copy");
        fs::create_dir(&destination).unwrap();

        let listed = list_pull_recoveries_inner(repo.local.to_str().unwrap()).unwrap();
        let restore_error = restore_pull_recovery_inner(
            repo.local.to_str().unwrap(),
            &recovery.id,
            destination.to_str().unwrap(),
        )
        .unwrap_err();
        let delete_error =
            delete_pull_recovery_inner(repo.local.to_str().unwrap(), &recovery.id).unwrap_err();

        assert!(listed.iter().all(|item| item.id != recovery.id));
        assert!(restore_error.contains("data is invalid"), "{restore_error}");
        assert!(delete_error.contains("data is invalid"), "{delete_error}");
        assert!(directory.is_dir());
        assert!(fs::read_dir(destination).unwrap().next().is_none());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn recovery_listing_waits_for_the_project_mutation_lock() {
        let repo = setup();
        let recovery = create_incomplete_pull_recovery(&repo, None);
        let guard = crate::commands::project_mutation::try_lock_for_write(
            &repo.local,
            "hold the recovery-list test lock",
        )
        .unwrap();
        let project_path = repo.local.to_string_lossy().into_owned();
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            started_sender.send(()).unwrap();
            result_sender
                .send(list_pull_recoveries_serialized(&project_path))
                .unwrap();
        });
        started_receiver.recv().unwrap();

        assert!(matches!(
            result_receiver.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(guard);
        let listed = result_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        worker.join().unwrap();

        assert!(listed.iter().any(|item| item.id == recovery.id));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn restores_pull_recovery_as_non_destructive_copies() {
        let repo = setup();
        let recovery = create_completed_pull_recovery(&repo);
        let destination = repo._temporary.path().join("restored-copies");
        fs::create_dir(&destination).unwrap();

        let first = PathBuf::from(
            restore_pull_recovery_inner(
                repo.local.to_str().unwrap(),
                &recovery.id,
                destination.to_str().unwrap(),
            )
            .unwrap(),
        );
        let second = PathBuf::from(
            restore_pull_recovery_inner(
                repo.local.to_str().unwrap(),
                &recovery.id,
                destination.to_str().unwrap(),
            )
            .unwrap(),
        );

        assert_ne!(first, second);
        for output in [&first, &second] {
            let versions = restored_post_versions(output);
            assert!(versions
                .iter()
                .any(|content| content == "first\nbase\nlast\n"));
            assert!(versions
                .iter()
                .any(|content| content == "first\nbase\nlocal recovery draft\n"));
        }
        assert_eq!(
            list_pull_recoveries_inner(repo.local.to_str().unwrap())
                .unwrap()
                .len(),
            1
        );
        assert!(recovery_directory(&repo, &recovery).is_dir());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn deletes_pull_recovery_and_its_matching_ref() {
        let repo = setup();
        let recovery = create_completed_pull_recovery(&repo);
        let directory = recovery_directory(&repo, &recovery);

        delete_pull_recovery_inner(repo.local.to_str().unwrap(), &recovery.id).unwrap();

        assert!(!directory.exists());
        assert_eq!(
            optional_ref(&repo.local, &recovery.recovery_ref).unwrap(),
            None
        );
        assert!(list_pull_recoveries_inner(repo.local.to_str().unwrap())
            .unwrap()
            .is_empty());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn recovery_ids_reject_traversal_without_touching_the_backup() {
        for incomplete in [false, true] {
            let repo = setup();
            let recovery = if incomplete {
                create_incomplete_pull_recovery(&repo, None)
            } else {
                create_completed_pull_recovery(&repo)
            };
            let expected_status = if incomplete {
                PullRecoveryStatus::NeedsAttention
            } else {
                PullRecoveryStatus::Completed
            };
            assert_eq!(recovery.status, expected_status);
            let destination = repo._temporary.path().join("traversal-destination");
            fs::create_dir(&destination).unwrap();
            let malicious_id = format!("../{}", recovery.id);

            let restore_error = restore_pull_recovery_inner(
                repo.local.to_str().unwrap(),
                &malicious_id,
                destination.to_str().unwrap(),
            )
            .unwrap_err();
            let delete_error =
                delete_pull_recovery_inner(repo.local.to_str().unwrap(), &malicious_id)
                    .unwrap_err();

            assert!(
                restore_error.contains("unsafe repository path"),
                "{restore_error}"
            );
            assert!(
                delete_error.contains("unsafe repository path"),
                "{delete_error}"
            );
            assert!(recovery_directory(&repo, &recovery).is_dir());
            assert!(fs::read_dir(destination).unwrap().next().is_none());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn restore_rejects_manifest_tree_traversal() {
        for incomplete in [false, true] {
            let repo = setup();
            let recovery = if incomplete {
                create_incomplete_pull_recovery(&repo, None)
            } else {
                create_completed_pull_recovery(&repo)
            };
            let directory = recovery_directory(&repo, &recovery);
            rewrite_manifest_value(
                &directory.join("manifest.jsonl"),
                "source",
                "sourceTree",
                "../../../../outside",
            );
            let destination = repo._temporary.path().join("tree-traversal-destination");
            fs::create_dir(&destination).unwrap();

            let error = restore_pull_recovery_inner(
                repo.local.to_str().unwrap(),
                &recovery.id,
                destination.to_str().unwrap(),
            )
            .unwrap_err();

            assert!(error.contains("source tree is invalid"), "{error}");
            assert!(fs::read_dir(destination).unwrap().next().is_none());
            assert!(directory.is_dir());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn delete_rejects_manifest_ref_outside_recovery_namespace() {
        for incomplete in [false, true] {
            let repo = setup();
            let recovery = if incomplete {
                create_incomplete_pull_recovery(&repo, None)
            } else {
                create_completed_pull_recovery(&repo)
            };
            let directory = recovery_directory(&repo, &recovery);
            let main_before = test_git(&repo.local, &["rev-parse", "refs/heads/main"]);
            rewrite_manifest_value(
                &directory.join("manifest.jsonl"),
                "header",
                "recoveryRef",
                "refs/heads/main",
            );

            let error =
                delete_pull_recovery_inner(repo.local.to_str().unwrap(), &recovery.id).unwrap_err();

            assert!(error.contains("reference is invalid"), "{error}");
            assert_eq!(
                test_git(&repo.local, &["rev-parse", "refs/heads/main"]),
                main_before
            );
            assert!(directory.is_dir());
        }
    }

    #[test]
    fn clean_checkout_fast_forwards() {
        let repo = setup();
        push_writer(&repo, "other.md", "remote\n");

        let result = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();

        assert!(result.starts_with("Updated"));
        assert_eq!(
            fs::read_to_string(repo.local.join("other.md")).unwrap(),
            "remote\n"
        );
        assert!(status(&repo.local).is_empty());
    }

    #[test]
    fn preserves_non_overlapping_tracked_and_untracked_drafts() {
        let repo = setup();
        write(&repo.local.join("post.md"), "first\nbase\nlocal last\n");
        write(&repo.local.join("draft.md"), "new draft\n");
        push_writer(&repo, "other.md", "remote\n");

        safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();

        assert_eq!(
            fs::read_to_string(repo.local.join("post.md")).unwrap(),
            "first\nbase\nlocal last\n"
        );
        assert_eq!(
            fs::read_to_string(repo.local.join("draft.md")).unwrap(),
            "new draft\n"
        );
        let state = status(&repo.local);
        assert!(state.contains("M post.md"));
        assert!(state.contains("?? draft.md"));
        assert!(test_git(&repo.local, &["diff", "--cached", "--name-only"]).is_empty());
    }

    #[test]
    fn destructive_transition_refuses_to_overwrite_a_later_write() {
        let repo = setup();
        let temporary = TemporaryIndex::new().unwrap();
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let captured = "first\nbase\ncaptured draft\n";
        let later = "first\nbase\nlater concurrent draft\n";
        write(&repo.local.join("post.md"), captured);
        let snapshot_tree = working_tree(&repo.local, &temporary.index, &head).unwrap();

        // Deterministically model a writer landing after Pull's final snapshot
        // but before the destructive checkout transition begins.
        write(&repo.local.join("post.md"), later);
        let error = materialize_tree_delta(&repo.local, &snapshot_tree, &head, &temporary.index)
            .unwrap_err();

        assert!(error.contains("post.md changed"), "{error}");
        assert_eq!(
            fs::read_to_string(repo.local.join("post.md")).unwrap(),
            later
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn later_write_through_open_file_is_preserved_after_quarantine() {
        let repo = setup();
        let temporary = TemporaryIndex::new().unwrap();
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let captured = "first\nbase\ncaptured draft\n";
        let later = "first\nbase\nlater open-file draft\n";
        let post = repo.local.join("post.md");
        write(&post, captured);
        let snapshot_tree = working_tree(&repo.local, &temporary.index, &head).unwrap();
        let mut open_writer = fs::OpenOptions::new().write(true).open(&post).unwrap();
        let mut quarantine = PullQuarantine::new(&repo.local).unwrap();
        let mut wrote_after_quarantine = false;

        let error = materialize_tree_delta_with_hook(
            &repo.local,
            &snapshot_tree,
            &head,
            &temporary.index,
            &mut quarantine,
            &mut |point, path| {
                if !wrote_after_quarantine
                    && point == TransitionHookPoint::Quarantined
                    && path == "post.md"
                {
                    open_writer.set_len(0).unwrap();
                    open_writer.seek(SeekFrom::Start(0)).unwrap();
                    open_writer.write_all(later.as_bytes()).unwrap();
                    open_writer.sync_all().unwrap();
                    wrote_after_quarantine = true;
                }
            },
        )
        .unwrap_err();

        assert!(wrote_after_quarantine);
        assert!(error.contains("post.md changed"), "{error}");
        assert_eq!(fs::read_to_string(post).unwrap(), later);
        assert!(quarantine.path.exists());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn finalized_quarantine_retains_a_late_open_file_write() {
        let repo = setup();
        let temporary = TemporaryIndex::new().unwrap();
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let captured = "first\nbase\ncaptured draft\n";
        let later = "first\nbase\nlate write after validation\n";
        let post = repo.local.join("post.md");
        write(&post, captured);
        let snapshot_tree = working_tree(&repo.local, &temporary.index, &head).unwrap();
        let mut open_writer = fs::OpenOptions::new().write(true).open(&post).unwrap();
        let mut quarantine = PullQuarantine::new(&repo.local).unwrap();
        materialize_tree_delta_in_quarantine(
            &repo.local,
            &snapshot_tree,
            &head,
            &temporary.index,
            &mut quarantine,
        )
        .unwrap();

        let retained = match quarantine.finalize(&repo.local, &temporary.index).unwrap() {
            QuarantineDisposition::Retained(path) => PathBuf::from(path),
            QuarantineDisposition::Removed => {
                panic!("quarantined source data must remain recoverable")
            }
        };
        open_writer.set_len(0).unwrap();
        open_writer.seek(SeekFrom::Start(0)).unwrap();
        open_writer.write_all(later.as_bytes()).unwrap();
        open_writer.sync_all().unwrap();

        let retained_sources = fs::read_dir(&retained)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name != "manifest.jsonl")
            })
            .collect::<Vec<_>>();
        assert_eq!(retained_sources.len(), 1);
        assert_eq!(fs::read(&retained_sources[0]).unwrap(), later.as_bytes());
        assert_eq!(fs::read_to_string(post).unwrap(), "first\nbase\nlast\n");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn late_write_after_validation_requires_recovery_attention() {
        let repo = setup();
        let temporary = TemporaryIndex::new().unwrap();
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let post = repo.local.join("post.md");
        write(&post, "first\nbase\ncaptured draft\n");
        let snapshot_tree = working_tree(&repo.local, &temporary.index, &head).unwrap();
        let mut open_writer = fs::OpenOptions::new().write(true).open(&post).unwrap();
        let mut quarantine = PullQuarantine::new(&repo.local).unwrap();
        let mut wrote_after_validation = false;
        materialize_tree_delta_with_hook(
            &repo.local,
            &snapshot_tree,
            &head,
            &temporary.index,
            &mut quarantine,
            &mut |point, path| {
                if !wrote_after_validation
                    && point == TransitionHookPoint::TargetPrepared
                    && path == "post.md"
                {
                    open_writer.set_len(0).unwrap();
                    open_writer.seek(SeekFrom::Start(0)).unwrap();
                    open_writer
                        .write_all(b"late write needing attention\n")
                        .unwrap();
                    open_writer.sync_all().unwrap();
                    wrote_after_validation = true;
                }
            },
        )
        .unwrap();

        let error = finish_successful_pull(
            &repo.local,
            &temporary.index,
            &mut quarantine,
            "refs/astro-editor/pull-recovery/test-late-write",
            &snapshot_tree,
            "Updated 11111111 to 22222222".to_string(),
        )
        .unwrap_err();

        assert!(wrote_after_validation);
        assert!(error.contains("recovery needs attention"), "{error}");
        assert!(error.contains("received a later write"), "{error}");
        assert!(quarantine.path.exists());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn parent_replaced_by_outside_symlink_stops_without_writing_outside() {
        let repo = setup();
        write(&repo.local.join("nested/post.md"), "base nested\n");
        let from = commit_all(&repo.local, "add nested post");
        write(&repo.local.join("nested/post.md"), "incoming nested\n");
        let to = commit_all(&repo.local, "change nested post");
        test_git(&repo.local, &["switch", "--detach", &from]);

        let outside = repo._temporary.path().join("outside-parent");
        fs::create_dir(&outside).unwrap();
        write(&outside.join("post.md"), "outside content\n");
        let displaced = repo.local.join("nested-displaced");
        let temporary = TemporaryIndex::new().unwrap();
        let mut quarantine = PullQuarantine::new(&repo.local).unwrap();
        let mut replaced_parent = false;

        let result = materialize_tree_delta_with_hook(
            &repo.local,
            &from,
            &to,
            &temporary.index,
            &mut quarantine,
            &mut |point, path| {
                if !replaced_parent
                    && point == TransitionHookPoint::ParentOpened
                    && path == "nested/post.md"
                {
                    fs::rename(repo.local.join("nested"), &displaced).unwrap();
                    symlink(&outside, repo.local.join("nested")).unwrap();
                    replaced_parent = true;
                }
            },
        );

        assert!(replaced_parent);
        let error = result.expect_err("a replaced live parent must stop reconciliation");
        assert!(error.contains("parent"), "{error}");
        assert_eq!(
            fs::read_to_string(outside.join("post.md")).unwrap(),
            "outside content\n"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn materializes_file_to_directory_transition() {
        let repo = setup();
        write(&repo.local.join("shape"), "file version\n");
        let from = commit_all(&repo.local, "add file shape");
        fs::remove_file(repo.local.join("shape")).unwrap();
        write(&repo.local.join("shape/child.md"), "directory version\n");
        let to = commit_all(&repo.local, "replace file with directory");
        test_git(&repo.local, &["switch", "--detach", &from]);
        let temporary = TemporaryIndex::new().unwrap();

        materialize_tree_delta(&repo.local, &from, &to, &temporary.index).unwrap();

        assert!(repo.local.join("shape").is_dir());
        assert_eq!(
            fs::read_to_string(repo.local.join("shape/child.md")).unwrap(),
            "directory version\n"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn materializes_directory_to_file_transition() {
        let repo = setup();
        write(&repo.local.join("shape/child.md"), "directory version\n");
        let from = commit_all(&repo.local, "add directory shape");
        fs::remove_dir_all(repo.local.join("shape")).unwrap();
        write(&repo.local.join("shape"), "file version\n");
        let to = commit_all(&repo.local, "replace directory with file");
        test_git(&repo.local, &["switch", "--detach", &from]);
        let temporary = TemporaryIndex::new().unwrap();

        materialize_tree_delta(&repo.local, &from, &to, &temporary.index).unwrap();

        assert!(repo.local.join("shape").is_file());
        assert_eq!(
            fs::read_to_string(repo.local.join("shape")).unwrap(),
            "file version\n"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn refuses_untracked_occupant_without_overwriting_it() {
        let repo = setup();
        let from = test_git(&repo.local, &["rev-parse", "HEAD"]);
        write(&repo.local.join("incoming.md"), "tracked incoming\n");
        let to = commit_all(&repo.local, "add incoming file");
        test_git(&repo.local, &["switch", "--detach", &from]);
        write(
            &repo.local.join("incoming.md"),
            "private untracked occupant\n",
        );
        let before_status = status(&repo.local);
        let temporary = TemporaryIndex::new().unwrap();

        let error = materialize_tree_delta(&repo.local, &from, &to, &temporary.index).unwrap_err();

        assert!(error.contains("incoming.md changed"), "{error}");
        assert_eq!(
            fs::read_to_string(repo.local.join("incoming.md")).unwrap(),
            "private untracked occupant\n"
        );
        assert_eq!(status(&repo.local), before_status);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn pre_fast_forward_recovery_restores_head_bytes_and_status() {
        let repo = setup();
        let post = repo.local.join("post.md");
        let draft = repo.local.join("draft.md");
        write(&post, "first\nbase\nlocal draft\n");
        write(&draft, "untracked draft\n");
        let before_head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let before_post = fs::read(&post).unwrap();
        let before_draft = fs::read(&draft).unwrap();
        let before_status = status(&repo.local);
        let branch = checked_out_branch(&repo.local).unwrap();
        let upstream_ref = configured_upstream(&repo.local).unwrap();
        let expected = checkout_state(&repo.local, branch, upstream_ref).unwrap();
        let temporary = TemporaryIndex::new().unwrap();
        let snapshot_tree = working_tree(&repo.local, &temporary.index, &before_head).unwrap();
        let snapshot =
            synthetic_commit(&repo.local, &temporary.index, &snapshot_tree, &before_head).unwrap();
        let recovery_ref = format!("{RECOVERY_REF_PREFIX}test-pre-fast-forward");
        update_ref(&repo.local, &recovery_ref, &snapshot).unwrap();
        let mut quarantine = PullQuarantine::new(&repo.local).unwrap();
        materialize_tree_delta_in_quarantine(
            &repo.local,
            &snapshot,
            &before_head,
            &temporary.index,
            &mut quarantine,
        )
        .unwrap();
        assert!(status(&repo.local).is_empty());

        let message = recover_before_fast_forward(
            &repo.local,
            &expected,
            &snapshot,
            &snapshot_tree,
            &temporary.index,
            &mut quarantine,
            &recovery_ref,
            "Injected failure before fast-forward".to_string(),
        );

        assert!(
            message.contains("Recovery: your files were restored"),
            "{message}"
        );
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), before_head);
        assert_eq!(fs::read(post).unwrap(), before_post);
        assert_eq!(fs::read(draft).unwrap(), before_draft);
        assert_eq!(status(&repo.local), before_status);
        assert!(message.contains("A safety backup remains"), "{message}");
        assert_eq!(
            test_git(&repo.local, &["rev-parse", &recovery_ref]),
            snapshot
        );
        assert!(quarantine.path.exists());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn pre_fast_forward_recovery_restores_after_partial_clear() {
        let repo = setup();
        write(&repo.local.join("alpha.md"), "base alpha\n");
        write(&repo.local.join("zeta.md"), "base zeta\n");
        commit_all(&repo.local, "add recovery fixtures");
        write(&repo.local.join("alpha.md"), "local alpha\n");
        write(&repo.local.join("zeta.md"), "local zeta\n");
        let before_head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let before_alpha = fs::read(repo.local.join("alpha.md")).unwrap();
        let before_zeta = fs::read(repo.local.join("zeta.md")).unwrap();
        let before_status = status(&repo.local);
        let branch = checked_out_branch(&repo.local).unwrap();
        let upstream_ref = configured_upstream(&repo.local).unwrap();
        let expected = checkout_state(&repo.local, branch, upstream_ref).unwrap();
        let temporary = TemporaryIndex::new().unwrap();
        let snapshot_tree = working_tree(&repo.local, &temporary.index, &before_head).unwrap();
        let snapshot =
            synthetic_commit(&repo.local, &temporary.index, &snapshot_tree, &before_head).unwrap();
        let recovery_ref = format!("{RECOVERY_REF_PREFIX}test-partial-clear");
        update_ref(&repo.local, &recovery_ref, &snapshot).unwrap();
        let mut quarantine = PullQuarantine::new(&repo.local).unwrap();
        let original_permissions = fs::metadata(&repo.local).unwrap().permissions();
        let mut blocked_second_install = false;

        let clear_result = materialize_tree_delta_with_hook(
            &repo.local,
            &snapshot,
            &before_head,
            &temporary.index,
            &mut quarantine,
            &mut |point, path| {
                if !blocked_second_install
                    && point == TransitionHookPoint::TargetPrepared
                    && path == "zeta.md"
                {
                    let mut read_only = original_permissions.clone();
                    read_only.set_mode(0o555);
                    fs::set_permissions(&repo.local, read_only).unwrap();
                    blocked_second_install = true;
                }
            },
        );
        fs::set_permissions(&repo.local, original_permissions).unwrap();

        assert!(blocked_second_install);
        assert!(
            clear_result.is_err(),
            "the injected second install must fail"
        );
        let message = recover_before_fast_forward(
            &repo.local,
            &expected,
            &snapshot,
            &snapshot_tree,
            &temporary.index,
            &mut quarantine,
            &recovery_ref,
            "Injected failure during checkout preparation".to_string(),
        );

        assert!(
            message.contains("Recovery: your files were restored"),
            "{message}"
        );
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), before_head);
        assert_eq!(fs::read(repo.local.join("alpha.md")).unwrap(), before_alpha);
        assert_eq!(fs::read(repo.local.join("zeta.md")).unwrap(), before_zeta);
        assert_eq!(status(&repo.local), before_status);
        assert!(message.contains("A safety backup remains"), "{message}");
        assert_eq!(
            test_git(&repo.local, &["rev-parse", &recovery_ref]),
            snapshot
        );
        assert!(quarantine.path.exists());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn recovery_ref_cleanup_failure_is_a_success_warning() {
        let repo = setup();
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        write(&repo.local.join("post.md"), "first\nbase\nlocal draft\n");
        let temporary = TemporaryIndex::new().unwrap();
        let snapshot_tree = working_tree(&repo.local, &temporary.index, &head).unwrap();
        let snapshot =
            synthetic_commit(&repo.local, &temporary.index, &snapshot_tree, &head).unwrap();
        assert_ne!(snapshot, head);
        let recovery_ref = format!("{RECOVERY_REF_PREFIX}test-cleanup-warning");
        update_ref(&repo.local, &recovery_ref, &head).unwrap();
        let mut quarantine = PullQuarantine::new(&repo.local).unwrap();

        let message = finish_successful_pull(
            &repo.local,
            &temporary.index,
            &mut quarantine,
            &recovery_ref,
            &snapshot,
            "Updated 11111111 to 22222222".to_string(),
        )
        .unwrap();

        assert!(
            message.starts_with("Updated 11111111 to 22222222"),
            "{message}"
        );
        assert!(message.contains("Recovery cleanup warning"), "{message}");
        assert!(message.contains(&recovery_ref), "{message}");
        assert_eq!(test_git(&repo.local, &["rev-parse", &recovery_ref]), head);
    }

    #[test]
    fn combines_disjoint_local_and_incoming_hunks() {
        let repo = setup();
        write(&repo.local.join("post.md"), "first\nbase\nlocal last\n");
        push_writer(&repo, "post.md", "remote first\nbase\nlast\n");

        safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();

        assert_eq!(
            fs::read_to_string(repo.local.join("post.md")).unwrap(),
            "remote first\nbase\nlocal last\n"
        );
        assert_eq!(status(&repo.local), "M post.md");
    }

    #[test]
    fn adopts_an_incoming_file_that_already_matches_the_local_copy() {
        let repo = setup();
        let shared = "first\nalready synced\nlast\n";
        write(&repo.local.join("post.md"), shared);
        push_writer(&repo, "post.md", shared);

        safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();

        assert_eq!(
            fs::read_to_string(repo.local.join("post.md")).unwrap(),
            shared
        );
        assert!(status(&repo.local).is_empty());
    }

    #[test]
    fn conflicting_hunks_refuse_without_changing_head_or_files() {
        let repo = setup();
        write(&repo.local.join("post.md"), "first\nlocal\nlast\n");
        push_writer(&repo, "post.md", "first\nremote\nlast\n");
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let before = fs::read(repo.local.join("post.md")).unwrap();
        let before_status = status(&repo.local);

        let error = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap_err();

        assert!(error.contains("conflict"));
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), head);
        assert_eq!(fs::read(repo.local.join("post.md")).unwrap(), before);
        assert_eq!(status(&repo.local), before_status);
        assert!(test_git(
            &repo.local,
            &["for-each-ref", "--format=%(refname)", RECOVERY_REF_PREFIX]
        )
        .is_empty());
    }

    #[test]
    fn staged_changes_refuse_without_fetching_or_mutating() {
        let repo = setup();
        write(&repo.local.join("post.md"), "staged\n");
        test_git(&repo.local, &["add", "post.md"]);
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);

        let error = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap_err();

        assert!(error.contains("staged changes"));
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            fs::read_to_string(repo.local.join("post.md")).unwrap(),
            "staged\n"
        );
    }

    #[test]
    fn ahead_only_branch_reports_a_noop() {
        let repo = setup();
        write(&repo.local.join("post.md"), "local commit\n");
        test_git(&repo.local, &["add", "post.md"]);
        test_git(&repo.local, &["commit", "-m", "local commit"]);
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);

        let result = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();

        assert!(result.contains("ahead of upstream"), "{result}");
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            fs::read_to_string(repo.local.join("post.md")).unwrap(),
            "local commit\n"
        );
        assert!(status(&repo.local).is_empty());
    }

    #[test]
    fn diverged_history_refuses_without_moving_the_branch() {
        let repo = setup();
        push_writer(&repo, "other.md", "remote\n");
        write(&repo.local.join("post.md"), "local commit\n");
        test_git(&repo.local, &["add", "post.md"]);
        test_git(&repo.local, &["commit", "-m", "local commit"]);
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);

        let error = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap_err();

        assert!(error.contains("diverged"));
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn preserves_a_modified_tracked_file_that_is_ignored() {
        let repo = setup();
        push_writer(&repo, "tracked-ignored.md", "base tracked content\n");
        safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();
        exclude(&repo.local, "tracked-ignored.md\n");
        write(
            &repo.local.join("tracked-ignored.md"),
            "local tracked draft\n",
        );
        push_writer(&repo, "other.md", "second remote change\n");

        let result = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();

        assert_eq!(
            fs::read_to_string(repo.local.join("tracked-ignored.md")).unwrap(),
            "local tracked draft\n"
        );
        assert!(status(&repo.local).contains("M tracked-ignored.md"));
        assert!(result.contains("Safety backup retained"), "{result}");
        let recovery_refs = test_git(
            &repo.local,
            &["for-each-ref", "--format=%(refname)", RECOVERY_REF_PREFIX],
        );
        assert_eq!(recovery_refs.lines().count(), 1, "{recovery_refs}");
    }

    #[test]
    fn refuses_an_incoming_file_that_would_overwrite_an_ignored_file() {
        let repo = setup();
        exclude(&repo.local, "incoming-ignored.md\n");
        write(
            &repo.local.join("incoming-ignored.md"),
            "private local content\n",
        );
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        push_writer(&repo, "incoming-ignored.md", "incoming content\n");

        let error = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap_err();

        assert!(error.contains("ignored local file"), "{error}");
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            fs::read_to_string(repo.local.join("incoming-ignored.md")).unwrap(),
            "private local content\n"
        );
    }

    #[test]
    fn detects_a_checked_out_branch_change() {
        let repo = setup();
        let branch = checked_out_branch(&repo.local).unwrap();
        let upstream_ref = configured_upstream(&repo.local).unwrap();
        let expected = checkout_state(&repo.local, branch, upstream_ref).unwrap();
        test_git(&repo.local, &["branch", "other"]);
        test_git(
            &repo.local,
            &["branch", "--set-upstream-to=origin/main", "other"],
        );
        test_git(&repo.local, &["switch", "other"]);

        let error = ensure_checkout_state(&repo.local, &expected).unwrap_err();

        assert!(error.contains("branch, HEAD, or upstream changed"));
    }

    #[cfg(unix)]
    #[test]
    fn fast_forward_does_not_run_repository_hooks() {
        let repo = setup();
        let hooks = repo.local.join(".git/test-hooks");
        fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("post-merge");
        write(&hook, "#!/bin/sh\nprintf 'ran' > hook-ran\n");
        let mut permissions = fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).unwrap();
        test_git(
            &repo.local,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        push_writer(&repo, "other.md", "remote hook test\n");

        safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap();

        assert!(!repo.local.join("hook-ran").exists());
        assert_eq!(
            fs::read_to_string(repo.local.join("other.md")).unwrap(),
            "remote hook test\n"
        );
    }

    #[test]
    fn refuses_a_custom_merge_attribute_on_a_draft() {
        let repo = setup();
        write(
            &repo.local.join(".gitattributes"),
            "post.md merge=keepRemote\n",
        );
        test_git(&repo.local, &["config", "merge.keepRemote.driver", "true"]);
        write(&repo.local.join("post.md"), "first\nlocal\nlast\n");
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        push_writer(&repo, "other.md", "remote attribute test\n");

        let error = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap_err();

        assert!(error.contains("active merge=keepRemote"));
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            fs::read_to_string(repo.local.join("post.md")).unwrap(),
            "first\nlocal\nlast\n"
        );
    }

    #[test]
    fn refuses_ident_attribute_on_a_draft_before_mutation() {
        let repo = setup();
        write(&repo.local.join(".gitattributes"), "post.md ident\n");
        write(
            &repo.local.join("post.md"),
            "first\nlocal $Id$ draft\nlast\n",
        );
        let before_head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        let before_post = fs::read(repo.local.join("post.md")).unwrap();
        let before_status = status(&repo.local);
        push_writer(&repo, "other.md", "remote ident test\n");

        let error = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap_err();

        assert!(error.contains("active ident=set"), "{error}");
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), before_head);
        assert_eq!(fs::read(repo.local.join("post.md")).unwrap(), before_post);
        assert_eq!(status(&repo.local), before_status);
    }

    #[test]
    fn refuses_autocrlf_for_dirty_reconciliation() {
        let repo = setup();
        test_git(&repo.local, &["config", "core.autocrlf", "true"]);
        write(&repo.local.join("post.md"), "first\nlocal\nlast\n");
        let head = test_git(&repo.local, &["rev-parse", "HEAD"]);
        push_writer(&repo, "other.md", "remote autocrlf test\n");

        let error = safe_git_pull_inner(repo.local.to_str().unwrap()).unwrap_err();

        assert!(error.contains("core.autocrlf"));
        assert_eq!(test_git(&repo.local, &["rev-parse", "HEAD"]), head);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_delete_through_a_symlinked_parent() {
        let repo = setup();
        let outside = repo._temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        write(&outside.join("victim.md"), "keep me\n");
        symlink(&outside, repo.local.join("linked-parent")).unwrap();

        let error = remove_worktree_path(&repo.local, "linked-parent/victim.md").unwrap_err();

        assert!(error.contains("symbolic link"));
        assert_eq!(
            fs::read_to_string(outside.join("victim.md")).unwrap(),
            "keep me\n"
        );
    }

    #[test]
    fn setup_uses_the_expected_origin() {
        let repo = setup();
        assert_eq!(
            test_git(&repo.local, &["remote", "get-url", "origin"]),
            repo.origin.to_string_lossy()
        );
    }
}
