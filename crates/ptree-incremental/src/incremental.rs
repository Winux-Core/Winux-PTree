// Incremental cache updates via explicit changed-path plans.
// Journal integration still needs platform-specific path reconstruction,
// but traversal can already consume trustworthy changed paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ptree_cache::DiskCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncrementalChangeKind {
    Created,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalChange {
    pub path:         PathBuf,
    pub kind:         IncrementalChangeKind,
    pub is_directory: bool,
}

impl IncrementalChange {
    pub fn created(path: PathBuf, is_directory: bool) -> Self {
        Self {
            path,
            kind: IncrementalChangeKind::Created,
            is_directory,
        }
    }

    pub fn modified(path: PathBuf, is_directory: bool) -> Self {
        Self {
            path,
            kind: IncrementalChangeKind::Modified,
            is_directory,
        }
    }

    pub fn deleted(path: PathBuf, is_directory: bool) -> Self {
        Self {
            path,
            kind: IncrementalChangeKind::Deleted,
            is_directory,
        }
    }

    pub fn renamed(path: PathBuf, is_directory: bool) -> Self {
        Self {
            path,
            kind: IncrementalChangeKind::Renamed,
            is_directory,
        }
    }
}

/// Build the exact directory set traversal should revisit for a localized refresh.
///
/// The returned set always includes `scan_root`, the directly affected directory for each
/// change, and every ancestor from that directory back to `scan_root`.
pub fn build_changed_directory_set(scan_root: &Path, changes: &[IncrementalChange]) -> HashSet<PathBuf> {
    let mut changed_dirs = HashSet::new();
    changed_dirs.insert(scan_root.to_path_buf());

    for change in changes {
        if !change.path.starts_with(scan_root) {
            continue;
        }

        if let Some(dir_path) = affected_directory_path(change, scan_root) {
            insert_directory_and_ancestors(&mut changed_dirs, scan_root, &dir_path);
        }
    }

    changed_dirs
}

fn affected_directory_path(change: &IncrementalChange, scan_root: &Path) -> Option<PathBuf> {
    match (change.kind, change.is_directory) {
        (IncrementalChangeKind::Deleted, true) => change.path.parent().map(Path::to_path_buf),
        (_, true) if change.path.is_dir() => Some(change.path.clone()),
        _ => change.path.parent().map(Path::to_path_buf),
    }
    .filter(|path| path.starts_with(scan_root))
}

fn insert_directory_and_ancestors(changed_dirs: &mut HashSet<PathBuf>, scan_root: &Path, dir_path: &Path) {
    let mut current = Some(dir_path);
    while let Some(path) = current {
        if !path.starts_with(scan_root) {
            break;
        }

        changed_dirs.insert(path.to_path_buf());
        if path == scan_root {
            break;
        }

        current = path.parent();
    }
}

/// Attempt incremental cache update using USN Journal
///
/// Returns true if incremental update succeeded, false if should fall back to full scan
/// - If journal unavailable: Returns false and falls back to full scan
/// - If journal available: Applies changes and returns true
#[cfg(windows)]
pub fn try_incremental_update(_cache: &mut DiskCache, _drive_letter: char) -> Result<bool> {
    // USN Journal integration is not implemented on this build
    // Fall back to full scan
    Ok(false)
}

#[cfg(not(windows))]
pub fn try_incremental_update(_cache: &mut DiskCache, _drive_letter: char) -> Result<bool> {
    Ok(false) // Not available on non-Windows
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("ptree_incremental_{name}_{unique}"))
    }

    #[test]
    fn file_changes_refresh_parent_chain() {
        let root = test_root("file_chain");
        let nested = root.join("alpha").join("beta");
        fs::create_dir_all(&nested).unwrap();

        let changed =
            build_changed_directory_set(&root, &[IncrementalChange::modified(nested.join("file.txt"), false)]);

        assert!(changed.contains(&root));
        assert!(changed.contains(&root.join("alpha")));
        assert!(changed.contains(&nested));
        assert!(!changed.contains(&nested.join("file.txt")));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn deleted_directory_refreshes_existing_parent_chain() {
        let root = test_root("deleted_dir");
        let existing_parent = root.join("alpha").join("beta");
        fs::create_dir_all(&existing_parent).unwrap();
        let deleted_dir = existing_parent.join("gone");

        let changed = build_changed_directory_set(&root, &[IncrementalChange::deleted(deleted_dir.clone(), true)]);

        assert!(changed.contains(&root));
        assert!(changed.contains(&root.join("alpha")));
        assert!(changed.contains(&existing_parent));
        assert!(!changed.contains(&deleted_dir));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn created_directory_includes_new_directory_when_it_exists() {
        let root = test_root("created_dir");
        let created_dir = root.join("alpha").join("fresh");
        fs::create_dir_all(&created_dir).unwrap();

        let changed = build_changed_directory_set(&root, &[IncrementalChange::created(created_dir.clone(), true)]);

        assert!(changed.contains(&root));
        assert!(changed.contains(&root.join("alpha")));
        assert!(changed.contains(&created_dir));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ignores_changes_outside_scan_root() {
        let root = test_root("outside_root");
        fs::create_dir_all(&root).unwrap();
        let outside = test_root("outside");
        fs::create_dir_all(&outside).unwrap();

        let changed =
            build_changed_directory_set(&root, &[IncrementalChange::modified(outside.join("file.txt"), false)]);

        assert_eq!(changed.len(), 1);
        assert!(changed.contains(&root));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }
}
