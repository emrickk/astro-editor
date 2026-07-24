use chrono::Local;
use indexmap::IndexMap;
use pathdiff::diff_paths;
use serde_json::Value;
use serde_norway;
use std::path::{Path, PathBuf};
use tauri::{path::BaseDirectory, Manager};

/// Validates that a file path is within the project boundaries
///
/// This function prevents path traversal attacks by ensuring all file operations
/// stay within the current project root directory.
fn validate_project_path(file_path: &str, project_root: &str) -> Result<PathBuf, String> {
    let file_path = Path::new(file_path);
    let project_root = Path::new(project_root);

    // Resolve canonical paths to handle symlinks and .. traversal
    let canonical_file = file_path
        .canonicalize()
        .or_else(|_| {
            // If file doesn't exist, try to canonicalize parent and append filename
            if let (Some(parent), Some(filename)) = (file_path.parent(), file_path.file_name()) {
                parent.canonicalize().map(|p| p.join(filename))
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Invalid file path",
                ))
            }
        })
        .map_err(|_| "Invalid file path".to_string())?;

    let canonical_root = project_root
        .canonicalize()
        .map_err(|_| "Invalid project root".to_string())?;

    // Ensure file is within project bounds
    canonical_file
        .strip_prefix(&canonical_root)
        .map_err(|_| "File outside project directory".to_string())?;

    Ok(canonical_file)
}

/// Calculates the relative path from the current file to an asset
///
/// # Arguments
/// * `current_file_path` - Absolute path to the current markdown file
/// * `project_path` - Absolute path to the project root
/// * `project_relative_asset_path` - Path to the asset relative to project root (e.g., "src/assets/image.png")
///
/// # Returns
/// Relative path from the file's directory to the asset (e.g., "../../assets/image.png")
fn calculate_relative_path(
    current_file_path: &str,
    project_path: &str,
    project_relative_asset_path: &str,
) -> Result<String, String> {
    // Get directory containing the current markdown file
    let current_file = Path::new(current_file_path);
    let current_file_dir = current_file.parent().ok_or("Invalid current file path")?;

    // Get the asset's full path
    let asset_full_path = Path::new(project_path).join(project_relative_asset_path);

    // Calculate relative path from file directory to asset
    let relative_path = diff_paths(&asset_full_path, current_file_dir)
        .ok_or("Could not calculate relative path")?;

    // Convert to string with forward slashes (Markdown convention)
    let path_string = relative_path.to_string_lossy().replace('\\', "/");

    // Ensure ./ prefix for same-directory files (clearer than bare filename)
    let final_path = if path_string.starts_with("../") {
        path_string
    } else {
        format!("./{path_string}")
    };

    Ok(final_path)
}

#[tauri::command]
#[specta::specta]
pub async fn read_file(file_path: String, project_root: String) -> Result<String, String> {
    let validated_path = validate_project_path(&file_path, &project_root)?;
    std::fs::read_to_string(&validated_path).map_err(|e| format!("Failed to read file: {e}"))
}

#[tauri::command]
#[specta::specta]
pub async fn write_file(
    file_path: String,
    content: String,
    project_root: String,
) -> Result<(), String> {
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_root), "save the file")?;
    let validated_path = validate_project_path(&file_path, &project_root)?;
    std::fs::write(&validated_path, content).map_err(|e| format!("Failed to write file: {e}"))
}

#[tauri::command]
#[specta::specta]
pub async fn create_file(
    directory: String,
    filename: String,
    content: String,
    project_root: String,
) -> Result<String, String> {
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_root), "create the file")?;
    // Validate directory is within project
    let validated_dir = validate_project_path(&directory, &project_root)?;
    let path = validated_dir.join(&filename);

    // Double-check the final path is still within project bounds
    let final_path_str = path.to_string_lossy().to_string();
    let validated_final_path = validate_project_path(&final_path_str, &project_root)?;

    if validated_final_path.exists() {
        return Err("File already exists".to_string());
    }

    std::fs::write(&validated_final_path, content)
        .map_err(|e| format!("Failed to create file: {e}"))?;

    Ok(validated_final_path.to_string_lossy().to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn delete_file(file_path: String, project_root: String) -> Result<(), String> {
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_root), "delete the file")?;
    let validated_path = validate_project_path(&file_path, &project_root)?;
    std::fs::remove_file(&validated_path).map_err(|e| format!("Failed to delete file: {e}"))
}

#[derive(Debug, Clone, serde::Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFileTarget {
    pub file_path: String,
    pub expected_content: String,
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DeletedFileRecovery {
    pub original_path: String,
    pub recovery_path: String,
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFilesResult {
    pub recovery_directory: String,
    pub files: Vec<DeletedFileRecovery>,
}

#[derive(Debug, Clone, serde::Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct RestoreFileTarget {
    pub original_path: String,
    pub recovery_path: String,
}

#[derive(Debug)]
struct ValidatedDeleteFileTarget {
    path: PathBuf,
    expected_content: Vec<u8>,
}

#[derive(Debug)]
struct ValidatedRestoreFileTarget {
    original_path: PathBuf,
    recovery_path: PathBuf,
}

#[derive(Debug)]
struct QuarantinedDeleteTarget<'a> {
    target: &'a ValidatedDeleteFileTarget,
    quarantine_path: PathBuf,
}

fn validate_delete_file_targets(
    targets: Vec<DeleteFileTarget>,
    project_root: &str,
) -> Result<Vec<ValidatedDeleteFileTarget>, String> {
    if targets.is_empty() || targets.len() > 2 {
        return Err("Delete transaction requires one post or one translation pair".to_string());
    }

    let mut unique_paths = std::collections::HashSet::new();
    let mut validated = Vec::with_capacity(targets.len());
    for target in targets {
        let path = validate_project_path(&target.file_path, project_root)?;
        if !path.is_file() {
            return Err(format!("Post file not found: {}", path.display()));
        }
        if !unique_paths.insert(path.clone()) {
            return Err("Delete transaction contains the same post more than once".to_string());
        }
        validated.push(ValidatedDeleteFileTarget {
            path,
            expected_content: target.expected_content.into_bytes(),
        });
    }
    Ok(validated)
}

fn write_atomic_no_clobber(path: &Path, content: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| format!("Invalid destination path: {}", path.display()))?;
    let filename = path
        .file_name()
        .ok_or_else(|| format!("Invalid destination path: {}", path.display()))?
        .to_string_lossy();
    let temp_path = parent.join(format!(
        ".{filename}.astro-editor-restore-{}",
        uuid::Uuid::new_v4()
    ));

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|e| format!("Could not create temporary recovery file: {e}"))?;
    if let Err(error) = file.write_all(content).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!(
            "Could not finish recovery file {}: {error}",
            path.display()
        ));
    }
    drop(file);

    if let Err(error) = std::fs::hard_link(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "A different file now exists at {}. It was not overwritten.",
                path.display()
            )
        } else {
            format!(
                "Could not atomically restore {} without overwriting it: {error}",
                path.display()
            )
        });
    }
    std::fs::remove_file(&temp_path).map_err(|error| {
        format!(
            "Restored {}, but could not remove its temporary recovery link: {error}",
            path.display()
        )
    })
}

fn restore_quarantined_no_clobber(target: &QuarantinedDeleteTarget<'_>) -> Result<(), String> {
    if let Err(error) = std::fs::hard_link(&target.quarantine_path, &target.target.path) {
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "a newer file exists at {}; quarantined data remains at {}",
                target.target.path.display(),
                target.quarantine_path.display()
            )
        } else {
            format!(
                "could not restore {} from quarantine {}: {error}",
                target.target.path.display(),
                target.quarantine_path.display()
            )
        });
    }
    std::fs::remove_file(&target.quarantine_path).map_err(|error| {
        format!(
            "restored {}, but could not remove quarantine {}: {error}",
            target.target.path.display(),
            target.quarantine_path.display()
        )
    })
}

fn rollback_quarantined(quarantined: &[QuarantinedDeleteTarget<'_>]) -> Vec<String> {
    quarantined
        .iter()
        .rev()
        .filter_map(|target| restore_quarantined_no_clobber(target).err())
        .collect()
}

fn delete_transaction_error(
    message: String,
    quarantined: &[QuarantinedDeleteTarget<'_>],
) -> String {
    let rollback_errors = rollback_quarantined(quarantined);
    if rollback_errors.is_empty() {
        format!("{message}. Quarantined files were restored without overwriting newer files.")
    } else {
        format!(
            "{message}. Some quarantined files could not be restored automatically: {}",
            rollback_errors.join("; ")
        )
    }
}

fn delete_files_transaction_with<R, H>(
    targets: &[ValidatedDeleteFileTarget],
    recovery_directory: &Path,
    mut rename_to_quarantine: R,
    mut after_quarantine: H,
) -> Result<DeleteFilesResult, String>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
    H: FnMut(usize, &ValidatedDeleteFileTarget, &Path) -> Result<(), String>,
{
    std::fs::create_dir_all(recovery_directory)
        .map_err(|error| format!("Could not create the deletion recovery folder: {error}"))?;

    let mut quarantined = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        let filename = target
            .path
            .file_name()
            .ok_or_else(|| format!("Invalid post path: {}", target.path.display()))?
            .to_string_lossy();
        let quarantine_path = target.path.with_file_name(format!(
            ".{filename}.astro-editor-quarantine-{}",
            uuid::Uuid::new_v4()
        ));

        if let Err(error) = rename_to_quarantine(&target.path, &quarantine_path) {
            return Err(delete_transaction_error(
                format!(
                    "Could not atomically quarantine {}: {error}",
                    target.path.display()
                ),
                &quarantined,
            ));
        }
        quarantined.push(QuarantinedDeleteTarget {
            target,
            quarantine_path,
        });
        let current_quarantine = quarantined.last().expect("just pushed");

        if let Err(error) =
            after_quarantine(index, target, current_quarantine.quarantine_path.as_path())
        {
            return Err(delete_transaction_error(error, &quarantined));
        }

        let current = match std::fs::read(&current_quarantine.quarantine_path) {
            Ok(content) => content,
            Err(error) => {
                return Err(delete_transaction_error(
                    format!(
                        "Could not verify quarantined post {}: {error}",
                        current_quarantine.quarantine_path.display()
                    ),
                    &quarantined,
                ));
            }
        };
        if current != target.expected_content {
            return Err(delete_transaction_error(
                format!(
                    "{} changed after the deletion recovery snapshot was created",
                    target.path.display()
                ),
                &quarantined,
            ));
        }
    }

    let mut files = Vec::with_capacity(quarantined.len());
    for (index, quarantined_target) in quarantined.iter().enumerate() {
        let filename = quarantined_target
            .target
            .path
            .file_name()
            .expect("validated file has a name")
            .to_string_lossy();
        let recovery_path = recovery_directory.join(format!("{}-{filename}", index + 1));
        if let Err(error) =
            write_atomic_no_clobber(&recovery_path, &quarantined_target.target.expected_content)
        {
            return Err(delete_transaction_error(
                format!(
                    "Could not persist a recovery copy for {}: {error}",
                    quarantined_target.target.path.display()
                ),
                &quarantined,
            ));
        }
        files.push(DeletedFileRecovery {
            original_path: quarantined_target.target.path.to_string_lossy().to_string(),
            recovery_path: recovery_path.to_string_lossy().to_string(),
        });
    }

    let manifest = serde_json::json!({
        "deletedAt": Local::now().to_rfc3339(),
        "files": &files,
    });
    let manifest_content = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("Could not serialize deletion recovery manifest: {error}"))?;
    if let Err(error) =
        write_atomic_no_clobber(&recovery_directory.join("manifest.json"), &manifest_content)
    {
        return Err(delete_transaction_error(
            format!("Could not persist the deletion recovery manifest: {error}"),
            &quarantined,
        ));
    }

    for quarantined_target in &quarantined {
        std::fs::remove_file(&quarantined_target.quarantine_path).map_err(|error| {
            format!(
                "The post was safely copied to recovery, but quarantine cleanup failed at {}: {error}",
                quarantined_target.quarantine_path.display()
            )
        })?;
    }

    let replacements = targets
        .iter()
        .filter(|target| target.path.exists())
        .map(|target| target.path.display().to_string())
        .collect::<Vec<_>>();
    if !replacements.is_empty() {
        return Err(format!(
            "Newer file content appeared during deletion and was preserved at: {}. Recovery copies are in {}.",
            replacements.join(", "),
            recovery_directory.display()
        ));
    }

    Ok(DeleteFilesResult {
        recovery_directory: recovery_directory.to_string_lossy().to_string(),
        files,
    })
}

/// Atomically quarantines a post or verified translation pair before checking
/// its bytes. A concurrent atomic save can therefore be preserved or rejected,
/// but can never be unlinked by this deletion transaction.
#[tauri::command]
#[specta::specta]
pub async fn delete_files_transaction(
    app: tauri::AppHandle,
    targets: Vec<DeleteFileTarget>,
    project_root: String,
) -> Result<DeleteFilesResult, String> {
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_root), "delete the post")?;
    let validated = validate_delete_file_targets(targets, &project_root)?;
    let recovery_root = app
        .path()
        .resolve("deleted-posts", BaseDirectory::AppLocalData)
        .map_err(|error| format!("Could not resolve the deletion recovery folder: {error}"))?;
    std::fs::create_dir_all(&recovery_root)
        .map_err(|error| format!("Could not create the deletion recovery folder: {error}"))?;
    let recovery_directory = recovery_root.join(format!(
        "{}-{}",
        Local::now().format("%Y%m%d-%H%M%S"),
        uuid::Uuid::new_v4()
    ));

    delete_files_transaction_with(
        &validated,
        &recovery_directory,
        |source, quarantine| std::fs::rename(source, quarantine),
        |_, _, _| Ok(()),
    )
}

fn validate_restore_file_targets(
    targets: Vec<RestoreFileTarget>,
    project_root: &str,
    app_data_dir: &str,
) -> Result<Vec<ValidatedRestoreFileTarget>, String> {
    if targets.is_empty() || targets.len() > 2 {
        return Err("Restore transaction requires one post or one translation pair".to_string());
    }

    let mut originals = std::collections::HashSet::new();
    let mut recoveries = std::collections::HashSet::new();
    targets
        .into_iter()
        .map(|target| {
            let original_path = validate_project_path(&target.original_path, project_root)?;
            let recovery_path = validate_app_data_path(&target.recovery_path, app_data_dir)?;
            if !recovery_path.is_file() {
                return Err(format!(
                    "Recovery copy not found: {}",
                    recovery_path.display()
                ));
            }
            if !originals.insert(original_path.clone()) || !recoveries.insert(recovery_path.clone())
            {
                return Err("Restore transaction contains duplicate files".to_string());
            }
            Ok(ValidatedRestoreFileTarget {
                original_path,
                recovery_path,
            })
        })
        .collect()
}

fn restore_files_transaction_inner(targets: &[ValidatedRestoreFileTarget]) -> Result<(), String> {
    for target in targets {
        let content = std::fs::read(&target.recovery_path).map_err(|error| {
            format!(
                "Could not read recovery copy {}: {error}",
                target.recovery_path.display()
            )
        })?;

        match std::fs::read(&target.original_path) {
            Ok(existing) if existing == content => continue,
            Ok(_) => {
                return Err(format!(
                    "A different file now exists at {}. It was not overwritten. Recovery copies remain available.",
                    target.original_path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not inspect restore destination {}: {error}",
                    target.original_path.display()
                ));
            }
        }

        write_atomic_no_clobber(&target.original_path, &content).map_err(|error| {
            format!(
                "Could not restore {}: {error}. Recovery copies remain available.",
                target.original_path.display()
            )
        })?;
    }
    Ok(())
}

/// Restores recovery files through atomic no-clobber links. Existing files are
/// never truncated or replaced, even if another application saves concurrently.
#[tauri::command]
#[specta::specta]
pub async fn restore_files_transaction(
    app: tauri::AppHandle,
    targets: Vec<RestoreFileTarget>,
    project_root: String,
) -> Result<(), String> {
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_root), "restore the post")?;
    let app_data_dir = app
        .path()
        .resolve("", BaseDirectory::AppLocalData)
        .map_err(|error| format!("Could not resolve app data directory: {error}"))?
        .to_string_lossy()
        .to_string();
    let validated = validate_restore_file_targets(targets, &project_root, &app_data_dir)?;
    restore_files_transaction_inner(&validated)
}

#[tauri::command]
#[specta::specta]
pub async fn rename_file(
    old_path: String,
    new_path: String,
    project_root: String,
) -> Result<(), String> {
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_root), "rename the file")?;
    let validated_old_path = validate_project_path(&old_path, &project_root)?;
    let validated_new_path = validate_project_path(&new_path, &project_root)?;
    std::fs::rename(&validated_old_path, &validated_new_path)
        .map_err(|e| format!("Failed to rename file: {e}"))
}

/// Convert a string to kebab case
fn to_kebab_case(s: &str) -> String {
    let parts: Vec<&str> = s.split('.').collect();
    let extension = if parts.len() > 1 { parts.last() } else { None };

    let filename = if parts.len() > 1 {
        parts[..parts.len() - 1].join(".")
    } else {
        s.to_string()
    };

    // Convert filename to kebab case
    let kebab_filename = filename
        .to_lowercase()
        .replace([' ', '_'], "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    // Reconstruct with extension if present
    if let Some(ext) = extension {
        format!("{}.{}", kebab_filename, ext.to_lowercase())
    } else {
        kebab_filename
    }
}

#[tauri::command]
#[specta::specta]
pub async fn copy_file_to_assets(
    source_path: String,
    project_path: String,
    collection: String,
    current_file_path: String,
    use_relative_paths: bool,
) -> Result<String, String> {
    copy_file_to_assets_with_override(
        source_path,
        project_path,
        collection,
        None,
        current_file_path,
        use_relative_paths,
    )
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn copy_file_to_assets_with_override(
    source_path: String,
    project_path: String,
    collection: String,
    assets_directory: Option<String>,
    current_file_path: String,
    use_relative_paths: bool,
) -> Result<String, String> {
    use std::fs;

    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_path), "copy the asset")?;

    // Validate project path
    let validated_project_root = Path::new(&project_path)
        .canonicalize()
        .map_err(|_| "Invalid project root".to_string())?;

    // Create the assets directory structure (use override if provided)
    let assets_base = if let Some(assets_override) = assets_directory {
        validated_project_root.join(assets_override)
    } else {
        validated_project_root.join("src").join("assets")
    };

    let assets_dir = assets_base.join(&collection);

    fs::create_dir_all(&assets_dir)
        .map_err(|e| format!("Failed to create assets directory: {e}"))?;

    // Get the source file info
    let source = PathBuf::from(&source_path);
    let file_name = source
        .file_name()
        .ok_or("Invalid source file path")?
        .to_string_lossy();

    // Extract extension
    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    // Create the base filename with date prefix
    let date_prefix = Local::now().format("%Y-%m-%d").to_string();
    let name_without_ext = file_name.trim_end_matches(&format!(".{extension}"));
    let kebab_name = to_kebab_case(name_without_ext);

    // Build the new filename
    let mut base_name = format!("{date_prefix}-{kebab_name}");
    if !extension.is_empty() {
        base_name.push('.');
        base_name.push_str(extension);
    }

    // Atomically find available filename and copy file
    // This prevents TOCTOU race conditions where multiple simultaneous calls
    // could all check existence and decide to use the same filename
    let mut final_path = assets_dir.join(&base_name);
    let mut counter = 1;
    const MAX_ATTEMPTS: u32 = 100;

    let validated_final_path = loop {
        // Validate the candidate path is within project bounds
        let final_path_str = final_path.to_string_lossy().to_string();
        let validated_path = validate_project_path(&final_path_str, &project_path)?;

        // Try to create the destination file atomically using create_new()
        // This fails if the file already exists, preventing race conditions
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&validated_path)
        {
            Ok(_) => {
                // File created successfully, now copy the content
                // Note: We created an empty file, so we need to copy over it
                fs::copy(&source_path, &validated_path)
                    .map_err(|e| format!("Failed to copy file content: {e}"))?;
                break validated_path;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // File exists, try with counter suffix
                if counter > MAX_ATTEMPTS {
                    return Err(format!(
                        "Could not find available filename after {MAX_ATTEMPTS} attempts"
                    ));
                }

                let name_with_counter = if extension.is_empty() {
                    format!("{date_prefix}-{kebab_name}-{counter}")
                } else {
                    format!("{date_prefix}-{kebab_name}-{counter}.{extension}")
                };
                final_path = assets_dir.join(name_with_counter);
                counter += 1; // Increment for next iteration
            }
            Err(e) => {
                // Other error (permissions, disk full, etc.)
                return Err(format!("Failed to create file: {e}"));
            }
        }
    };

    // Get the path relative to project root
    let project_relative_path = validated_final_path
        .strip_prefix(&validated_project_root)
        .map_err(|_| "Failed to create relative path")?
        .to_string_lossy()
        .to_string();

    // Convert to appropriate path style based on setting
    let final_path = if use_relative_paths {
        calculate_relative_path(&current_file_path, &project_path, &project_relative_path)?
    } else {
        // Absolute path from project root (legacy behavior)
        format!("/{}", project_relative_path.replace('\\', "/"))
    };

    Ok(final_path)
}

#[derive(serde::Serialize, serde::Deserialize, specta::Type)]
pub struct MarkdownContent {
    pub frontmatter: IndexMap<String, Value>,
    pub content: String,
    pub raw_frontmatter: String,
    pub imports: String, // MDX imports to hide from editor
}

#[tauri::command]
#[specta::specta]
pub async fn parse_markdown_content(
    file_path: String,
    project_root: String,
) -> Result<MarkdownContent, String> {
    let validated_path = validate_project_path(&file_path, &project_root)?;
    let content = std::fs::read_to_string(&validated_path)
        .map_err(|e| format!("Failed to read file: {e}"))?;

    parse_frontmatter(&content)
}

#[tauri::command]
#[specta::specta]
pub async fn update_frontmatter(
    file_path: String,
    frontmatter: IndexMap<String, Value>,
    project_root: String,
) -> Result<(), String> {
    let _mutation_guard = super::project_mutation::try_lock_for_write(
        Path::new(&project_root),
        "update frontmatter",
    )?;
    let validated_path = validate_project_path(&file_path, &project_root)?;
    let content = std::fs::read_to_string(&validated_path)
        .map_err(|e| format!("Failed to read file: {e}"))?;

    let parsed = parse_frontmatter(&content)?;
    let new_content = rebuild_markdown_with_frontmatter_and_imports(
        &frontmatter,
        &parsed.imports,
        &parsed.content,
    )?;

    std::fs::write(&validated_path, new_content).map_err(|e| format!("Failed to write file: {e}"))
}

#[tauri::command]
#[specta::specta]
pub async fn save_markdown_content(
    file_path: String,
    frontmatter: Option<IndexMap<String, Value>>,
    raw_frontmatter: Option<String>,
    content: String,
    imports: String,
    schema_field_order: Option<Vec<String>>,
    project_root: String,
) -> Result<(), String> {
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_root), "save the post")?;
    let validated_path = validate_project_path(&file_path, &project_root)?;

    let new_content = match (frontmatter, raw_frontmatter) {
        // Frontmatter was edited. Prefer a format-preserving merge into the
        // original raw block (keeps field order, quoting style, and comments
        // for untouched fields); fall back to the full ordered rebuild when
        // the original can't be merged safely.
        (Some(fm), raw) => {
            let mut normalized = fm.clone();
            normalize_dates(&mut normalized);
            let merged = raw
                .as_deref()
                .filter(|r| !r.trim().is_empty() && !normalized.is_empty())
                .and_then(|r| merge_frontmatter_preserving_format(r, &normalized));
            match merged {
                Some(merged_yaml) => {
                    rebuild_markdown_with_raw_frontmatter(&merged_yaml, &imports, &content)?
                }
                None => rebuild_markdown_with_frontmatter_and_imports_ordered(
                    &fm,
                    &imports,
                    &content,
                    schema_field_order,
                )?,
            }
        }
        // Frontmatter unchanged - preserve original (non-empty)
        (None, Some(ref raw)) if !raw.trim().is_empty() => {
            rebuild_markdown_with_raw_frontmatter(raw, &imports, &content)?
        }
        // No frontmatter at all (None, None, or empty string)
        _ => rebuild_markdown_content_only(&imports, &content)?,
    };

    std::fs::write(&validated_path, new_content).map_err(|e| format!("Failed to write file: {e}"))
}

pub fn parse_frontmatter_internal(content: &str) -> Result<MarkdownContent, String> {
    parse_frontmatter(content)
}

fn parse_frontmatter(content: &str) -> Result<MarkdownContent, String> {
    // Track if original content ends with newline - lines() drops this info
    let original_ends_with_newline = content.ends_with('\n');
    let lines: Vec<&str> = content.lines().collect();

    // Check if file starts with frontmatter
    if lines.is_empty() || lines[0] != "---" {
        // No frontmatter, but might have imports at the top
        let (imports, mut body_content) = extract_imports_from_content(&lines);
        // Preserve trailing newline that lines() dropped.
        // lines() always drops exactly one trailing \n, so always add one back.
        if original_ends_with_newline && !body_content.is_empty() {
            body_content.push('\n');
        }
        return Ok(MarkdownContent {
            frontmatter: IndexMap::new(),
            content: body_content,
            raw_frontmatter: String::new(),
            imports,
        });
    }

    // Find the closing ---
    let mut frontmatter_end = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if *line == "---" {
            frontmatter_end = Some(i);
            break;
        }
    }

    let Some(end_index) = frontmatter_end else {
        return Err("Frontmatter not properly closed with '---'".to_string());
    };

    // Extract frontmatter lines (between the --- markers)
    let frontmatter_lines: Vec<&str> = lines[1..end_index].to_vec();
    let raw_frontmatter = frontmatter_lines.join("\n");

    // Parse YAML frontmatter
    let frontmatter: IndexMap<String, Value> = if raw_frontmatter.trim().is_empty() {
        IndexMap::new()
    } else {
        parse_yaml_to_json(&raw_frontmatter)?
    };

    // Extract content after frontmatter and process imports
    let content_start = end_index + 1;
    let remaining_lines: Vec<&str> = if content_start < lines.len() {
        lines[content_start..].to_vec()
    } else {
        vec![]
    };

    let (imports, mut body_content) = extract_imports_from_content(&remaining_lines);

    // Preserve trailing newline that lines() dropped.
    // lines() always drops exactly one trailing \n, so always add one back.
    if original_ends_with_newline && !body_content.is_empty() {
        body_content.push('\n');
    }

    Ok(MarkdownContent {
        frontmatter,
        content: body_content,
        raw_frontmatter,
        imports,
    })
}

/// Detects if a line is the start of a Markdown block (not an import continuation)
fn is_markdown_block_start(line: &str) -> bool {
    let trimmed = line.trim();

    // Check for common Markdown block starts
    trimmed.starts_with('#')       // Headings
        || trimmed.starts_with('>')    // Blockquotes
        || trimmed.starts_with('-')    // Lists
        || trimmed.starts_with('*')    // Lists
        || trimmed.starts_with('+')    // Lists
        || trimmed.starts_with("```")  // Code fences
        || trimmed.starts_with('<')    // HTML tags/JSX
        || is_numbered_list_start(trimmed) // Numbered lists (1., 2., etc)
}

/// Checks if a line starts with a numbered list (e.g., "1. ", "42. ")
fn is_numbered_list_start(line: &str) -> bool {
    let mut chars = line.chars();

    // Must start with a digit
    match chars.next() {
        Some(c) if c.is_numeric() => {}
        _ => return false,
    };

    // Consume any additional digits and look for a period
    loop {
        match chars.next() {
            Some(c) if c.is_numeric() => continue,
            Some('.') => return true, // Found the pattern: digit(s) followed by period
            _ => return false,
        }
    }
}

/// Check if a trimmed line starts an import or export statement
fn is_import_line(trimmed: &str) -> bool {
    trimmed.starts_with("import ") || trimmed.starts_with("export ")
}

/// Check if a line has an import terminator (semicolon or closing quote)
fn has_import_terminator(trimmed: &str) -> bool {
    trimmed.ends_with(';') || trimmed.ends_with("';") || trimmed.ends_with("\";")
}

/// Check if a line is a continuation of a multi-line import
#[allow(dead_code)] // Kept for documentation and potential future use
fn is_import_continuation(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && !is_markdown_block_start(trimmed) && !has_import_terminator(trimmed)
}

/// Check if there are more imports after empty lines (look-ahead logic)
/// Returns true if we should skip the empty line and continue collecting imports
fn should_skip_empty_line(lines: &[&str], current_idx: usize) -> bool {
    let mut next_idx = current_idx + 1;
    while next_idx < lines.len() && lines[next_idx].trim().is_empty() {
        next_idx += 1;
    }

    if next_idx < lines.len() {
        let next_line = lines[next_idx].trim();
        is_import_line(next_line)
    } else {
        false
    }
}

fn extract_imports_from_content(lines: &[&str]) -> (String, String) {
    let mut imports = Vec::new();
    let mut content_start_idx = 0;

    // Skip empty lines at the beginning
    while content_start_idx < lines.len() && lines[content_start_idx].trim().is_empty() {
        content_start_idx += 1;
    }

    // Extract import statements
    while content_start_idx < lines.len() {
        let line = lines[content_start_idx].trim();

        // Check if this line is an import statement
        if is_import_line(line) {
            imports.push(lines[content_start_idx]);
            content_start_idx += 1;

            // Handle multi-line imports until a trailing semicolon line
            while content_start_idx < lines.len() {
                let current_line = lines[content_start_idx].trim();

                // Empty line or markdown block - stop continuation
                if current_line.is_empty() || is_markdown_block_start(current_line) {
                    break;
                }

                // This is a continuation of the previous import - add it first
                imports.push(lines[content_start_idx]);
                content_start_idx += 1;

                // Stop if this line has a terminator
                if has_import_terminator(current_line) {
                    break;
                }
            }
        } else if line.is_empty() {
            // Check if there are more imports after this empty line (look-ahead)
            if should_skip_empty_line(lines, content_start_idx) {
                // More imports coming, skip empty line
                content_start_idx += 1;
            } else {
                // No more imports, this empty line separates imports from content
                break;
            }
        } else {
            // Found non-import content, stop processing imports
            break;
        }
    }

    // Skip any remaining empty lines after imports
    while content_start_idx < lines.len() && lines[content_start_idx].trim().is_empty() {
        content_start_idx += 1;
    }

    let imports_string = imports.join("\n");
    let content_lines: Vec<&str> = if content_start_idx < lines.len() {
        lines[content_start_idx..].to_vec()
    } else {
        vec![]
    };
    let content_string = content_lines.join("\n");

    (imports_string, content_string)
}

/// Normalizes ISO datetime strings to date-only format recursively
/// Converts "2024-01-15T00:00:00Z" -> "2024-01-15"
fn normalize_dates(frontmatter: &mut IndexMap<String, Value>) {
    for (_, value) in frontmatter.iter_mut() {
        normalize_value(value);
    }
}

/// Recursively normalizes dates in a Value
fn normalize_value(value: &mut Value) {
    match value {
        Value::String(s)
            if s.len() > 10 && s.contains('T') && (s.ends_with('Z') || s.contains('+')) =>
        {
            // If string looks like ISO datetime, extract date part
            if let Some(date_part) = s.split('T').next() {
                if date_part.len() == 10 && date_part.matches('-').count() == 2 {
                    *s = date_part.to_string();
                }
            }
        }
        Value::Object(obj) => {
            for (_, v) in obj.iter_mut() {
                normalize_value(v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                normalize_value(v);
            }
        }
        _ => {}
    }
}

/// Builds an ordered IndexMap with schema fields first, then remaining fields alphabetically
fn build_ordered_frontmatter(
    frontmatter: IndexMap<String, Value>,
    schema_field_order: Option<Vec<String>>,
) -> IndexMap<String, Value> {
    let mut ordered = IndexMap::new();

    // First, add schema fields in order
    if let Some(schema_order) = schema_field_order {
        for key in schema_order {
            if let Some(value) = frontmatter.get(&key) {
                ordered.insert(key, value.clone());
            }
        }
    }

    // Then add remaining fields alphabetically
    let mut remaining: Vec<_> = frontmatter
        .iter()
        .filter(|(k, _)| !ordered.contains_key(*k))
        .collect();
    remaining.sort_by_key(|(k, _)| *k);

    for (key, value) in remaining {
        ordered.insert(key.clone(), value.clone());
    }

    ordered
}

/// Parse YAML string to IndexMap using serde_norway
fn parse_yaml_to_json(yaml_str: &str) -> Result<IndexMap<String, Value>, String> {
    serde_norway::from_str(yaml_str).map_err(|e| format!("Failed to parse YAML: {e}"))
}

fn rebuild_markdown_with_frontmatter_and_imports(
    frontmatter: &IndexMap<String, Value>,
    imports: &str,
    content: &str,
) -> Result<String, String> {
    rebuild_markdown_with_frontmatter_and_imports_ordered(frontmatter, imports, content, None)
}

/// Serialize a value to YAML format with proper indentation
fn rebuild_markdown_with_frontmatter_and_imports_ordered(
    frontmatter: &IndexMap<String, Value>,
    imports: &str,
    content: &str,
    schema_field_order: Option<Vec<String>>,
) -> Result<String, String> {
    let mut result = String::new();

    // Add frontmatter if present
    if !frontmatter.is_empty() {
        // Build ordered frontmatter (schema fields first, then alphabetical)
        let ordered = build_ordered_frontmatter(frontmatter.clone(), schema_field_order);

        // Normalize dates (ISO datetime -> date-only)
        let mut normalized = ordered;
        normalize_dates(&mut normalized);

        // Serialize to YAML using serde_norway
        result.push_str("---\n");
        let yaml = serde_norway::to_string(&normalized)
            .map_err(|e| format!("Failed to serialize YAML: {e}"))?;
        result.push_str(&yaml);
        result.push_str("---\n");
    }

    // Add imports if present
    if !imports.trim().is_empty() {
        if !frontmatter.is_empty() {
            result.push('\n');
        }
        result.push_str(imports);
        if !imports.ends_with('\n') {
            result.push('\n');
        }
    }

    // Add content if present
    if !content.is_empty() {
        if !frontmatter.is_empty() || !imports.trim().is_empty() {
            result.push('\n');
        }
        result.push_str(content);
    }

    Ok(result)
}

/// Splits a raw frontmatter line into a top-level `key` and the rest, when it
/// looks like a plain top-level mapping entry (`key: ...` at column 0).
fn split_top_level_key(line: &str) -> Option<(&str, &str)> {
    let first = line.chars().next()?;
    if first.is_whitespace() || first == '#' || first == '-' {
        return None;
    }
    let colon = line.find(':')?;
    let key = &line[..colon];
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return None;
    }
    // `key:value` without a space is not a mapping entry in YAML
    let rest = &line[colon + 1..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((key, rest))
}

/// Serializes one key/value pair as a YAML mapping entry, matching the
/// original line's single-quote style for plain string values.
fn serialize_yaml_entry(
    key: &str,
    value: &Value,
    original_first_line: Option<&str>,
) -> Result<String, String> {
    if let (Value::String(s), Some(orig)) = (value, original_first_line) {
        let after_colon = orig
            .split_once(':')
            .map(|(_, rest)| rest.trim_start())
            .unwrap_or("");
        if after_colon.starts_with('\'') && !s.contains('\n') {
            return Ok(format!("{}: '{}'\n", key, s.replace('\'', "''")));
        }
    }
    let mut single: IndexMap<String, Value> = IndexMap::new();
    single.insert(key.to_string(), value.clone());
    serde_norway::to_string(&single).map_err(|e| format!("Failed to serialize YAML: {e}"))
}

/// One top-level frontmatter field with its original lines, plus any comment
/// or blank lines that directly precede it.
struct RawSegment {
    key: String,
    leading: Vec<String>,
    lines: Vec<String>,
}

/// Merges edited frontmatter values into the original raw frontmatter text,
/// preserving the file's own field order, quoting style, comments, and blank
/// lines for fields whose values did not change. Changed fields are
/// re-serialized in place, removed fields are dropped, and new fields are
/// appended at the end.
///
/// Returns None when the original doesn't fit the simple top-level-mapping
/// model or when the merged text does not round-trip to exactly the intended
/// values; the caller then falls back to the full rebuild, so this is purely
/// a formatting improvement, never a correctness risk.
fn merge_frontmatter_preserving_format(
    raw_frontmatter: &str,
    new_frontmatter: &IndexMap<String, Value>,
) -> Option<String> {
    let original = parse_yaml_to_json(raw_frontmatter).ok()?;

    // Segment the raw text by top-level keys
    let mut segments: Vec<RawSegment> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut has_current = false;
    for line in raw_frontmatter.lines() {
        if let Some((key, _)) = split_top_level_key(line) {
            segments.push(RawSegment {
                key: key.to_string(),
                leading: std::mem::take(&mut pending),
                lines: vec![line.to_string()],
            });
            has_current = true;
        } else if line.trim().is_empty() || line.trim_start().starts_with('#') {
            pending.push(line.to_string());
        } else if has_current
            && (line.starts_with(' ')
                || line.starts_with('\t')
                || line.starts_with("- ")
                || line == "-")
        {
            let current = segments.last_mut()?;
            current.lines.append(&mut pending);
            current.lines.push(line.to_string());
        } else {
            // Anything else (flow mappings, quoted keys, unindented
            // continuations) is outside the simple model
            return None;
        }
    }

    // Rebuild: original order first, changed values re-serialized in place
    let mut out: Vec<String> = Vec::new();
    for segment in &segments {
        let Some(new_value) = new_frontmatter.get(&segment.key) else {
            continue; // field removed
        };
        out.extend(segment.leading.iter().cloned());
        if original.get(&segment.key) == Some(new_value) {
            out.extend(segment.lines.iter().cloned());
        } else {
            let entry = serialize_yaml_entry(
                &segment.key,
                new_value,
                segment.lines.first().map(|s| s.as_str()),
            )
            .ok()?;
            out.extend(entry.lines().map(String::from));
        }
    }

    // Append fields that are new to this file, in panel order
    for (key, value) in new_frontmatter {
        if !segments.iter().any(|s| &s.key == key) {
            let entry = serialize_yaml_entry(key, value, None).ok()?;
            out.extend(entry.lines().map(String::from));
        }
    }

    // Trailing comments or blank lines
    out.append(&mut pending);

    let mut merged = out.join("\n");
    merged.push('\n');

    // Round-trip guard: the merged text must parse back to exactly the
    // intended values, or we don't use it.
    let reparsed = parse_yaml_to_json(&merged).ok()?;
    if reparsed != *new_frontmatter {
        return None;
    }

    Some(merged)
}

/// Rebuild markdown file preserving original raw frontmatter (no normalization)
fn rebuild_markdown_with_raw_frontmatter(
    raw_frontmatter: &str,
    imports: &str,
    content: &str,
) -> Result<String, String> {
    let mut result = String::new();

    // Add frontmatter with raw content (preserves original formatting)
    result.push_str("---\n");
    result.push_str(raw_frontmatter);
    if !raw_frontmatter.ends_with('\n') {
        result.push('\n');
    }
    result.push_str("---\n");

    // Add imports if present
    if !imports.trim().is_empty() {
        result.push('\n');
        result.push_str(imports);
        if !imports.ends_with('\n') {
            result.push('\n');
        }
    }

    // Add content if present
    if !content.is_empty() {
        result.push('\n');
        result.push_str(content);
    }

    Ok(result)
}

/// Rebuild markdown file with no frontmatter (content only)
fn rebuild_markdown_content_only(imports: &str, content: &str) -> Result<String, String> {
    let mut result = String::new();

    // Add imports if present
    if !imports.trim().is_empty() {
        result.push_str(imports);
        if !imports.ends_with('\n') {
            result.push('\n');
        }
    }

    // Add content if present
    if !content.is_empty() {
        if !imports.trim().is_empty() {
            result.push('\n');
        }
        result.push_str(content);
    }

    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub async fn save_recovery_data(app: tauri::AppHandle, data: Value) -> Result<(), String> {
    let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let filename = data
        .get("fileName")
        .and_then(|v| v.as_str())
        .unwrap_or("untitled");

    // Create recovery directory
    let recovery_dir = app
        .path()
        .resolve("recovery", BaseDirectory::AppLocalData)
        .map_err(|e| format!("Failed to resolve recovery directory: {e}"))?;

    std::fs::create_dir_all(&recovery_dir)
        .map_err(|e| format!("Failed to create recovery directory: {e}"))?;

    // Save JSON file with complete state
    let json_filename = format!("{timestamp}-{filename}.recovery.json");
    let json_path = recovery_dir.join(&json_filename);
    let json_content = serde_json::to_string_pretty(&data)
        .map_err(|e| format!("Failed to serialize recovery data: {e}"))?;

    std::fs::write(&json_path, json_content)
        .map_err(|e| format!("Failed to write recovery JSON: {e}"))?;

    // Save Markdown file with just the content
    let md_filename = format!("{timestamp}-{filename}.recovery.md");
    let md_path = recovery_dir.join(&md_filename);
    let md_content = data
        .get("editorContent")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    std::fs::write(&md_path, md_content)
        .map_err(|e| format!("Failed to write recovery Markdown: {e}"))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn save_crash_report(app: tauri::AppHandle, report: Value) -> Result<(), String> {
    let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();

    // Create crash-reports directory
    let crash_dir = app
        .path()
        .resolve("crash-reports", BaseDirectory::AppLocalData)
        .map_err(|e| format!("Failed to resolve crash reports directory: {e}"))?;

    std::fs::create_dir_all(&crash_dir)
        .map_err(|e| format!("Failed to create crash reports directory: {e}"))?;

    // Save crash report
    let filename = format!("{timestamp}-crash.json");
    let file_path = crash_dir.join(&filename);
    let content = serde_json::to_string_pretty(&report)
        .map_err(|e| format!("Failed to serialize crash report: {e}"))?;

    std::fs::write(&file_path, content)
        .map_err(|e| format!("Failed to write crash report: {e}"))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn get_app_data_dir(app: tauri::AppHandle) -> Result<String, String> {
    let app_data_dir = app
        .path()
        .resolve("", BaseDirectory::AppLocalData)
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))?;

    Ok(app_data_dir.to_string_lossy().to_string())
}

/// Validates that a file path is within the app data directory
///
/// This function prevents path traversal attacks for app data operations
/// by ensuring all file operations stay within the app's data directory.
/// Creates the app data directory if it doesn't exist.
fn validate_app_data_path(file_path: &str, app_data_dir: &str) -> Result<PathBuf, String> {
    use log::info;

    let app_data_dir = Path::new(app_data_dir);

    // Create app data directory if it doesn't exist
    if !app_data_dir.exists() {
        info!(
            "Nevertheless Editor [PROJECT_REGISTRY] Creating app data directory: {}",
            app_data_dir.display()
        );
        std::fs::create_dir_all(app_data_dir)
            .map_err(|e| format!("Failed to create app data directory: {e}"))?;
        info!("Nevertheless Editor [PROJECT_REGISTRY] App data directory created successfully");
    }

    // If file_path is relative (just a filename), join it with app_data_dir
    let file_path = if Path::new(file_path).is_absolute() {
        Path::new(file_path).to_path_buf()
    } else {
        app_data_dir.join(file_path)
    };

    // Resolve canonical paths to handle symlinks and .. traversal
    let canonical_file = file_path
        .canonicalize()
        .or_else(|_| {
            // If file doesn't exist, try to canonicalize parent and append filename
            if let (Some(parent), Some(filename)) = (file_path.parent(), file_path.file_name()) {
                // Ensure parent directory exists
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    info!(
                        "Nevertheless Editor [PROJECT_REGISTRY] Creating parent directory: {}",
                        parent.display()
                    );
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("Failed to create parent directory: {e}"),
                        ));
                    }
                }
                parent.canonicalize().map(|p| p.join(filename))
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Invalid file path",
                ))
            }
        })
        .map_err(|e| format!("Invalid file path: {e}"))?;

    let canonical_app_data = app_data_dir
        .canonicalize()
        .map_err(|e| format!("Invalid app data directory: {e}"))?;

    // Ensure file is within app data bounds
    canonical_file
        .strip_prefix(&canonical_app_data)
        .map_err(|_| "File outside app data directory".to_string())?;

    Ok(canonical_file)
}

#[tauri::command]
#[specta::specta]
pub async fn write_app_data_file(
    app: tauri::AppHandle,
    file_path: String,
    content: String,
) -> Result<(), String> {
    let app_data_dir = app
        .path()
        .resolve("", BaseDirectory::AppLocalData)
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))?
        .to_string_lossy()
        .to_string();

    let validated_path = validate_app_data_path(&file_path, &app_data_dir)?;

    // Ensure parent directory exists
    if let Some(parent) = validated_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create parent directory: {e}"))?;
    }

    std::fs::write(&validated_path, content)
        .map_err(|e| format!("Failed to write app data file: {e}"))
}

#[tauri::command]
#[specta::specta]
pub async fn read_app_data_file(
    app: tauri::AppHandle,
    file_path: String,
) -> Result<String, String> {
    let app_data_dir = app
        .path()
        .resolve("", BaseDirectory::AppLocalData)
        .map_err(|e| format!("Failed to resolve app data directory: {e}"))?
        .to_string_lossy()
        .to_string();

    let validated_path = validate_app_data_path(&file_path, &app_data_dir)?;

    std::fs::read_to_string(&validated_path)
        .map_err(|e| format!("Failed to read app data file: {e}"))
}

#[tauri::command]
#[specta::specta]
pub async fn read_file_content(file_path: String, project_root: String) -> Result<String, String> {
    let validated_path = validate_project_path(&file_path, &project_root)?;
    std::fs::read_to_string(&validated_path).map_err(|e| format!("Failed to read file: {e}"))
}

#[tauri::command]
#[specta::specta]
pub async fn write_file_content(
    file_path: String,
    content: String,
    project_root: String,
) -> Result<(), String> {
    let _mutation_guard =
        super::project_mutation::try_lock_for_write(Path::new(&project_root), "write the file")?;
    let validated_path = validate_project_path(&file_path, &project_root)?;

    // Create parent directories if they don't exist
    if let Some(parent) = validated_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directories: {e}"))?;
    }

    std::fs::write(&validated_path, content).map_err(|e| format!("Failed to write file: {e}"))
}

#[tauri::command]
#[specta::specta]
pub async fn create_directory(path: String, project_root: String) -> Result<(), String> {
    let _mutation_guard = super::project_mutation::try_lock_for_write(
        Path::new(&project_root),
        "create the directory",
    )?;
    let validated_path = validate_project_path(&path, &project_root)?;
    std::fs::create_dir_all(&validated_path).map_err(|e| format!("Failed to create directory: {e}"))
}

/// Checks if a file path is within the project directory
///
/// # Arguments
/// * `file_path` - The absolute path to check
/// * `project_path` - The absolute path to the project root
///
/// # Returns
/// True if the file is within the project, false otherwise
#[tauri::command]
#[specta::specta]
pub async fn is_path_in_project(file_path: String, project_path: String) -> bool {
    let file = Path::new(&file_path);
    let project = Path::new(&project_path);

    file.canonicalize()
        .ok()
        .and_then(|f| project.canonicalize().ok().map(|p| f.starts_with(p)))
        .unwrap_or(false)
}

/// Gets the relative path of a file from the project root
///
/// # Arguments
/// * `file_path` - The absolute path to the file
/// * `project_path` - The absolute path to the project root
///
/// # Returns
/// The relative path from project root, or an error if the file is not in the project
#[tauri::command]
#[specta::specta]
pub async fn get_relative_path(
    file_path: String,
    project_path: String,
    current_file_path: String,
    use_relative_paths: bool,
) -> Result<String, String> {
    let file = Path::new(&file_path)
        .canonicalize()
        .map_err(|e| format!("Invalid file path: {e}"))?;
    let project = Path::new(&project_path)
        .canonicalize()
        .map_err(|e| format!("Invalid project path: {e}"))?;

    let project_relative_path = file
        .strip_prefix(&project)
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|_| "Path not in project".to_string())?;

    // Convert to appropriate path style based on setting
    let final_path = if use_relative_paths {
        calculate_relative_path(&current_file_path, &project_path, &project_relative_path)?
    } else {
        // Absolute path from project root (legacy behavior)
        format!("/{}", project_relative_path.replace('\\', "/"))
    };

    Ok(final_path)
}

/// Resolves an image path from markdown to an absolute filesystem path
///
/// Handles both absolute paths (starting with /) and relative paths (starting with ./ or ../)
/// For absolute paths: treats them as relative to project root
/// For relative paths: resolves relative to the current file's directory
///
/// # Arguments
/// * `image_path` - The image path from markdown (e.g., "/src/assets/image.png" or "./image.png")
/// * `project_root` - The absolute path to the project root directory
/// * `current_file_path` - Optional absolute path to the current file being edited
///
/// # Returns
/// The validated absolute filesystem path that can be used with convertFileSrc
#[tauri::command]
#[specta::specta]
pub async fn resolve_image_path(
    image_path: String,
    project_root: String,
    current_file_path: Option<String>,
) -> Result<String, String> {
    let project_root_path = Path::new(&project_root);

    // Determine the absolute path based on the image path format
    let absolute_path = if image_path.starts_with('/') {
        // Absolute path from project root - strip leading slash and join with project root
        let relative_path = image_path.trim_start_matches('/');
        project_root_path.join(relative_path)
    } else if image_path.starts_with("./") || image_path.starts_with("../") {
        // Relative path - need current file path to resolve
        let current_file = current_file_path
            .ok_or_else(|| "Cannot resolve relative path without current file path".to_string())?;
        let current_file_path = Path::new(&current_file);
        let current_dir = current_file_path
            .parent()
            .ok_or_else(|| "Invalid current file path".to_string())?;
        current_dir.join(&image_path)
    } else {
        // Ambiguous path (no leading / or ./) - try as absolute from project root first
        project_root_path.join(&image_path)
    };

    // Validate the path is within project bounds and exists
    let validated_path =
        validate_project_path(absolute_path.to_string_lossy().as_ref(), &project_root)?;

    // Check if file exists
    if !validated_path.exists() {
        return Err(format!(
            "Image file not found: {}",
            validated_path.display()
        ));
    }

    // Return the absolute path as a string
    Ok(validated_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn test_validate_project_path_valid() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let test_file = project_root.join("content").join("test.md");

        // Create test structure
        fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        fs::write(&test_file, "test content").unwrap();

        let result = validate_project_path(
            &test_file.to_string_lossy(),
            &project_root.to_string_lossy(),
        );

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn test_validate_project_path_traversal_attack() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let malicious_path = project_root.join("../../../etc/passwd");

        // Create project directory
        fs::create_dir_all(&project_root).unwrap();

        let result = validate_project_path(
            &malicious_path.to_string_lossy(),
            &project_root.to_string_lossy(),
        );

        // Should fail due to path traversal
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("File outside project directory") || error.contains("Invalid file path")
        );

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn test_validate_project_path_nonexistent_file() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let nonexistent_file = project_root.join("nonexistent.md");

        // Create project directory
        fs::create_dir_all(&project_root).unwrap();

        let result = validate_project_path(
            &nonexistent_file.to_string_lossy(),
            &project_root.to_string_lossy(),
        );

        // Should succeed now that we allow non-existent files
        assert!(result.is_ok(), "Failed with error: {:?}", result.err());

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[tokio::test]
    async fn test_read_file_success() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let test_file = project_root.join("test_read.md");
        let test_content = "# Test Content\n\nThis is a test file.";

        // Create test file
        fs::create_dir_all(&project_root).unwrap();
        fs::write(&test_file, test_content).unwrap();

        let result = read_file(
            test_file.to_string_lossy().to_string(),
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());
        assert_eq!(result.unwrap(), test_content);

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[tokio::test]
    async fn test_read_file_path_traversal() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let malicious_file = project_root.join("../../../etc/passwd");

        // Create project directory
        fs::create_dir_all(&project_root).unwrap();

        let result = read_file(
            malicious_file.to_string_lossy().to_string(),
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("File outside project directory") || error.contains("Invalid file path")
        );

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[tokio::test]
    async fn test_write_file_success() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let test_file = project_root.join("test_write.md");
        let test_content = "# Written Content\n\nThis was written by the test.";

        // Create test structure
        fs::create_dir_all(&project_root).unwrap();
        fs::write(&test_file, "initial").unwrap(); // Create file first

        let result = write_file(
            test_file.to_string_lossy().to_string(),
            test_content.to_string(),
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());

        // Verify content was written
        let written_content = fs::read_to_string(&test_file).unwrap();
        assert_eq!(written_content, test_content);

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[tokio::test]
    async fn test_create_file_success() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let content_dir = project_root.join("content");
        let test_content = "# New File\n\nThis is a newly created file.";

        // Create project structure - ensure project_root exists first
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(&content_dir).unwrap();

        let result = create_file(
            content_dir.to_string_lossy().to_string(),
            "test_create.md".to_string(),
            test_content.to_string(),
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());

        let created_path = result.unwrap();
        assert!(Path::new(&created_path).exists());

        // Verify content
        let written_content = fs::read_to_string(&created_path).unwrap();
        assert_eq!(written_content, test_content);

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[tokio::test]
    async fn test_create_file_path_traversal() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let malicious_dir = project_root.join("../../../tmp");

        // Create project directory
        fs::create_dir_all(&project_root).unwrap();

        let result = create_file(
            malicious_dir.to_string_lossy().to_string(),
            "malicious.md".to_string(),
            "malicious content".to_string(),
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("File outside project directory") || error.contains("Invalid file path")
        );

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[tokio::test]
    async fn test_delete_file_success() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let test_file = project_root.join("test_delete.md");

        // Create file to delete
        fs::create_dir_all(&project_root).unwrap();
        fs::write(&test_file, "content to delete").unwrap();
        assert!(test_file.exists());

        let result = delete_file(
            test_file.to_string_lossy().to_string(),
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());
        assert!(!test_file.exists());

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    #[tokio::test]
    async fn test_delete_file_path_traversal() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let malicious_file = project_root.join("../../../tmp/should_not_delete.txt");

        // Create project directory
        fs::create_dir_all(&project_root).unwrap();

        let result = delete_file(
            malicious_file.to_string_lossy().to_string(),
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("File outside project directory") || error.contains("Invalid file path")
        );

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    fn deletion_target(path: &Path, expected_content: &str) -> DeleteFileTarget {
        DeleteFileTarget {
            file_path: path.to_string_lossy().to_string(),
            expected_content: expected_content.to_string(),
        }
    }

    #[test]
    fn delete_files_transaction_quarantines_and_recovers_a_verified_pair() {
        let project = tempfile::tempdir().unwrap();
        let recovery = tempfile::tempdir().unwrap();
        let primary = project.path().join("post.md");
        let sibling = project.path().join("post.zh.md");
        fs::write(&primary, "primary").unwrap();
        fs::write(&sibling, "translation").unwrap();

        let validated = validate_delete_file_targets(
            vec![
                deletion_target(&primary, "primary"),
                deletion_target(&sibling, "translation"),
            ],
            project.path().to_str().unwrap(),
        )
        .unwrap();
        let result = delete_files_transaction_with(
            &validated,
            recovery.path(),
            |source, quarantine| fs::rename(source, quarantine),
            |_, _, _| Ok(()),
        )
        .unwrap();

        assert!(!primary.exists());
        assert!(!sibling.exists());
        assert_eq!(
            fs::read_to_string(&result.files[0].recovery_path).unwrap(),
            "primary"
        );
        assert_eq!(
            fs::read_to_string(&result.files[1].recovery_path).unwrap(),
            "translation"
        );
        assert!(recovery.path().join("manifest.json").is_file());
    }

    #[test]
    fn delete_files_transaction_restores_the_first_file_on_partial_failure() {
        let project = tempfile::tempdir().unwrap();
        let recovery = tempfile::tempdir().unwrap();
        let primary = project.path().join("post.md");
        let sibling = project.path().join("post.zh.md");
        fs::write(&primary, "primary").unwrap();
        fs::write(&sibling, "translation").unwrap();
        let validated = validate_delete_file_targets(
            vec![
                deletion_target(&primary, "primary"),
                deletion_target(&sibling, "translation"),
            ],
            project.path().to_str().unwrap(),
        )
        .unwrap();
        let mut moves = 0;

        let error = delete_files_transaction_with(
            &validated,
            recovery.path(),
            |source, quarantine| {
                moves += 1;
                if moves == 2 {
                    return Err(std::io::Error::other("injected failure"));
                }
                fs::rename(source, quarantine)
            },
            |_, _, _| Ok(()),
        )
        .unwrap_err();

        assert!(error.contains("Quarantined files were restored"), "{error}");
        assert_eq!(fs::read_to_string(primary).unwrap(), "primary");
        assert_eq!(fs::read_to_string(sibling).unwrap(), "translation");
    }

    #[test]
    fn delete_files_transaction_refuses_stale_recovery_bytes_and_rolls_back() {
        let project = tempfile::tempdir().unwrap();
        let recovery = tempfile::tempdir().unwrap();
        let primary = project.path().join("post.md");
        let sibling = project.path().join("post.zh.md");
        fs::write(&primary, "primary").unwrap();
        fs::write(&sibling, "translation changed after backup").unwrap();
        let validated = validate_delete_file_targets(
            vec![
                deletion_target(&primary, "primary"),
                deletion_target(&sibling, "stale translation"),
            ],
            project.path().to_str().unwrap(),
        )
        .unwrap();

        let error = delete_files_transaction_with(
            &validated,
            recovery.path(),
            |source, quarantine| fs::rename(source, quarantine),
            |_, _, _| Ok(()),
        )
        .unwrap_err();

        assert!(
            error.contains("changed after the deletion recovery snapshot"),
            "{error}"
        );
        assert!(error.contains("Quarantined files were restored"), "{error}");
        assert_eq!(fs::read_to_string(primary).unwrap(), "primary");
        assert_eq!(
            fs::read_to_string(sibling).unwrap(),
            "translation changed after backup"
        );
    }

    #[test]
    fn delete_files_transaction_preserves_an_atomic_save_after_quarantine() {
        let project = tempfile::tempdir().unwrap();
        let recovery = tempfile::tempdir().unwrap();
        let primary = project.path().join("post.md");
        let replacement = project.path().join("replacement.tmp");
        fs::write(&primary, "approved bytes").unwrap();
        fs::write(&replacement, "new external save").unwrap();
        let validated = validate_delete_file_targets(
            vec![deletion_target(&primary, "approved bytes")],
            project.path().to_str().unwrap(),
        )
        .unwrap();

        let error = delete_files_transaction_with(
            &validated,
            recovery.path(),
            |source, quarantine| fs::rename(source, quarantine),
            |index, target, _| {
                if index == 0 {
                    fs::rename(&replacement, &target.path).map_err(|error| error.to_string())?;
                }
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.contains("Newer file content appeared"), "{error}");
        assert_eq!(fs::read_to_string(&primary).unwrap(), "new external save");
        assert_eq!(
            fs::read_to_string(recovery.path().join("1-post.md")).unwrap(),
            "approved bytes"
        );
    }

    #[test]
    fn restore_files_transaction_never_clobbers_a_newer_file() {
        let project = tempfile::tempdir().unwrap();
        let recovery = tempfile::tempdir().unwrap();
        let original = project.path().join("post.md");
        let backup = recovery.path().join("post.md");
        fs::write(&original, "newer content").unwrap();
        fs::write(&backup, "deleted content").unwrap();
        let targets = vec![ValidatedRestoreFileTarget {
            original_path: original.clone(),
            recovery_path: backup,
        }];

        let error = restore_files_transaction_inner(&targets).unwrap_err();

        assert!(error.contains("was not overwritten"), "{error}");
        assert_eq!(fs::read_to_string(original).unwrap(), "newer content");
    }

    #[test]
    fn restore_files_transaction_publishes_complete_bytes_atomically() {
        let project = tempfile::tempdir().unwrap();
        let recovery = tempfile::tempdir().unwrap();
        let original = project.path().join("post.md");
        let backup = recovery.path().join("post.md");
        fs::write(&backup, "deleted content").unwrap();
        let targets = vec![ValidatedRestoreFileTarget {
            original_path: original.clone(),
            recovery_path: backup,
        }];

        restore_files_transaction_inner(&targets).unwrap();

        assert_eq!(fs::read_to_string(original).unwrap(), "deleted content");
    }

    #[test]
    fn test_parse_frontmatter_with_yaml() {
        let content = r#"---
title: Test Post
description: A test post for parsing
draft: false
date: 2023-12-01
---

# Content

This is the main content of the post."#;

        let result = parse_frontmatter(content).unwrap();

        assert_eq!(result.frontmatter.len(), 4);
        assert_eq!(result.frontmatter.get("title").unwrap(), "Test Post");
        assert_eq!(
            result.frontmatter.get("draft").unwrap(),
            &Value::Bool(false)
        );
        assert!(result.content.contains("# Content"));
    }

    #[test]
    fn test_parse_frontmatter_no_yaml() {
        let content = r#"# Regular Markdown

This is just regular markdown content without frontmatter."#;

        let result = parse_frontmatter(content).unwrap();

        assert!(result.frontmatter.is_empty());
        assert_eq!(result.content, content);
        assert!(result.raw_frontmatter.is_empty());
    }

    #[test]
    fn test_parse_frontmatter_with_arrays() {
        let content = r#"---
title: Test Post
tags:
  - javascript
  - typescript
  - react
categories: [tech, programming]
---

# Content

This is a test post with arrays."#;

        let result = parse_frontmatter(content).unwrap();

        assert_eq!(result.frontmatter.len(), 3);
        assert_eq!(result.frontmatter.get("title").unwrap(), "Test Post");

        // Check multi-line array
        let tags = result.frontmatter.get("tags").unwrap();
        if let Value::Array(tags_array) = tags {
            assert_eq!(tags_array.len(), 3);
            assert_eq!(tags_array[0], Value::String("javascript".to_string()));
            assert_eq!(tags_array[1], Value::String("typescript".to_string()));
            assert_eq!(tags_array[2], Value::String("react".to_string()));
        } else {
            panic!("Expected tags to be an array");
        }

        // Check inline array
        let categories = result.frontmatter.get("categories").unwrap();
        if let Value::Array(categories_array) = categories {
            assert_eq!(categories_array.len(), 2);
            assert_eq!(categories_array[0], Value::String("tech".to_string()));
            assert_eq!(
                categories_array[1],
                Value::String("programming".to_string())
            );
        } else {
            panic!("Expected categories to be an array");
        }
    }

    #[test]
    fn test_rebuild_markdown_with_frontmatter() {
        let mut frontmatter = IndexMap::new();
        frontmatter.insert("title".to_string(), Value::String("New Title".to_string()));
        frontmatter.insert("draft".to_string(), Value::Bool(true));

        let content = "# Content\n\nThis is the content.";

        let result =
            rebuild_markdown_with_frontmatter_and_imports(&frontmatter, "", content).unwrap();

        assert!(result.starts_with("---\n"));
        // serde_norway doesn't quote simple strings without special characters
        assert!(result.contains("title: New Title") || result.contains("title: \"New Title\""));
        assert!(result.contains("draft: true"));
        assert!(result.contains("# Content"));
    }

    #[tokio::test]
    async fn test_save_markdown_content() {
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let test_file = project_root.join("test_save_markdown.md");

        // Create project structure
        fs::create_dir_all(&project_root).unwrap();
        fs::write(&test_file, "initial").unwrap(); // Create file first

        let mut frontmatter = IndexMap::new();
        frontmatter.insert(
            "title".to_string(),
            Value::String("Test Article".to_string()),
        );
        frontmatter.insert("draft".to_string(), Value::Bool(false));

        let content = "# Test Article\n\nThis is the article content.";

        let result = save_markdown_content(
            test_file.to_string_lossy().to_string(),
            Some(frontmatter), // Frontmatter was edited
            None,              // No raw frontmatter (frontmatter was edited)
            content.to_string(),
            String::new(), // No imports for this test
            None,          // No schema field order for this test
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());

        // Verify the saved file
        let saved_content = fs::read_to_string(&test_file).unwrap();
        assert!(saved_content.starts_with("---\n"));
        // serde_norway doesn't quote simple strings
        assert!(
            saved_content.contains("title: Test Article")
                || saved_content.contains("title: \"Test Article\"")
        );
        assert!(saved_content.contains("draft: false"));
        assert!(saved_content.contains("# Test Article"));
        assert!(saved_content.contains("This is the article content."));

        // Clean up
        let _ = fs::remove_dir_all(&project_root);
    }

    #[test]
    fn test_rebuild_with_raw_frontmatter() {
        // Test that raw frontmatter is preserved exactly as-is
        let raw_frontmatter = "title: My Title\ndate: 2024-01-15T12:30:00Z\ncustom_field: value";
        let content = "# Heading\n\nSome content.";
        let imports = "";

        let result = rebuild_markdown_with_raw_frontmatter(raw_frontmatter, imports, content);
        assert!(result.is_ok());

        let output = result.unwrap();
        // Should preserve the exact raw frontmatter including the datetime format
        assert!(output.contains("date: 2024-01-15T12:30:00Z"));
        assert!(output.starts_with("---\n"));
        assert!(output.contains("---\n\n# Heading"));
        assert!(output.contains("Some content."));
    }

    #[test]
    fn test_rebuild_with_raw_frontmatter_and_imports() {
        let raw_frontmatter = "title: Test";
        let content = "# Heading";
        let imports = "import Component from './Component';";

        let result = rebuild_markdown_with_raw_frontmatter(raw_frontmatter, imports, content);
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.contains("---\ntitle: Test\n---"));
        assert!(output.contains("import Component from './Component';"));
        assert!(output.contains("# Heading"));
    }

    #[test]
    fn test_rebuild_content_only() {
        // Test when there's no frontmatter at all
        let content = "# Just Content\n\nNo frontmatter here.";
        let imports = "";

        let result = rebuild_markdown_content_only(imports, content);
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.contains("---"));
        assert!(output.starts_with("# Just Content"));
        assert!(output.contains("No frontmatter here."));
    }

    #[test]
    fn test_rebuild_content_only_with_imports() {
        let content = "# Content";
        let imports = "import React from 'react';";

        let result = rebuild_markdown_content_only(imports, content);
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.contains("---"));
        assert!(output.starts_with("import React from 'react';"));
        assert!(output.contains("# Content"));
    }

    #[test]
    fn test_extract_imports_from_content() {
        let lines = vec![
            "import React from 'react';",
            "import { Component } from './Component';",
            "",
            "# Heading",
            "",
            "Some content here.",
        ];

        let (imports, content) = extract_imports_from_content(&lines);

        assert_eq!(
            imports,
            "import React from 'react';\nimport { Component } from './Component';"
        );
        assert_eq!(content, "# Heading\n\nSome content here.");
    }

    #[test]
    fn test_extract_multiline_imports() {
        let lines = vec![
            "import {",
            "  Component1,",
            "  Component2",
            "} from './components';",
            "",
            "# Content starts here",
        ];

        let (imports, content) = extract_imports_from_content(&lines);

        assert!(imports.contains("import {"));
        assert!(imports.contains("} from './components';"));
        assert_eq!(content, "# Content starts here");
    }

    #[test]
    fn test_extract_imports_without_semicolon_followed_by_markdown() {
        // Regression test: import without semicolon should NOT absorb markdown content
        let lines = vec![
            "import Foo from './foo'", // No semicolon!
            "# Heading starts here",   // This should be content, not import
            "",
            "Paragraph content.",
        ];

        let (imports, content) = extract_imports_from_content(&lines);

        // The import should be captured (even without semicolon)
        assert!(
            imports.contains("import Foo from './foo'"),
            "Import should be captured"
        );

        // The heading should NOT be in imports
        assert!(
            !imports.contains("# Heading"),
            "Markdown heading should not be absorbed into imports"
        );

        // The heading should be in content
        assert!(
            content.contains("# Heading starts here"),
            "Heading should be in content"
        );
        assert!(
            content.contains("Paragraph content."),
            "Paragraph should be in content"
        );
    }

    #[test]
    fn test_is_markdown_block_start() {
        // Test various Markdown block starts
        assert!(is_markdown_block_start("# Heading"));
        assert!(is_markdown_block_start("## Another heading"));
        assert!(is_markdown_block_start("> Blockquote"));
        assert!(is_markdown_block_start("- List item"));
        assert!(is_markdown_block_start("* Another list"));
        assert!(is_markdown_block_start("+ Plus list"));
        assert!(is_markdown_block_start("1. Numbered list"));
        assert!(is_markdown_block_start("42. Another number"));
        assert!(is_markdown_block_start("``` code fence"));
        assert!(is_markdown_block_start("<div>HTML tag</div>"));
        assert!(is_markdown_block_start("<Component />"));

        // Test things that are NOT markdown block starts
        assert!(!is_markdown_block_start("  Component1,"));
        assert!(!is_markdown_block_start("} from './foo'"));
        assert!(!is_markdown_block_start("Just regular text"));
        assert!(!is_markdown_block_start(""));
    }

    // --- UNIT TESTS FOR IMPORT PARSING HELPERS ---

    #[test]
    fn test_is_import_line() {
        // Should detect imports and exports
        assert!(is_import_line("import { foo } from 'bar'"));
        assert!(is_import_line("export const foo = 'bar'"));
        assert!(is_import_line("import foo from 'bar'"));
        assert!(is_import_line("export default Component"));

        // Should not match imports within strings
        assert!(!is_import_line("const foo = 'import'"));
        assert!(!is_import_line("// import comment"));
        assert!(!is_import_line("Regular text"));
    }

    #[test]
    fn test_has_import_terminator() {
        assert!(has_import_terminator("from 'foo';"));
        assert!(has_import_terminator("from 'bar';"));
        assert!(has_import_terminator("from \"baz\";"));
        assert!(!has_import_terminator("from 'foo'"));
        assert!(!has_import_terminator("from 'foo"));
        assert!(!has_import_terminator("import { Component,"));
    }

    #[test]
    fn test_is_import_continuation() {
        // Should continue on non-empty, non-terminated lines
        assert!(is_import_continuation("  from 'foo'"));
        assert!(is_import_continuation("  Component1,"));
        assert!(is_import_continuation("  Component2"));

        // Should NOT continue on empty lines
        assert!(!is_import_continuation(""));
        assert!(!is_import_continuation("   "));

        // Should NOT continue on terminated lines
        assert!(!is_import_continuation("from 'foo';"));

        // Should NOT continue on markdown blocks
        assert!(!is_import_continuation("# Heading"));
        assert!(!is_import_continuation("- List item"));
    }

    #[test]
    fn test_should_skip_empty_line() {
        let lines = vec!["import foo", "", "import bar"];
        assert!(should_skip_empty_line(&lines, 1)); // Should skip empty line at index 1

        let lines = vec!["import foo", "", "# Heading"];
        assert!(!should_skip_empty_line(&lines, 1)); // Should NOT skip, no more imports

        let lines = vec!["import foo", ""];
        assert!(!should_skip_empty_line(&lines, 1)); // Should NOT skip, EOF

        let lines = vec!["import foo", "", "", "import bar"];
        assert!(should_skip_empty_line(&lines, 1)); // Should skip multiple empty lines
    }

    // --- END IMPORT PARSING HELPER TESTS ---

    #[test]
    fn test_parse_mdx_with_imports() {
        let content = r#"---
title: Test Post
draft: false
---

import React from 'react';
import { Callout } from '../components/Callout';

# Test Post

<Callout type="info">
This is a callout component.
</Callout>

Regular markdown content here."#;

        let result = parse_frontmatter(content).unwrap();

        assert_eq!(result.frontmatter.len(), 2);
        assert_eq!(result.frontmatter.get("title").unwrap(), "Test Post");
        assert!(result.imports.contains("import React from 'react';"));
        assert!(result
            .imports
            .contains("import { Callout } from '../components/Callout';"));
        assert!(result.content.contains("# Test Post"));
        assert!(result.content.contains("<Callout"));
        assert!(!result.content.contains("import React"));
    }

    #[test]
    fn test_rebuild_with_imports() {
        let mut frontmatter = IndexMap::new();
        frontmatter.insert("title".to_string(), Value::String("Test".to_string()));

        let imports = "import React from 'react';\nimport { Component } from './Component';";
        let content = "# Test\n\n<Component />";

        let result =
            rebuild_markdown_with_frontmatter_and_imports(&frontmatter, imports, content).unwrap();

        assert!(result.starts_with("---\n"));
        assert!(result.contains("title: Test"));
        assert!(result.contains("import React from 'react';"));
        assert!(result.contains("import { Component } from './Component';"));
        assert!(result.contains("# Test"));
        assert!(result.contains("<Component />"));

        // Ensure proper spacing
        let lines: Vec<&str> = result.lines().collect();
        let frontmatter_end = lines.iter().position(|&line| line == "---").unwrap();
        let second_frontmatter_end = lines[frontmatter_end + 1..]
            .iter()
            .position(|&line| line == "---")
            .unwrap()
            + frontmatter_end
            + 1;

        // Should have a blank line after frontmatter before imports
        assert_eq!(lines[second_frontmatter_end + 1], "");
        // Should have imports next
        assert!(lines[second_frontmatter_end + 2].starts_with("import"));
    }

    #[test]
    fn test_validate_app_data_path_valid() {
        let app_data_dir = tempfile::TempDir::new().unwrap();
        let test_file = app_data_dir
            .path()
            .join("preferences")
            .join("settings.json");

        fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        fs::write(&test_file, "test content").unwrap();

        let result = validate_app_data_path(
            &test_file.to_string_lossy(),
            &app_data_dir.path().to_string_lossy(),
        );

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());
    }

    #[test]
    fn test_validate_app_data_path_traversal_attack() {
        let app_data_dir = tempfile::TempDir::new().unwrap();
        let malicious_path = app_data_dir.path().join("../../../etc/passwd");

        let result = validate_app_data_path(
            &malicious_path.to_string_lossy(),
            &app_data_dir.path().to_string_lossy(),
        );

        // Should fail due to path traversal
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("File outside app data directory")
                || error.contains("Invalid file path")
        );
    }

    #[test]
    fn test_to_kebab_case() {
        assert_eq!(to_kebab_case("My Image.png"), "my-image.png");
        assert_eq!(to_kebab_case("some_file_name.jpg"), "some-file-name.jpg");
        assert_eq!(to_kebab_case("UPPERCASE.PDF"), "uppercase.pdf");
        assert_eq!(
            to_kebab_case("Mixed Case File Name.txt"),
            "mixed-case-file-name.txt"
        );
        assert_eq!(
            to_kebab_case("already-kebab-case.md"),
            "already-kebab-case.md"
        );
        assert_eq!(
            to_kebab_case("file with   spaces.png"),
            "file-with-spaces.png"
        );
        assert_eq!(
            to_kebab_case("file___with___underscores.js"),
            "file-with-underscores.js"
        );
    }

    #[tokio::test]
    async fn test_copy_file_to_assets() {
        use std::fs;
        use tempfile::TempDir;

        // Create temporary directories
        let source_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();

        // Create a test file
        let test_file_path = source_dir.path().join("Test Image.png");
        fs::write(&test_file_path, b"fake image data").unwrap();

        // Copy file to assets
        let result = copy_file_to_assets(
            test_file_path.to_str().unwrap().to_string(),
            project_dir.path().to_str().unwrap().to_string(),
            "blog".to_string(),
            project_dir
                .path()
                .join("src/content/blog/post.md")
                .to_str()
                .unwrap()
                .to_string(),
            false, // Use absolute paths for test assertions
        )
        .await;

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());
        let relative_path = result.unwrap();

        // Check the returned path format (absolute from project root with leading /)
        assert!(relative_path.starts_with("/src/assets/blog/"));
        assert!(relative_path.contains("-test-image.png"));

        // Check file was actually copied (strip leading / for path construction)
        let dest_path = project_dir
            .path()
            .join(relative_path.trim_start_matches('/'));
        assert!(dest_path.exists());

        let content = fs::read(&dest_path).unwrap();
        assert_eq!(content, b"fake image data");
    }

    #[tokio::test]
    async fn test_copy_file_to_assets_with_conflict() {
        use chrono::Local;
        use std::fs;
        use tempfile::TempDir;

        // Create temporary directories
        let source_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();

        // Create assets directory
        let assets_dir = project_dir.path().join("src/assets/posts");
        fs::create_dir_all(&assets_dir).unwrap();

        // Create an existing file with today's date
        let date_prefix = Local::now().format("%Y-%m-%d").to_string();
        let existing_file = assets_dir.join(format!("{date_prefix}-test-file.md"));
        fs::write(&existing_file, b"existing").unwrap();

        // Create source file
        let test_file_path = source_dir.path().join("Test File.md");
        fs::write(&test_file_path, b"new content").unwrap();

        // Copy file - should add -1 suffix
        let result = copy_file_to_assets(
            test_file_path.to_str().unwrap().to_string(),
            project_dir.path().to_str().unwrap().to_string(),
            "posts".to_string(),
            project_dir
                .path()
                .join("src/content/posts/post.md")
                .to_str()
                .unwrap()
                .to_string(),
            false, // Use absolute paths for test assertions
        )
        .await;

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());
        let relative_path = result.unwrap();

        // Should have -1 suffix
        assert!(relative_path.contains(&format!("{date_prefix}-test-file-1.md")));

        // Both files should exist
        assert!(existing_file.exists());
        let new_file = project_dir
            .path()
            .join(relative_path.trim_start_matches('/'));
        assert!(new_file.exists());
    }

    #[tokio::test]
    async fn test_copy_file_to_assets_creates_directory() {
        use std::fs;
        use tempfile::TempDir;

        // Create temporary directories
        let source_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();

        // Create a test file
        let test_file_path = source_dir.path().join("document.pdf");
        fs::write(&test_file_path, b"pdf content").unwrap();

        // Assets directory doesn't exist yet
        let assets_dir = project_dir.path().join("src/assets/newsletters");
        assert!(!assets_dir.exists());

        // Copy file - should create directory
        let result = copy_file_to_assets(
            test_file_path.to_str().unwrap().to_string(),
            project_dir.path().to_str().unwrap().to_string(),
            "newsletters".to_string(),
            project_dir
                .path()
                .join("src/content/newsletters/post.md")
                .to_str()
                .unwrap()
                .to_string(),
            false, // Use absolute paths for test assertions
        )
        .await;

        assert!(result.is_ok(), "Failed with error: {:?}", result.err());

        // Directory should now exist
        assert!(assets_dir.exists());

        // File should be copied (strip leading / for path construction)
        let relative_path = result.unwrap();
        let dest_path = project_dir
            .path()
            .join(relative_path.trim_start_matches('/'));
        assert!(dest_path.exists());
    }

    #[test]
    fn test_serialize_nested_object_to_yaml() {
        use serde_json::json;

        let mut frontmatter = IndexMap::new();
        frontmatter.insert("title".to_string(), json!("Test Post"));
        frontmatter.insert(
            "metadata".to_string(),
            json!({
                "category": "Blog",
                "priority": 2,
                "deadline": "2025-10-21"
            }),
        );
        frontmatter.insert("tags".to_string(), json!(["rust", "yaml", "testing"]));

        let content = "# Test Content\n\nThis is a test.";

        let result =
            rebuild_markdown_with_frontmatter_and_imports_ordered(&frontmatter, "", content, None)
                .unwrap();

        // Verify the result contains proper YAML nested object syntax
        assert!(result.contains("metadata:"));
        assert!(result.contains("  category: Blog"));
        assert!(result.contains("  priority: 2"));
        assert!(result.contains("  deadline: 2025-10-21"));

        // Verify tags array is formatted correctly
        // serde_norway uses "- item" without indent (valid YAML)
        assert!(result.contains("tags:"));
        assert!(result.contains("- rust"));
        assert!(result.contains("- yaml"));
        assert!(result.contains("- testing"));

        // Ensure metadata is NOT JSON-stringified
        assert!(!result.contains(r#"metadata: "{"#));
        assert!(!result.contains(r#"{"category":"Blog""#));

        // Verify proper frontmatter structure
        assert!(result.starts_with("---\n"));
        assert!(result.contains("---\n\n# Test Content"));
    }

    #[test]
    fn test_parse_nested_object_from_yaml() {
        let yaml_content = r#"title: Test Post
metadata:
  category: Blog
  priority: 2
  deadline: 2025-10-21
tags:
  - rust
  - yaml
  - testing"#;

        let result = parse_yaml_to_json(yaml_content).unwrap();

        // Verify title
        assert_eq!(
            result.get("title").unwrap(),
            &Value::String("Test Post".to_string())
        );

        // Verify nested metadata object
        let metadata = result.get("metadata").unwrap();
        assert!(metadata.is_object());
        let metadata_obj = metadata.as_object().unwrap();
        assert_eq!(
            metadata_obj.get("category").unwrap(),
            &Value::String("Blog".to_string())
        );
        assert_eq!(
            metadata_obj.get("priority").unwrap(),
            &Value::Number(serde_json::Number::from(2))
        );
        assert_eq!(
            metadata_obj.get("deadline").unwrap(),
            &Value::String("2025-10-21".to_string())
        );

        // Verify tags array
        let tags = result.get("tags").unwrap();
        assert!(tags.is_array());
        let tags_array = tags.as_array().unwrap();
        assert_eq!(tags_array.len(), 3);
        assert_eq!(tags_array[0], Value::String("rust".to_string()));
        assert_eq!(tags_array[1], Value::String("yaml".to_string()));
        assert_eq!(tags_array[2], Value::String("testing".to_string()));
    }

    #[test]
    fn test_parse_and_serialize_roundtrip() {
        let original_yaml = r#"title: Roundtrip Test
description: Testing parse and serialize roundtrip
metadata:
  category: Development
  priority: 5
  deadline: 2025-12-31
tags:
  - test
  - roundtrip"#;

        // Parse YAML to HashMap
        let parsed = parse_yaml_to_json(original_yaml).unwrap();

        // Serialize back to markdown with frontmatter (verifies serialization works)
        let content = "# Test Content";
        let _serialized =
            rebuild_markdown_with_frontmatter_and_imports_ordered(&parsed, "", content, None)
                .unwrap();

        // Parse again
        let reparsed_content = format!("---\n{original_yaml}\n---\n\n{content}");
        let reparsed = parse_frontmatter(&reparsed_content).unwrap();

        // Verify metadata object survived the roundtrip
        let metadata = reparsed.frontmatter.get("metadata").unwrap();
        assert!(metadata.is_object());
        let metadata_obj = metadata.as_object().unwrap();
        assert_eq!(
            metadata_obj.get("category").unwrap(),
            &Value::String("Development".to_string())
        );
        assert_eq!(
            metadata_obj.get("priority").unwrap(),
            &Value::Number(serde_json::Number::from(5))
        );
    }

    #[test]
    fn test_serde_norway_handles_anchors() {
        // Test that serde_norway parses YAML with anchors/aliases without errors
        // Note: When deserializing to serde_json::Value, YAML merge keys (<<) are
        // preserved as literal keys rather than being merged. This is expected behavior
        // and doesn't affect Astro frontmatter which rarely uses anchors.
        let yaml = r#"base: &base
  title: Base Title
  category: Blog
reference: *base"#;

        let result = parse_yaml_to_json(yaml);
        assert!(
            result.is_ok(),
            "serde_norway should parse YAML with anchors without error"
        );

        let parsed = result.unwrap();

        // Verify base object
        let base = parsed.get("base").unwrap();
        assert!(base.is_object());
        let base_obj = base.as_object().unwrap();
        assert_eq!(
            base_obj.get("title").unwrap(),
            &Value::String("Base Title".to_string())
        );
        assert_eq!(
            base_obj.get("category").unwrap(),
            &Value::String("Blog".to_string())
        );

        // Verify reference points to the same structure
        let reference = parsed.get("reference").unwrap();
        assert!(reference.is_object());
        let reference_obj = reference.as_object().unwrap();
        assert_eq!(
            reference_obj.get("title").unwrap(),
            &Value::String("Base Title".to_string())
        );
        assert_eq!(
            reference_obj.get("category").unwrap(),
            &Value::String("Blog".to_string())
        );
    }

    #[test]
    fn test_serde_norway_handles_block_scalars() {
        // Test multi-line strings with pipe (literal) and fold scalars
        let yaml = r#"literal: |
  First line
  Second line
  Third line
folded: >
  This is a
  long description
  that will be folded"#;

        let result = parse_yaml_to_json(yaml);
        assert!(
            result.is_ok(),
            "serde_norway should parse block scalars (| and >)"
        );

        let parsed = result.unwrap();

        // Verify literal scalar (preserves newlines)
        let literal = parsed.get("literal").unwrap();
        assert!(literal.is_string());
        let literal_str = literal.as_str().unwrap();
        assert!(literal_str.contains("First line"));
        assert!(literal_str.contains("Second line"));

        // Verify folded scalar exists
        let folded = parsed.get("folded").unwrap();
        assert!(folded.is_string());
        let folded_str = folded.as_str().unwrap();
        assert!(folded_str.contains("This is a"));
        assert!(folded_str.contains("long description"));
    }

    #[test]
    fn test_date_normalization_in_nested_objects() {
        // Test that date normalization works recursively in nested objects and arrays
        let mut frontmatter = IndexMap::new();

        // Add nested object with date
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "deadline".to_string(),
            Value::String("2024-01-15T00:00:00Z".to_string()),
        );
        metadata.insert(
            "created".to_string(),
            Value::String("2024-01-01T12:30:00+00:00".to_string()),
        );
        frontmatter.insert("metadata".to_string(), Value::Object(metadata));

        // Add array with dates
        let events = vec![
            Value::String("2024-02-14T00:00:00Z".to_string()),
            Value::String("2024-03-20T00:00:00Z".to_string()),
        ];
        frontmatter.insert("events".to_string(), Value::Array(events));

        // Add top-level date
        frontmatter.insert(
            "publishDate".to_string(),
            Value::String("2024-06-15T00:00:00Z".to_string()),
        );

        // Apply normalization
        normalize_dates(&mut frontmatter);

        // Verify nested object dates are normalized
        let metadata = frontmatter.get("metadata").unwrap().as_object().unwrap();
        assert_eq!(
            metadata.get("deadline").unwrap(),
            &Value::String("2024-01-15".to_string()),
            "Nested object date should be normalized to date-only"
        );
        assert_eq!(
            metadata.get("created").unwrap(),
            &Value::String("2024-01-01".to_string()),
            "Nested object date with timezone should be normalized"
        );

        // Verify array dates are normalized
        let events = frontmatter.get("events").unwrap().as_array().unwrap();
        assert_eq!(
            events[0],
            Value::String("2024-02-14".to_string()),
            "Array date should be normalized"
        );
        assert_eq!(
            events[1],
            Value::String("2024-03-20".to_string()),
            "Array date should be normalized"
        );

        // Verify top-level date is normalized
        assert_eq!(
            frontmatter.get("publishDate").unwrap(),
            &Value::String("2024-06-15".to_string()),
            "Top-level date should be normalized"
        );
    }

    #[test]
    fn test_date_normalization_preserves_non_dates() {
        // Ensure date normalization doesn't affect strings that aren't ISO datetimes
        let mut frontmatter = IndexMap::new();

        frontmatter.insert(
            "title".to_string(),
            Value::String("My Post Title".to_string()),
        );
        frontmatter.insert("slug".to_string(), Value::String("my-post".to_string()));
        frontmatter.insert(
            "url".to_string(),
            Value::String("https://example.com/path".to_string()),
        );
        frontmatter.insert(
            "short_date".to_string(),
            Value::String("2024-01-15".to_string()),
        ); // Already date-only

        let original = frontmatter.clone();
        normalize_dates(&mut frontmatter);

        // All non-datetime strings should be unchanged
        assert_eq!(frontmatter, original);
    }

    #[test]
    fn test_field_ordering_preserved() {
        // Test that build_ordered_frontmatter respects schema order then alphabetical
        let mut frontmatter = IndexMap::new();

        // Add fields in random order
        frontmatter.insert("zebra".to_string(), Value::String("z".to_string()));
        frontmatter.insert("title".to_string(), Value::String("Test".to_string()));
        frontmatter.insert(
            "publishDate".to_string(),
            Value::String("2024-01-15".to_string()),
        );
        frontmatter.insert("apple".to_string(), Value::String("a".to_string()));
        frontmatter.insert("draft".to_string(), Value::Bool(false));

        // Define schema order (common Astro frontmatter fields)
        let schema_order = vec![
            "title".to_string(),
            "publishDate".to_string(),
            "draft".to_string(),
        ];

        let ordered = build_ordered_frontmatter(frontmatter, Some(schema_order));

        // Verify order: schema fields first (title, publishDate, draft),
        // then non-schema fields alphabetically (apple, zebra)
        let keys: Vec<&String> = ordered.keys().collect();
        assert_eq!(keys.len(), 5);
        assert_eq!(keys[0], "title");
        assert_eq!(keys[1], "publishDate");
        assert_eq!(keys[2], "draft");
        assert_eq!(keys[3], "apple"); // Alphabetical
        assert_eq!(keys[4], "zebra"); // Alphabetical
    }

    #[test]
    fn test_field_ordering_no_schema() {
        // Test that without schema, fields are purely alphabetical
        let mut frontmatter = IndexMap::new();

        frontmatter.insert("zebra".to_string(), Value::String("z".to_string()));
        frontmatter.insert("apple".to_string(), Value::String("a".to_string()));
        frontmatter.insert("middle".to_string(), Value::String("m".to_string()));

        let ordered = build_ordered_frontmatter(frontmatter, None);

        let keys: Vec<&String> = ordered.keys().collect();
        assert_eq!(keys, vec!["apple", "middle", "zebra"]);
    }

    #[test]
    fn test_field_ordering_partial_schema_match() {
        // Test when schema specifies fields that don't exist in frontmatter
        let mut frontmatter = IndexMap::new();

        frontmatter.insert("title".to_string(), Value::String("Test".to_string()));
        frontmatter.insert("extra".to_string(), Value::String("e".to_string()));

        // Schema includes fields that don't exist
        let schema_order = vec![
            "title".to_string(),
            "publishDate".to_string(), // Doesn't exist
            "draft".to_string(),       // Doesn't exist
        ];

        let ordered = build_ordered_frontmatter(frontmatter, Some(schema_order));

        let keys: Vec<&String> = ordered.keys().collect();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], "title"); // Schema field that exists
        assert_eq!(keys[1], "extra"); // Non-schema field, alphabetically
    }

    #[tokio::test]
    async fn test_update_frontmatter_preserves_mdx_imports() {
        // Regression test for: update_frontmatter should not delete MDX imports
        let temp_dir = std::env::temp_dir();
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_id = std::thread::current().id();
        let project_root = temp_dir.join(format!("test_project_{timestamp}_{thread_id:?}"));
        let test_file = project_root.join("test.mdx");

        // Create test file with MDX imports and frontmatter
        let original_content = r#"---
title: Original Title
draft: false
---

import { Component } from './Component'
import { AnotherComponent } from './AnotherComponent'

# Content

This is the main content."#;

        fs::create_dir_all(&project_root).unwrap();
        fs::write(&test_file, original_content).unwrap();

        // Update frontmatter
        let mut new_frontmatter = IndexMap::new();
        new_frontmatter.insert(
            "title".to_string(),
            Value::String("Updated Title".to_string()),
        );
        new_frontmatter.insert("draft".to_string(), Value::Bool(true));

        let result = update_frontmatter(
            test_file.to_string_lossy().to_string(),
            new_frontmatter,
            project_root.to_string_lossy().to_string(),
        )
        .await;

        assert!(
            result.is_ok(),
            "Failed to update frontmatter: {:?}",
            result.err()
        );

        // Read the file back and verify imports are preserved
        let updated_content = fs::read_to_string(&test_file).unwrap();

        // Check that imports are still present
        assert!(
            updated_content.contains("import { Component } from './Component'"),
            "First import was lost! Content:\n{updated_content}"
        );
        assert!(
            updated_content.contains("import { AnotherComponent } from './AnotherComponent'"),
            "Second import was lost! Content:\n{updated_content}"
        );

        // Check that frontmatter was updated
        assert!(
            updated_content.contains("title: Updated Title"),
            "Frontmatter title was not updated! Content:\n{updated_content}"
        );
        assert!(
            updated_content.contains("draft: true"),
            "Frontmatter draft was not updated! Content:\n{updated_content}"
        );

        // Check that content is still present
        assert!(
            updated_content.contains("# Content"),
            "Main content was lost! Content:\n{updated_content}"
        );

        // Cleanup
        let _ = fs::remove_dir_all(&project_root);
    }

    // ============================================================================
    // Unicode Edge Case Tests
    // ============================================================================

    #[test]
    fn test_frontmatter_with_emoji_in_values() {
        // Real-world: users type emoji in titles and descriptions
        let content = r#"---
title: "New Feature 🚀 Released!"
description: "Super cool ✨ stuff"
tags: ["🎉", "announcement"]
---

Content here"#;

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(
            parsed.frontmatter.get("title").unwrap(),
            "New Feature 🚀 Released!"
        );
        assert_eq!(
            parsed.frontmatter.get("description").unwrap(),
            "Super cool ✨ stuff"
        );
    }

    #[test]
    fn test_frontmatter_with_rtl_text() {
        // Right-to-left languages (Arabic, Hebrew)
        let content = r#"---
title: "مرحبا بك في العالم"
author: "محمد"
titleHebrew: "שלום עולם"
---

Content"#;

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(
            parsed.frontmatter.get("title").unwrap(),
            "مرحبا بك في العالم"
        );
        assert_eq!(parsed.frontmatter.get("author").unwrap(), "محمد");
        assert_eq!(parsed.frontmatter.get("titleHebrew").unwrap(), "שלום עולם");
    }

    #[test]
    fn test_frontmatter_with_mixed_scripts() {
        // CJK + Latin + Cyrillic
        let content = r#"---
title: "Hello 世界 Мир"
author: "名前-Name-Имя"
---

Content"#;

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.frontmatter.get("title").unwrap(), "Hello 世界 Мир");
        assert_eq!(parsed.frontmatter.get("author").unwrap(), "名前-Name-Имя");
    }

    #[test]
    fn test_frontmatter_with_combining_characters() {
        // Combining diacritics (common in some languages)
        let content = r#"---
title: "café"
author: "José"
---

Content"#;

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.frontmatter.get("title").unwrap(), "café");
        assert_eq!(parsed.frontmatter.get("author").unwrap(), "José");
    }

    #[test]
    fn test_frontmatter_with_zero_width_characters() {
        // Zero-width characters (can break parsing if not handled)
        let content = "---\ntitle: \"test\u{200B}word\"\n---\n\nContent"; // zero-width space

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.frontmatter.get("title").unwrap(), "test\u{200B}word");
    }

    #[test]
    fn test_serialize_unicode_roundtrip() {
        // CRITICAL: Ensure unicode survives parse → serialize → parse
        let mut frontmatter = IndexMap::new();
        frontmatter.insert(
            "title".to_string(),
            Value::String("🚀 Test 世界".to_string()),
        );
        frontmatter.insert("emoji".to_string(), Value::String("✨🎉🔥".to_string()));

        let serialized =
            rebuild_markdown_with_frontmatter_and_imports(&frontmatter, "", "Content").unwrap();
        let reparsed = parse_frontmatter(&serialized).unwrap();

        assert_eq!(reparsed.frontmatter.get("title").unwrap(), "🚀 Test 世界");
        assert_eq!(reparsed.frontmatter.get("emoji").unwrap(), "✨🎉🔥");
    }

    // ============================================================================
    // Malformed YAML Tests
    // ============================================================================

    #[test]
    fn test_frontmatter_unclosed_quote() {
        // Common typo: forget closing quote
        let content = r#"---
title: "Unclosed quote
description: "Valid"
---

Content"#;

        let result = parse_frontmatter(content);
        assert!(result.is_err(), "Should reject unclosed quote");
    }

    #[test]
    fn test_frontmatter_mixed_indentation() {
        // Common issue: mixing tabs and spaces
        let content = "---\ntitle: Test\n\tdescription: Mixed\n  author: Name\n---\n\nContent";

        let result = parse_frontmatter(content);
        // Should either parse correctly or fail gracefully (not corrupt)
        if let Ok(parsed) = result {
            assert!(parsed.frontmatter.contains_key("title"));
        }
    }

    #[test]
    fn test_frontmatter_missing_closing_delimiter() {
        // Missing closing ---
        let content = r#"---
title: Test
description: Missing closer

Content starts here"#;

        let result = parse_frontmatter(content);
        assert!(result.is_err(), "Should reject missing closing delimiter");
    }

    #[test]
    fn test_frontmatter_with_only_comments() {
        // Edge case: frontmatter block with only comments
        let content = r#"---
# This is just a comment
# Another comment
---

Content"#;

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        assert!(result.unwrap().frontmatter.is_empty());
    }

    // ============================================================================
    // Line Ending Edge Case Tests
    // ============================================================================

    #[test]
    fn test_frontmatter_with_crlf_line_endings() {
        // Windows line endings
        let content = "---\r\ntitle: Test\r\ndescription: Windows\r\n---\r\n\r\nContent";

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.frontmatter.get("title").unwrap(), "Test");
        assert_eq!(parsed.frontmatter.get("description").unwrap(), "Windows");
    }

    #[test]
    fn test_frontmatter_with_mixed_line_endings() {
        // Mixed CRLF and LF (can happen with git autocrlf)
        let content = "---\r\ntitle: Test\ndescription: Mixed\r\n---\n\nContent";

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.frontmatter.get("title").unwrap(), "Test");
        assert_eq!(parsed.frontmatter.get("description").unwrap(), "Mixed");
    }

    #[test]
    fn test_serialize_preserves_unix_line_endings() {
        // Our output should always be LF, not CRLF
        let mut frontmatter = IndexMap::new();
        frontmatter.insert("title".to_string(), Value::String("Test".to_string()));

        let result =
            rebuild_markdown_with_frontmatter_and_imports(&frontmatter, "", "Content").unwrap();

        assert!(!result.contains("\r\n"), "Should use LF, not CRLF");
        assert!(result.contains('\n'), "Should have LF line endings");
    }

    // ============================================================================
    // Empty/Minimal Input Tests
    // ============================================================================

    #[test]
    fn test_frontmatter_with_empty_string_values() {
        // Empty strings should be preserved, not treated as null
        let content = r#"---
title: ""
description: ""
author: "Actual Value"
---

Content"#;

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.frontmatter.get("title").unwrap(), "");
        assert_eq!(parsed.frontmatter.get("description").unwrap(), "");
        assert_eq!(parsed.frontmatter.get("author").unwrap(), "Actual Value");
    }

    #[test]
    fn test_single_character_content_after_frontmatter() {
        // Edge case: minimal content
        let content = r#"---
title: Test
---

X"#;

        let result = parse_frontmatter(content);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.content, "X");
        assert_eq!(parsed.frontmatter.get("title").unwrap(), "Test");
    }

    // --- Trailing newline round-trip tests ---
    // These verify that save -> reload preserves trailing whitespace exactly,
    // preventing the editor scroll jump bug (full doc replacement on content mismatch).

    #[test]
    fn test_roundtrip_content_no_trailing_newline() {
        let body = "Hello world.";
        let raw_fm = "title: Test";
        let saved = rebuild_markdown_with_raw_frontmatter(raw_fm, "", body).unwrap();
        let parsed = parse_frontmatter(&saved).unwrap();
        assert_eq!(parsed.content, body);
    }

    #[test]
    fn test_roundtrip_content_single_trailing_newline() {
        let body = "Hello world.\n";
        let raw_fm = "title: Test";
        let saved = rebuild_markdown_with_raw_frontmatter(raw_fm, "", body).unwrap();
        let parsed = parse_frontmatter(&saved).unwrap();
        assert_eq!(parsed.content, body);
    }

    #[test]
    fn test_roundtrip_content_double_trailing_newline() {
        let body = "Hello world.\n\n";
        let raw_fm = "title: Test";
        let saved = rebuild_markdown_with_raw_frontmatter(raw_fm, "", body).unwrap();
        let parsed = parse_frontmatter(&saved).unwrap();
        assert_eq!(parsed.content, body);
    }

    #[test]
    fn test_roundtrip_content_triple_trailing_newline() {
        let body = "Hello world.\n\n\n";
        let raw_fm = "title: Test";
        let saved = rebuild_markdown_with_raw_frontmatter(raw_fm, "", body).unwrap();
        let parsed = parse_frontmatter(&saved).unwrap();
        assert_eq!(parsed.content, body);
    }

    #[test]
    fn test_roundtrip_content_with_imports_no_trailing_newline() {
        let body = "Some content";
        let imports = "import Foo from './foo'";
        let raw_fm = "title: Test";
        let saved = rebuild_markdown_with_raw_frontmatter(raw_fm, imports, body).unwrap();
        let parsed = parse_frontmatter(&saved).unwrap();
        assert_eq!(parsed.content, body);
        assert_eq!(parsed.imports, imports);
    }

    #[test]
    fn test_roundtrip_content_with_imports_double_trailing_newline() {
        let body = "Some content\n\n";
        let imports = "import Foo from './foo'";
        let raw_fm = "title: Test";
        let saved = rebuild_markdown_with_raw_frontmatter(raw_fm, imports, body).unwrap();
        let parsed = parse_frontmatter(&saved).unwrap();
        assert_eq!(parsed.content, body);
        assert_eq!(parsed.imports, imports);
    }

    #[test]
    fn test_roundtrip_no_frontmatter_no_trailing_newline() {
        let saved = "Just some content.";
        let parsed = parse_frontmatter(saved).unwrap();
        assert_eq!(parsed.content, saved);
    }

    #[test]
    fn test_roundtrip_no_frontmatter_double_trailing_newline() {
        let saved = "Just some content.\n\n";
        let parsed = parse_frontmatter(saved).unwrap();
        assert_eq!(parsed.content, saved);
    }
}

#[cfg(test)]
mod frontmatter_merge_tests {
    use super::*;
    use serde_json::json;

    fn fm(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    const WLOG_RAW: &str = "title: 'A City Walk in San Francisco'\ndescription: 'A Saturday afternoon walk: two Transamericas.'\npubDate: '2024-07-06'\nheroImage: '../../assets/hero/2026/07/sf-city-walk-cover.webp'\ncategory: 'Journal'\nlang: 'en'\ntranslationKey: 'a-city-walk-in-san-francisco'";

    fn wlog_values() -> IndexMap<String, Value> {
        fm(&[
            ("title", json!("A City Walk in San Francisco")),
            (
                "description",
                json!("A Saturday afternoon walk: two Transamericas."),
            ),
            ("pubDate", json!("2024-07-06")),
            (
                "heroImage",
                json!("../../assets/hero/2026/07/sf-city-walk-cover.webp"),
            ),
            ("category", json!("Journal")),
            ("lang", json!("en")),
            ("translationKey", json!("a-city-walk-in-san-francisco")),
        ])
    }

    #[test]
    fn unchanged_fields_keep_original_bytes_and_order() {
        let mut new_fm = wlog_values();
        new_fm.insert("title".to_string(), json!("A Better Walk"));

        let merged = merge_frontmatter_preserving_format(WLOG_RAW, &new_fm).unwrap();
        let lines: Vec<&str> = merged.lines().collect();
        assert_eq!(lines[0], "title: 'A Better Walk'");
        assert_eq!(
            lines[1],
            "description: 'A Saturday afternoon walk: two Transamericas.'"
        );
        assert_eq!(lines[2], "pubDate: '2024-07-06'");
        assert_eq!(lines[6], "translationKey: 'a-city-walk-in-san-francisco'");
        assert_eq!(lines.len(), 7);
    }

    #[test]
    fn identical_values_reproduce_the_block_verbatim() {
        let merged = merge_frontmatter_preserving_format(WLOG_RAW, &wlog_values()).unwrap();
        assert_eq!(merged, format!("{WLOG_RAW}\n"));
    }

    #[test]
    fn single_quote_style_escapes_inner_quotes() {
        let mut new_fm = wlog_values();
        new_fm.insert("title".to_string(), json!("it's a walk"));
        let merged = merge_frontmatter_preserving_format(WLOG_RAW, &new_fm).unwrap();
        assert!(
            merged.starts_with("title: 'it''s a walk'\n"),
            "got: {merged}"
        );
    }

    #[test]
    fn removed_fields_drop_and_new_fields_append() {
        let mut new_fm = wlog_values();
        new_fm.shift_remove("category");
        new_fm.insert("draft".to_string(), json!(true));

        let merged = merge_frontmatter_preserving_format(WLOG_RAW, &new_fm).unwrap();
        assert!(!merged.contains("category"));
        assert!(merged.ends_with("draft: true\n"), "got: {merged}");
    }

    #[test]
    fn comments_and_blank_lines_are_preserved() {
        let raw = "# owner note\ntitle: 'X'\n\npubDate: '2024-07-06'";
        let new_fm = fm(&[("title", json!("Y")), ("pubDate", json!("2024-07-06"))]);
        let merged = merge_frontmatter_preserving_format(raw, &new_fm).unwrap();
        assert_eq!(
            merged,
            "# owner note\ntitle: 'Y'\n\npubDate: '2024-07-06'\n"
        );
    }

    #[test]
    fn multiline_values_are_kept_when_unchanged() {
        let raw = "title: 'X'\ntags:\n  - one\n  - two";
        let new_fm = fm(&[("title", json!("X")), ("tags", json!(["one", "two"]))]);
        let merged = merge_frontmatter_preserving_format(raw, &new_fm).unwrap();
        assert_eq!(merged, "title: 'X'\ntags:\n  - one\n  - two\n");
    }

    #[test]
    fn falls_back_on_flow_mapping() {
        let raw = "{title: X, lang: en}";
        let new_fm = fm(&[("title", json!("X")), ("lang", json!("en"))]);
        assert!(merge_frontmatter_preserving_format(raw, &new_fm).is_none());
    }

    #[test]
    fn unquoted_scalars_stay_unquoted_via_serde() {
        let raw = "title: plain words\ncount: 3";
        let new_fm = fm(&[("title", json!("plain words")), ("count", json!(5))]);
        let merged = merge_frontmatter_preserving_format(raw, &new_fm).unwrap();
        assert_eq!(merged, "title: plain words\ncount: 5\n");
    }

    #[test]
    fn save_markdown_uses_merge_when_raw_present() {
        let dir = std::env::temp_dir().join(format!(
            "ae-merge-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("post.md");
        std::fs::write(&file, "seed").unwrap();

        let mut new_fm = wlog_values();
        new_fm.insert("title".to_string(), json!("Edited Title"));

        tauri::async_runtime::block_on(save_markdown_content(
            file.to_string_lossy().to_string(),
            Some(new_fm),
            Some(WLOG_RAW.to_string()),
            "Body text.".to_string(),
            String::new(),
            None,
            dir.to_string_lossy().to_string(),
        ))
        .unwrap();

        let written = std::fs::read_to_string(&file).unwrap();
        assert!(written.starts_with("---\ntitle: 'Edited Title'\ndescription: 'A Saturday afternoon walk: two Transamericas.'\n"), "got: {written}");
        assert!(written.contains("translationKey: 'a-city-walk-in-san-francisco'\n---\n"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
