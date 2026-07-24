use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as RegistryMutex, OnceLock, Weak};
use tokio::sync::{Mutex as ProjectMutex, OwnedMutexGuard};

type ProjectLock = ProjectMutex<()>;

static PROJECT_MUTATION_LOCKS: OnceLock<RegistryMutex<HashMap<PathBuf, Weak<ProjectLock>>>> =
    OnceLock::new();

fn project_mutation_locks() -> &'static RegistryMutex<HashMap<PathBuf, Weak<ProjectLock>>> {
    PROJECT_MUTATION_LOCKS.get_or_init(|| RegistryMutex::new(HashMap::new()))
}

fn project_lock(project_root: &Path) -> Result<Arc<ProjectLock>, String> {
    let canonical_root = project_root
        .canonicalize()
        .map_err(|error| format!("Could not coordinate project changes: {error}"))?;
    let mut locks = project_mutation_locks()
        .lock()
        .map_err(|_| "Project change coordination is unavailable".to_string())?;

    locks.retain(|_, project_lock| project_lock.strong_count() > 0);
    if let Some(project_lock) = locks.get(&canonical_root).and_then(Weak::upgrade) {
        return Ok(project_lock);
    }

    let project_lock = Arc::new(ProjectMutex::new(()));
    locks.insert(canonical_root, Arc::downgrade(&project_lock));
    Ok(project_lock)
}

/// Pull waits for an already-running write in the same project to finish
/// before it takes its snapshot. Once held, every cooperating backend mutator
/// for that project fails fast instead of queueing stale bytes that could land
/// after the pull.
pub fn lock_for_pull(project_root: &Path) -> Result<OwnedMutexGuard<()>, String> {
    Ok(project_lock(project_root)?.blocking_lock_owned())
}

/// Project writes never wait behind Pull in the same project. Waiting would
/// allow content captured before Pull to overwrite the newly pulled version
/// after Pull completes. Independent projects use independent locks.
pub fn try_lock_for_write(
    project_root: &Path,
    operation: &str,
) -> Result<OwnedMutexGuard<()>, String> {
    project_lock(project_root)?.try_lock_owned().map_err(|_| {
        format!("Could not {operation} because Pull or another project change is still running")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_project_writes_share_a_lock() {
        let project = tempfile::tempdir().unwrap();
        let _guard = try_lock_for_write(project.path(), "save").unwrap();

        let error = try_lock_for_write(project.path(), "rename").unwrap_err();

        assert!(error.contains("another project change is still running"));
    }

    #[test]
    fn independent_projects_do_not_block_each_other() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let _first_guard = try_lock_for_write(first.path(), "save").unwrap();

        let _second_guard = try_lock_for_write(second.path(), "save").unwrap();
    }
}
