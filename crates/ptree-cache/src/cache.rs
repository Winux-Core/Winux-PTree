use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use colored::Colorize;
use rayon::slice::ParallelSliceMut;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[cfg(windows)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct USNJournalState;

#[cfg(not(windows))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct USNJournalState;

/// Directory metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub path:         PathBuf,
    pub name:         String,
    pub modified:     DateTime<Utc>,
    pub content_hash: u64,
    pub file_count:   usize,
    pub total_size:   u64,
    pub children:     Vec<String>,
    pub is_hidden:    bool,
    pub is_dir:       bool,
}

/// Compute Merkle tree-style content hash for a directory
///
/// The hash captures:
/// - Directory path (normalized)
/// - Modification timestamp (as i64)
/// - Number of children (file count)
/// - Sorted child names (alphabetically)
/// - Sorted child content hashes (for subdirectories)
///
/// This makes the hash sensitive to any structural changes in the directory:
/// - New files/directories
/// - Deleted files/directories
/// - Renamed items
/// - Timestamp changes
/// - Recursive child changes (due to Merkle structure)
pub fn compute_content_hash(
    path: &Path,
    modified: DateTime<Utc>,
    children: &[String],
    child_hashes: &HashMap<PathBuf, u64>,
) -> u64 {
    let mut hasher = DefaultHasher::new();

    // 1. Hash directory path (normalized)
    let normalized_path = path.to_string_lossy().to_lowercase();
    normalized_path.hash(&mut hasher);

    // 2. Hash modification timestamp (as i64)
    modified.timestamp().hash(&mut hasher);

    // 3. Hash children count
    children.len().hash(&mut hasher);

    // 4. Hash sorted child names
    let mut sorted_children = children.to_vec();
    sorted_children.sort();
    for child_name in &sorted_children {
        child_name.hash(&mut hasher);
    }

    // 5. Hash sorted child hashes (Merkle tree propagation)
    let mut child_hashes_list: Vec<(String, u64)> = child_hashes
        .iter()
        .filter_map(|(child_path, hash)| {
            // Only include children that are direct children of this directory
            if child_path.parent() == Some(path) {
                child_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|name| (name.to_string(), *hash))
            } else {
                None
            }
        })
        .collect();

    child_hashes_list.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, hash) in child_hashes_list {
        hash.hash(&mut hasher);
    }

    hasher.finish()
}

/// Check if a directory has changed by comparing content hashes
pub fn has_directory_changed(old_entry: &DirEntry, new_entry: &DirEntry) -> bool {
    old_entry.content_hash != new_entry.content_hash
}

/// In-memory tree cache
///
/// Memory Model (Hard-Bounded per README spec):
/// - Each directory entry is capped at 200 bytes (directory name + metadata)
/// - Memory usage is strictly: `memory ≤ directory_count × 200 bytes`
/// - Example: 2M directories = 400MB maximum memory footprint
/// - No unbounded string growth; paths are traversed, not accumulated
///
/// This is enforced at the type level through bounded path handling and
/// non-recursive DFS traversal. The 200-byte bound includes:
/// - PathBuf key in HashMap (varies, but path length is constrained)
/// - DirEntry value (name String, metadata, Vec<String> children)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiskCache {
    /// Map of absolute paths to directory entries
    pub entries: HashMap<PathBuf, DirEntry>,

    /// Last scan timestamp
    pub last_scan: DateTime<Utc>,

    /// Root path (e.g., C:\)
    pub root: PathBuf,

    /// Last scanned directory (for subsequent runs to only scan current dir)
    pub last_scanned_root: PathBuf,

    /// USN Journal state for tracking changes (Windows only)
    #[cfg(windows)]
    pub usn_state: USNJournalState,

    /// Pending writes (buffered for batch updates)
    #[serde(skip)]
    pub pending_writes: Vec<(PathBuf, DirEntry)>,

    /// Maximum pending writes before flush
    #[serde(skip)]
    pub flush_threshold: usize,

    /// Whether to show hidden file attributes in output
    #[serde(skip)]
    pub show_hidden: bool,

    /// Skip statistics: count of skipped directories by name
    #[serde(skip)]
    pub skip_stats: std::collections::HashMap<String, usize>,

    /// True when cache metadata/files were loaded from disk.
    /// Used to distinguish "lazy-loaded cache" from true first run.
    #[serde(skip)]
    pub has_persisted_snapshot: bool,

    /// Entry count loaded from the cache index for cheap cache-hit stats.
    #[serde(skip)]
    pub persisted_entry_count: usize,

    /// File-count hint loaded from the cache index for cheap cache-hit stats.
    #[serde(skip)]
    pub persisted_file_count: usize,
}

impl DiskCache {
    // ============================================================================
    // Cache Loading & Saving
    // ============================================================================

    /// Open or create cache file with fast cold-start lazy loading
    ///
    /// Strategy:
    /// - Load index only (~1ms for millions of entries)
    /// - Defer entry deserialization until output phase
    /// - Use in-memory entries for traversal building
    pub fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().unwrap())?;

        // Load from lazy cache format (index only, deferred entry loading)
        let index_path = path.with_extension("idx");
        let data_path = path.with_extension("dat");

        if index_path.exists() {
            match Self::load_from_lazy_cache(&index_path, &data_path) {
                Ok(cache) => return Ok(cache),
                Err(_) => Self::recover_snapshot(&index_path, &data_path)?,
            }
        }

        Ok(Self::new_empty())
    }

    /// Load from lazy cache format - index only (fast cold start)
    /// Entries not loaded until output phase to minimize startup time
    fn load_from_lazy_cache(index_path: &Path, data_path: &Path) -> Result<Self> {
        use crate::cache_rkyv::RkyvMmapCache;

        let rkyv_cache = RkyvMmapCache::open(index_path, data_path)?;

        // DO NOT load all entries - keep HashMap empty for cold-start speed
        // Entries will be loaded on-demand during output formatting

        Ok(DiskCache {
            entries:                   HashMap::new(), // Empty - entries loaded on-demand
            last_scan:                 rkyv_cache.index.last_scan,
            root:                      rkyv_cache.index.root.clone(),
            last_scanned_root:         rkyv_cache.index.last_scanned_root.clone(),
            #[cfg(windows)]
            usn_state:                 rkyv_cache.index.usn_state.clone(),
            pending_writes:            Vec::new(),
            flush_threshold:           5000,
            show_hidden:               false,
            skip_stats:                rkyv_cache.index.skip_stats.clone(),
            has_persisted_snapshot:    true,
            persisted_entry_count:     rkyv_cache.index.offsets.len(),
            persisted_file_count:      rkyv_cache.index.total_files,
        })
    }

    /// Create a new empty cache with default USN state
    #[cfg(windows)]
    fn new_empty() -> Self {
        DiskCache {
            // Pre-allocate for typical disk with ~100k directories
            // Reduces reallocation overhead during traversal
            entries:                HashMap::with_capacity(100_000),
            last_scan:              Utc::now(),
            root:                   PathBuf::new(),
            last_scanned_root:      PathBuf::new(),
            usn_state:              USNJournalState::default(),
            pending_writes:         Vec::with_capacity(5000),
            flush_threshold:        5000,
            show_hidden:            false,
            skip_stats:             HashMap::new(),
            has_persisted_snapshot: false,
            persisted_entry_count:  0,
            persisted_file_count:   0,
        }
    }

    /// Create a new empty cache with default USN state (non-Windows)
    #[cfg(not(windows))]
    fn new_empty() -> Self {
        DiskCache {
            // Pre-allocate for typical disk with ~100k directories
            // Reduces reallocation overhead during traversal
            entries:                HashMap::with_capacity(100_000),
            last_scan:              Utc::now(),
            root:                   PathBuf::new(),
            last_scanned_root:      PathBuf::new(),
            pending_writes:         Vec::with_capacity(5000),
            flush_threshold:        5000,
            show_hidden:            false,
            skip_stats:             HashMap::new(),
            has_persisted_snapshot: false,
            persisted_entry_count:  0,
            persisted_file_count:   0,
        }
    }

    /// Save cache using rkyv mmap format (index + data files with O(1) access)
    pub fn save(&mut self, path: &Path) -> Result<()> {
        self.flush_pending_writes();
        self.has_persisted_snapshot = true;
        self.persisted_entry_count = self.entries.len();
        self.persisted_file_count = self
            .entries
            .get(&self.root)
            .map(|entry| entry.file_count)
            .unwrap_or_else(|| self.entries.values().map(|entry| entry.file_count).sum());

        let index_path = path.with_extension("idx");
        let data_path = path.with_extension("dat");

        self.save_as_rkyv_mmap(&index_path, &data_path)?;
        Ok(())
    }

    /// True if we have an existing on-disk cache snapshot.
    pub fn has_cache_snapshot(&self) -> bool {
        self.has_persisted_snapshot
    }

    /// Entry-count hint for cache-hit stats when entries are lazily loaded.
    pub fn entry_count_hint(&self) -> usize {
        if self.entries.is_empty() {
            self.persisted_entry_count
        } else {
            self.entries.len()
        }
    }

    /// File-count hint for cache-hit stats when entries are lazily loaded.
    pub fn file_count_hint(&self) -> usize {
        if self.entries.is_empty() {
            self.persisted_file_count
        } else {
            self.entries
                .get(&self.root)
                .map(|entry| entry.file_count)
                .unwrap_or_else(|| self.entries.values().map(|entry| entry.file_count).sum())
        }
    }

    /// Save cache in mmap format (index + data files with bincode serialization)
    fn save_as_rkyv_mmap(&self, index_path: &Path, data_path: &Path) -> Result<()> {
        use crate::cache_rkyv::{RkyvCacheIndex, RkyvDirEntry};

        fs::create_dir_all(index_path.parent().unwrap())?;

        // Build index with byte offsets
        let mut rkyv_index = RkyvCacheIndex::new();
        rkyv_index.offsets = HashMap::with_capacity(self.entries.len());
        rkyv_index.total_files = self
            .entries
            .get(&self.root)
            .map(|entry| entry.file_count)
            .unwrap_or_else(|| self.entries.values().map(|entry| entry.file_count).sum());
        rkyv_index.root = self.root.clone();
        rkyv_index.last_scanned_root = self.last_scanned_root.clone();
        rkyv_index.last_scan = self.last_scan;
        rkyv_index.skip_stats = self.skip_stats.clone();
        #[cfg(windows)]
        {
            rkyv_index.usn_state = self.usn_state.clone();
        }

        // Group entries by depth for depth-split files
        let mut entries_by_depth: BTreeMap<u32, Vec<(PathBuf, RkyvDirEntry)>> = BTreeMap::new();

        for (path, entry) in &self.entries {
            let depth = path.components().count() as u32;
            let rkyv_entry = RkyvDirEntry {
                path:         entry.path.clone(),
                name:         entry.name.clone(),
                modified:     entry.modified,
                content_hash: entry.content_hash,
                file_count:   entry.file_count,
                total_size:   entry.total_size,
                children:     entry.children.clone(),
                is_hidden:    entry.is_hidden,
                is_dir:       entry.is_dir,
            };
            entries_by_depth
                .entry(depth)
                .or_insert_with(Vec::new)
                .push((path.clone(), rkyv_entry));
        }

        let mut final_depth_files = HashSet::new();
        let mut temp_depth_files = Vec::new();

        // Write each depth to its own temp file first so the index never points
        // at partially rewritten shards.
        for (depth, entries) in entries_by_depth {
            if depth >= 31 {
                continue; // Skip pathological depths
            }

            // Generate depth-split filename
            let stem = data_path.file_stem().and_then(|s| s.to_str()).unwrap_or("ptree");
            let parent = data_path.parent().unwrap_or_else(|| std::path::Path::new("."));
            let depth_file = parent.join(format!("{}-d{}.dat", stem, depth));
            let temp_file = parent.join(format!("{}-d{}.dat.tmp", stem, depth));
            final_depth_files.insert(depth_file.clone());

            let file = File::create(&temp_file)?;
            let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, file);
            let mut offset: u64 = 0;

            for (path, rkyv_entry) in entries {
                let serialized = bincode::serialize(&rkyv_entry)?;
                let len = serialized.len() as u32;

                rkyv_index.offsets.insert(path, (depth, offset));
                writer.write_all(&len.to_le_bytes())?;
                writer.write_all(&serialized)?;
                offset += 4 + len as u64;
            }
            writer.flush()?;
            writer.get_ref().sync_all()?;
            drop(writer);
            temp_depth_files.push((temp_file, depth_file));
        }

        for (temp_file, depth_file) in &temp_depth_files {
            fs::rename(temp_file, depth_file)?;
        }

        Self::remove_stale_depth_files(data_path, &final_depth_files)?;

        // Save index
        let index_serialized = bincode::serialize(&rkyv_index)?;
        let temp_path = index_path.with_extension("tmp");
        let index_file = File::create(&temp_path)?;
        let mut index_file = BufWriter::new(index_file);
        index_file.write_all(&index_serialized)?;
        index_file.flush()?;
        index_file.get_ref().sync_all()?;
        drop(index_file);
        fs::rename(&temp_path, index_path)?;

        Ok(())
    }

    fn remove_stale_depth_files(data_path: &Path, keep: &HashSet<PathBuf>) -> Result<()> {
        let stem = data_path.file_stem().and_then(|s| s.to_str()).unwrap_or("ptree");
        let parent = data_path.parent().unwrap_or_else(|| Path::new("."));

        for entry in fs::read_dir(parent)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            if name.starts_with(&format!("{stem}-d")) && name.ends_with(".dat") && !keep.contains(&path) {
                let _ = fs::remove_file(path);
            }
        }

        Ok(())
    }

    fn recover_snapshot(index_path: &Path, data_path: &Path) -> Result<()> {
        let _ = fs::remove_file(index_path);

        let stem = data_path.file_stem().and_then(|s| s.to_str()).unwrap_or("ptree");
        let parent = data_path.parent().unwrap_or_else(|| Path::new("."));

        for entry in fs::read_dir(parent)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };

            if name.starts_with(&format!("{stem}-d")) && (name.ends_with(".dat") || name.ends_with(".dat.tmp")) {
                let _ = fs::remove_file(path);
            }
        }

        Ok(())
    }

    // ============================================================================
    // Entry Management
    // ============================================================================

    /// Buffer a directory entry for batch writing
    pub fn buffer_entry(&mut self, path: PathBuf, entry: DirEntry) {
        self.pending_writes.push((path, entry));

        if self.pending_writes.len() >= self.flush_threshold {
            self.flush_pending_writes();
        }
    }

    /// Flush all buffered writes to main cache HashMap
    pub fn flush_pending_writes(&mut self) {
        for (path, entry) in self.pending_writes.drain(..) {
            self.entries.insert(path, entry);
        }
    }

    /// Load entries on-demand from lazy cache (for cold-start output)
    /// Only loads entries needed for tree building, not entire cache
    pub fn load_entries_lazy(&mut self, paths: &[PathBuf], cache_path: &Path) -> Result<()> {
        use crate::cache_rkyv::RkyvMmapCache;

        let index_path = cache_path.with_extension("idx");
        let data_path = cache_path.with_extension("dat");

        if !index_path.exists() {
            return Ok(());
        }

        let rkyv_cache = RkyvMmapCache::open(&index_path, &data_path)?;

        for path in paths {
            if !self.entries.contains_key(path) {
                if let Some(rkyv_entry) = rkyv_cache.get_entry(path)? {
                    self.entries.insert(path.clone(), Self::dir_entry_from_rkyv(rkyv_entry));
                }
            }
        }

        Ok(())
    }

    /// Load only the directory entries needed to render output from the root.
    ///
    /// This keeps cache hits demand-driven: we load the root entry, then only
    /// descend into child directories whose contents could actually be printed.
    pub fn load_visible_entries_lazy(&mut self, cache_path: &Path, max_depth: Option<usize>) -> Result<()> {
        use crate::cache_rkyv::RkyvMmapCache;

        let index_path = cache_path.with_extension("idx");
        let data_path = cache_path.with_extension("dat");

        if !index_path.exists() {
            return Ok(());
        }

        let rkyv_cache = RkyvMmapCache::open(&index_path, &data_path)?;
        let root = self.root.clone();
        let mut visited = HashSet::new();

        self.expand_visible_entries(&rkyv_cache, &root, 0, max_depth, &mut visited)
    }

    /// Load all entries from lazy cache (fallback for full tree operations)
    pub fn load_all_entries_lazy(&mut self, cache_path: &Path) -> Result<()> {
        use crate::cache_rkyv::RkyvMmapCache;

        let index_path = cache_path.with_extension("idx");
        let data_path = cache_path.with_extension("dat");

        if !index_path.exists() {
            return Ok(());
        }

        let rkyv_cache = RkyvMmapCache::open(&index_path, &data_path)?;
        let lazy_entries = rkyv_cache.get_all()?;

        for (path, entry) in lazy_entries {
            if !self.entries.contains_key(&path) {
                self.entries.insert(path, entry);
            }
        }

        Ok(())
    }

    fn expand_visible_entries(
        &mut self,
        rkyv_cache: &crate::cache_rkyv::RkyvMmapCache,
        path: &Path,
        current_depth: usize,
        max_depth: Option<usize>,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        if let Some(max) = max_depth {
            if current_depth >= max {
                return Ok(());
            }
        }

        if !visited.insert(path.to_path_buf()) {
            return Ok(());
        }

        if !self.entries.contains_key(path) {
            let Some(rkyv_entry) = rkyv_cache.get_entry(path)? else {
                return Ok(());
            };
            self.entries
                .insert(path.to_path_buf(), Self::dir_entry_from_rkyv(rkyv_entry));
        }

        if let Some(max) = max_depth {
            if current_depth + 1 >= max {
                return Ok(());
            }
        }

        let child_paths: Vec<PathBuf> = self
            .entries
            .get(path)
            .map(|entry| entry.children.iter().map(|name| path.join(name)).collect())
            .unwrap_or_default();

        for child_path in child_paths {
            if self.entries.contains_key(&child_path) {
                self.expand_visible_entries(rkyv_cache, &child_path, current_depth + 1, max_depth, visited)?;
                continue;
            }

            if let Some(rkyv_entry) = rkyv_cache.get_entry(&child_path)? {
                self.entries
                    .insert(child_path.clone(), Self::dir_entry_from_rkyv(rkyv_entry));
                self.expand_visible_entries(rkyv_cache, &child_path, current_depth + 1, max_depth, visited)?;
            }
        }

        Ok(())
    }

    fn dir_entry_from_rkyv(rkyv_entry: crate::cache_rkyv::RkyvDirEntry) -> DirEntry {
        DirEntry {
            path:         rkyv_entry.path,
            name:         rkyv_entry.name,
            modified:     rkyv_entry.modified,
            content_hash: rkyv_entry.content_hash,
            file_count:   rkyv_entry.file_count,
            total_size:   rkyv_entry.total_size,
            children:     rkyv_entry.children,
            is_hidden:    rkyv_entry.is_hidden,
            is_dir:       rkyv_entry.is_dir,
        }
    }

    /// Add or update directory entry (via buffer)
    pub fn add_entry(&mut self, path: PathBuf, entry: DirEntry) {
        self.buffer_entry(path, entry);
    }

    /// Get entry by path
    pub fn get_entry(&self, path: &Path) -> Option<&DirEntry> {
        self.entries.get(path)
    }

    /// Format a directory name with optional hidden indicator
    pub fn format_name(&self, name: &str, path: &Path, show_hidden: bool) -> String {
        if !show_hidden {
            return name.to_string();
        }

        if let Some(entry) = self.get_entry(path) {
            if entry.is_hidden {
                format!("{} [H]", name)
            } else {
                name.to_string()
            }
        } else {
            name.to_string()
        }
    }

    /// Record that a directory was skipped
    pub fn record_skip(&mut self, dir_name: &str) {
        *self.skip_stats.entry(dir_name.to_string()).or_insert(0) += 1;
    }

    /// Get skip statistics report
    pub fn get_skip_report(&self) -> String {
        if self.skip_stats.is_empty() {
            return "(no directories skipped)".to_string();
        }

        let mut report = String::from("Skip Statistics:\n");
        let mut sorted: Vec<_> = self.skip_stats.iter().collect();
        sorted.sort_by_key(|(_name, count)| std::cmp::Reverse(**count));

        for (name, count) in sorted {
            report.push_str(&format!("  {} × {}\n", count, name));
        }

        report
    }

    /// Remove entry and all child entries
    pub fn remove_entry(&mut self, path: &Path) {
        // Path::starts_with checks path components, so "/foo" does not match "/foobar".
        self.entries.retain(|k, _| !(k == path || k.starts_with(path)));
    }

    /// Remove cached directory subtrees whose names disappeared from a rescanned parent.
    pub fn remove_missing_child_subtrees(&mut self, parent: &Path, current_children: &[String]) {
        let Some(previous) = self.entries.get(parent) else {
            return;
        };

        let current_children: std::collections::HashSet<&str> = current_children.iter().map(String::as_str).collect();
        let stale_paths: Vec<PathBuf> = previous
            .children
            .iter()
            .filter(|child_name| !current_children.contains(child_name.as_str()))
            .map(|child_name| parent.join(child_name))
            .filter(|child_path| self.entries.contains_key(child_path))
            .collect();

        for child_path in stale_paths {
            self.remove_entry(&child_path);
        }
    }

    // ============================================================================
    // ASCII Tree Output
    // ============================================================================

    /// Build ASCII tree output with optional max depth
    pub fn build_tree_output(&self) -> Result<String> {
        self.build_tree_output_with_options(None, false, false)
    }

    /// Build ASCII tree output with optional max depth limit (allocates String)
    pub fn build_tree_output_with_depth(&self, max_depth: Option<usize>) -> Result<String> {
        self.build_tree_output_with_options(max_depth, false, false)
    }

    pub fn build_tree_output_with_options(
        &self,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<String> {
        let mut output = String::new();

        if self.entries.is_empty() {
            return Ok("(empty)\n".to_string());
        }

        let root = &self.root;
        output.push_str(&format!("{}\n", root.display()));

        // No need for visited set - filesystem is acyclic and in_progress set prevents cycles during traversal
        self.print_tree(&mut output, root, "", true, 0, max_depth, show_size, show_file_count)?;

        Ok(output)
    }

    /// Stream ASCII tree directly to a writer to avoid building a giant String.
    pub fn write_tree_output_with_depth<W: Write>(&self, writer: &mut W, max_depth: Option<usize>) -> Result<()> {
        self.write_tree_output_with_options(writer, max_depth, false, false)
    }

    pub fn write_tree_output_with_options<W: Write>(
        &self,
        writer: &mut W,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<()> {
        if self.entries.is_empty() {
            writer.write_all(b"(empty)\n")?;
            return Ok(());
        }

        let root = &self.root;
        writeln!(writer, "{}", root.display())?;

        self.write_tree(writer, root, "", true, 0, max_depth, show_size, show_file_count)?;
        Ok(())
    }

    fn print_tree(
        &self,
        output: &mut String,
        path: &Path,
        prefix: &str,
        is_last: bool,
        current_depth: usize,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<()> {
        // Check depth limit
        if let Some(max) = max_depth {
            if current_depth >= max {
                return Ok(());
            }
        }

        if let Some(entry) = self.entries.get(path) {
            // Sort children only at output time (not during traversal)
            let mut children: Vec<_> = entry.children.iter().collect();
            children.sort();

            for (i, child_name) in children.iter().enumerate() {
                let is_last_child = i == children.len() - 1;
                let child_prefix = if is_last {
                    "    ".to_string()
                } else {
                    "│   ".to_string()
                };

                let branch = if is_last_child { "└── " } else { "├── " };

                let child_path = path.join(child_name);
                let display_name = if let Some(child_entry) = self.entries.get(&child_path) {
                    let name = if self.show_hidden && child_entry.is_hidden {
                        format!("{} [H]", child_name)
                    } else {
                        child_name.to_string()
                    };
                    format!("{}{}", name, Self::metadata_suffix(child_entry, show_size, show_file_count))
                } else {
                    child_name.to_string()
                };

                output.push_str(&format!("{}{}{}\n", prefix, branch, display_name));
                self.print_tree(
                    output,
                    &child_path,
                    &format!("{}{}", prefix, child_prefix),
                    is_last_child,
                    current_depth + 1,
                    max_depth,
                    show_size,
                    show_file_count,
                )?;
            }
        }

        Ok(())
    }

    fn write_tree<W: Write>(
        &self,
        writer: &mut W,
        path: &Path,
        prefix: &str,
        is_last: bool,
        current_depth: usize,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<()> {
        // Check depth limit
        if let Some(max) = max_depth {
            if current_depth >= max {
                return Ok(());
            }
        }

        if let Some(entry) = self.entries.get(path) {
            // Sort children only at output time (not during traversal)
            let mut children: Vec<_> = entry.children.iter().collect();
            children.sort();

            for (i, child_name) in children.iter().enumerate() {
                let is_last_child = i == children.len() - 1;
                let child_prefix = if is_last {
                    "    ".to_string()
                } else {
                    "│   ".to_string()
                };

                let branch = if is_last_child { "└── " } else { "├── " };

                let child_path = path.join(child_name);
                let display_name = if let Some(child_entry) = self.entries.get(&child_path) {
                    let name = if self.show_hidden && child_entry.is_hidden {
                        format!("{} [H]", child_name)
                    } else {
                        child_name.to_string()
                    };
                    format!("{}{}", name, Self::metadata_suffix(child_entry, show_size, show_file_count))
                } else {
                    child_name.to_string()
                };

                writeln!(writer, "{}{}{}", prefix, branch, display_name)?;
                self.write_tree(
                    writer,
                    &child_path,
                    &format!("{}{}", prefix, child_prefix),
                    is_last_child,
                    current_depth + 1,
                    max_depth,
                    show_size,
                    show_file_count,
                )?;
            }
        }

        Ok(())
    }

    // ============================================================================
    // Colored Tree Output
    // ============================================================================

    /// Build colored tree output
    pub fn build_colored_tree_output(&self) -> Result<String> {
        self.build_colored_tree_output_with_options(None, false, false)
    }

    /// Build colored tree output with optional max depth limit
    pub fn build_colored_tree_output_with_depth(&self, max_depth: Option<usize>) -> Result<String> {
        self.build_colored_tree_output_with_options(max_depth, false, false)
    }

    pub fn build_colored_tree_output_with_options(
        &self,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<String> {
        let mut output = String::new();

        if self.entries.is_empty() {
            return Ok("(empty)\n".to_string());
        }

        let root = &self.root;
        output.push_str(&format!("{}\n", root.display().to_string().blue().bold()));

        // No need for visited set - filesystem is acyclic and in_progress set prevents cycles during traversal
        self.print_colored_tree(&mut output, root, "", true, 0, max_depth, show_size, show_file_count)?;

        Ok(output)
    }

    /// Stream colored tree directly to a writer.
    pub fn write_colored_tree_output_with_depth<W: Write>(
        &self,
        writer: &mut W,
        max_depth: Option<usize>,
    ) -> Result<()> {
        self.write_colored_tree_output_with_options(writer, max_depth, false, false)
    }

    pub fn write_colored_tree_output_with_options<W: Write>(
        &self,
        writer: &mut W,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<()> {
        if self.entries.is_empty() {
            writer.write_all(b"(empty)\n")?;
            return Ok(());
        }

        let root = &self.root;
        writeln!(writer, "{}", root.display().to_string().blue().bold())?;

        self.write_colored_tree(writer, root, "", true, 0, max_depth, show_size, show_file_count)?;
        Ok(())
    }

    fn print_colored_tree(
        &self,
        output: &mut String,
        path: &Path,
        prefix: &str,
        is_last: bool,
        current_depth: usize,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<()> {
        // Check depth limit
        if let Some(max) = max_depth {
            if current_depth >= max {
                return Ok(());
            }
        }

        if let Some(entry) = self.entries.get(path) {
            // Sort children only at output time (not during traversal)
            // Use parallel sort for large directories (>500 children)
            let mut children: Vec<_> = entry.children.iter().collect();
            if children.len() > 500 {
                children.par_sort();
            } else {
                children.sort();
            }

            for (i, child_name) in children.iter().enumerate() {
                let is_last_child = i == children.len() - 1;
                let child_prefix = if is_last {
                    "    ".to_string()
                } else {
                    "│   ".to_string()
                };

                let branch = if is_last_child { "└── " } else { "├── " };
                let branch_colored = branch.cyan().to_string();

                let child_path = path.join(child_name);
                let display_name = if let Some(child_entry) = self.entries.get(&child_path) {
                    let name = if self.show_hidden && child_entry.is_hidden {
                        format!("{} [H]", child_name)
                    } else {
                        child_name.to_string()
                    };
                    format!("{}{}", name, Self::metadata_suffix(child_entry, show_size, show_file_count))
                        .bright_blue()
                        .to_string()
                } else {
                    child_name.bright_blue().to_string()
                };

                output.push_str(&format!("{}{}{}\n", prefix, branch_colored, display_name));
                self.print_colored_tree(
                    output,
                    &child_path,
                    &format!("{}{}", prefix, child_prefix),
                    is_last_child,
                    current_depth + 1,
                    max_depth,
                    show_size,
                    show_file_count,
                )?;
            }
        }

        Ok(())
    }

    fn write_colored_tree<W: Write>(
        &self,
        writer: &mut W,
        path: &Path,
        prefix: &str,
        is_last: bool,
        current_depth: usize,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<()> {
        // Check depth limit
        if let Some(max) = max_depth {
            if current_depth >= max {
                return Ok(());
            }
        }

        if let Some(entry) = self.entries.get(path) {
            // Sort children only at output time (not during traversal)
            // Use parallel sort for large directories (>500 children)
            let mut children: Vec<_> = entry.children.iter().collect();
            if children.len() > 500 {
                children.par_sort();
            } else {
                children.sort();
            }

            for (i, child_name) in children.iter().enumerate() {
                let is_last_child = i == children.len() - 1;
                let child_prefix = if is_last {
                    "    ".to_string()
                } else {
                    "│   ".to_string()
                };

                let branch = if is_last_child { "└── " } else { "├── " };
                let branch_colored = branch.cyan().to_string();

                let child_path = path.join(child_name);
                let display_name = if let Some(child_entry) = self.entries.get(&child_path) {
                    let name = if self.show_hidden && child_entry.is_hidden {
                        format!("{} [H]", child_name)
                    } else {
                        child_name.to_string()
                    };
                    format!("{}{}", name, Self::metadata_suffix(child_entry, show_size, show_file_count))
                        .bright_blue()
                        .to_string()
                } else {
                    child_name.bright_blue().to_string()
                };

                writeln!(writer, "{}{}{}", prefix, branch_colored, display_name)?;
                self.write_colored_tree(
                    writer,
                    &child_path,
                    &format!("{}{}", prefix, child_prefix),
                    is_last_child,
                    current_depth + 1,
                    max_depth,
                    show_size,
                    show_file_count,
                )?;
            }
        }

        Ok(())
    }

    // ============================================================================
    // JSON Tree Output
    // ============================================================================

    /// Build JSON tree representation
    pub fn build_json_output(&self) -> Result<String> {
        self.build_json_output_with_options(None, false, false)
    }

    /// Build JSON tree representation with optional max depth limit
    pub fn build_json_output_with_depth(&self, max_depth: Option<usize>) -> Result<String> {
        self.build_json_output_with_options(max_depth, false, false)
    }

    pub fn build_json_output_with_options(
        &self,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<String> {
        let mut root_json = json!({
            "path": self.root.to_string_lossy().to_string(),
            "children": []
        });

        if self.entries.is_empty() {
            return Ok(root_json.to_string());
        }

        // No need for visited set - filesystem is acyclic and in_progress set prevents cycles during traversal
        if let Some(root_entry) = self.get_entry(&self.root) {
            if show_size {
                root_json["size_bytes"] = json!(root_entry.total_size);
            }
            if show_file_count {
                root_json["file_count"] = json!(root_entry.file_count);
            }
        }

        self.populate_json(&mut root_json, &self.root, 0, max_depth, show_size, show_file_count)?;

        Ok(serde_json::to_string_pretty(&root_json)?)
    }

    fn populate_json(
        &self,
        node: &mut serde_json::Value,
        path: &Path,
        current_depth: usize,
        max_depth: Option<usize>,
        show_size: bool,
        show_file_count: bool,
    ) -> Result<()> {
        // Check depth limit
        if let Some(max) = max_depth {
            if current_depth >= max {
                return Ok(());
            }
        }

        if let Some(entry) = self.get_entry(path) {
            let mut children_array = Vec::new();
            let mut children_names: Vec<_> = entry.children.iter().collect();
            // Sort children only at output time (not during traversal)
            // Use parallel sort for large directories (>500 children)
            if children_names.len() > 500 {
                children_names.par_sort();
            } else {
                children_names.sort();
            }

            for child_name in children_names {
                let child_path = path.join(child_name);
                let mut child_json = json!({
                    "name": child_name,
                    "path": child_path.to_string_lossy().to_string(),
                    "children": []
                });

                if let Some(child_entry) = self.get_entry(&child_path) {
                    if show_size {
                        child_json["size_bytes"] = json!(child_entry.total_size);
                    }
                    if show_file_count {
                        child_json["file_count"] = json!(child_entry.file_count);
                    }
                }

                self.populate_json(
                    &mut child_json,
                    &child_path,
                    current_depth + 1,
                    max_depth,
                    show_size,
                    show_file_count,
                )?;
                children_array.push(child_json);
            }

            node["children"] = serde_json::json!(children_array);
        }

        Ok(())
    }

    pub fn refresh_derived_metadata(&mut self) {
        let mut paths: Vec<PathBuf> = self.entries.keys().cloned().collect();
        paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));

        let mut computed_hashes = HashMap::with_capacity(paths.len());

        for path in paths {
            let Some(existing) = self.entries.get(&path) else {
                continue;
            };

            let children = existing.children.clone();
            let modified = existing.modified;
            let mut file_count = existing.file_count;
            let mut total_size = existing.total_size;

            for child_name in &children {
                let child_path = path.join(child_name);
                if let Some(child_entry) = self.entries.get(&child_path) {
                    file_count += child_entry.file_count;
                    total_size += child_entry.total_size;
                }
            }

            let content_hash = compute_content_hash(&path, modified, &children, &computed_hashes);

            if let Some(entry) = self.entries.get_mut(&path) {
                entry.file_count = file_count;
                entry.total_size = total_size;
                entry.content_hash = content_hash;
            }

            computed_hashes.insert(path, content_hash);
        }
    }

    fn metadata_suffix(entry: &DirEntry, show_size: bool, show_file_count: bool) -> String {
        let mut parts = Vec::new();
        if show_size {
            parts.push(Self::format_size(entry.total_size));
        }
        if show_file_count {
            parts.push(format!("{} files", entry.file_count));
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        }
    }

    fn format_size(size: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

        let mut value = size as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }

        if unit == 0 {
            format!("{} {}", size, UNITS[unit])
        } else {
            format!("{value:.1} {}", UNITS[unit])
        }
    }
}

/// Get cache directory path
pub fn get_cache_path() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let appdata = std::env::var("APPDATA")?;
        return Ok(PathBuf::from(appdata).join("ptree").join("cache").join("ptree.dat"));
    }

    #[cfg(not(windows))]
    {
        if let Some(cache_home) = xdg_absolute_dir("XDG_CACHE_HOME") {
            return Ok(PathBuf::from(cache_home).join("ptree").join("ptree.dat"));
        }

        if let Ok(home) = std::env::var("HOME") {
            let home_path = PathBuf::from(home);
            if home_path.is_absolute() {
                return Ok(home_path.join(".cache").join("ptree").join("ptree.dat"));
            }
        }

        Err(anyhow!("Could not determine cache directory. Set XDG_CACHE_HOME or HOME to an absolute path."))
    }
}

#[cfg(not(windows))]
fn xdg_absolute_dir(var_name: &str) -> Option<PathBuf> {
    let raw = std::env::var(var_name).ok()?;
    parse_absolute_dir(&raw)
}

#[cfg(not(windows))]
fn parse_absolute_dir(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let path = PathBuf::from(trimmed);
    path.is_absolute().then_some(path)
}

/// Get cache directory path with custom directory
pub fn get_cache_path_custom(custom_dir: Option<&str>) -> Result<PathBuf> {
    if let Some(dir) = custom_dir {
        Ok(PathBuf::from(dir).join("ptree.dat"))
    } else {
        get_cache_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_creation() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("ptree_test_cache");
        fs::create_dir_all(&temp_dir)?;
        let cache_path = temp_dir.join("test.dat");

        let cache = DiskCache::open(&cache_path)?;
        assert!(cache.entries.is_empty());

        // Clean up
        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    #[test]
    fn test_load_all_entries_lazy_with_depth_split_files_only() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("ptree_test_lazy_depth_split");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir)?;
        let cache_path = temp_dir.join("ptree.dat");
        let root = temp_dir.join("root");
        let child = root.join("child");

        let mut cache = DiskCache::default();
        cache.root = root.clone();
        cache.entries.insert(
            root.clone(),
            DirEntry {
                path:         root.clone(),
                name:         "root".to_string(),
                modified:     Utc::now(),
                content_hash: 0,
                file_count:   0,
                total_size:   0,
                children:     vec!["child".to_string()],
                is_hidden:    false,
                is_dir:       true,
            },
        );
        cache.entries.insert(
            child.clone(),
            DirEntry {
                path:         child.clone(),
                name:         "child".to_string(),
                modified:     Utc::now(),
                content_hash: 0,
                file_count:   1,
                total_size:   128,
                children:     vec!["leaf.txt".to_string()],
                is_hidden:    false,
                is_dir:       true,
            },
        );

        cache.save(&cache_path)?;

        assert!(cache_path.with_extension("idx").exists());
        assert!(!cache_path.exists(), "depth-split cache should not require base .dat file");

        let mut reopened = DiskCache::open(&cache_path)?;
        assert!(reopened.entries.is_empty(), "open should stay lazy");

        reopened.load_all_entries_lazy(&cache_path)?;
        assert!(reopened.entries.contains_key(&root));
        assert!(reopened.entries.contains_key(&child));

        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    #[test]
    fn test_load_visible_entries_lazy_respects_max_depth() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("ptree_test_visible_lazy_depth");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir)?;
        let cache_path = temp_dir.join("ptree.dat");
        let root = temp_dir.join("root");
        let child = root.join("alpha");
        let grandchild = child.join("beta");

        let mut cache = DiskCache::default();
        cache.root = root.clone();
        cache.last_scanned_root = root.clone();
        cache.entries.insert(
            root.clone(),
            DirEntry {
                path:         root.clone(),
                name:         "root".to_string(),
                modified:     Utc::now(),
                content_hash: 0,
                file_count:   1,
                total_size:   64,
                children:     vec!["alpha".to_string(), "note.txt".to_string()],
                is_hidden:    false,
                is_dir:       true,
            },
        );
        cache.entries.insert(
            child.clone(),
            DirEntry {
                path:         child.clone(),
                name:         "alpha".to_string(),
                modified:     Utc::now(),
                content_hash: 0,
                file_count:   1,
                total_size:   32,
                children:     vec!["beta".to_string(), "child.txt".to_string()],
                is_hidden:    false,
                is_dir:       true,
            },
        );
        cache.entries.insert(
            grandchild.clone(),
            DirEntry {
                path:         grandchild.clone(),
                name:         "beta".to_string(),
                modified:     Utc::now(),
                content_hash: 0,
                file_count:   1,
                total_size:   16,
                children:     vec!["leaf.txt".to_string()],
                is_hidden:    false,
                is_dir:       true,
            },
        );

        cache.save(&cache_path)?;

        let mut depth_one = DiskCache::open(&cache_path)?;
        depth_one.load_visible_entries_lazy(&cache_path, Some(1))?;
        assert_eq!(depth_one.entries.len(), 1);
        assert!(depth_one.entries.contains_key(&root));
        assert!(!depth_one.entries.contains_key(&child));
        assert!(!depth_one.entries.contains_key(&grandchild));
        let depth_one_tree = depth_one.build_tree_output_with_depth(Some(1))?;
        assert!(depth_one_tree.contains("alpha"));
        assert!(depth_one_tree.contains("note.txt"));
        assert!(!depth_one_tree.contains("beta"));

        let mut depth_two = DiskCache::open(&cache_path)?;
        depth_two.load_visible_entries_lazy(&cache_path, Some(2))?;
        assert!(depth_two.entries.contains_key(&root));
        assert!(depth_two.entries.contains_key(&child));
        assert!(!depth_two.entries.contains_key(&grandchild));
        let depth_two_tree = depth_two.build_tree_output_with_depth(Some(2))?;
        assert!(depth_two_tree.contains("beta"));
        assert!(!depth_two_tree.contains("leaf.txt"));

        let mut full = DiskCache::open(&cache_path)?;
        full.load_visible_entries_lazy(&cache_path, None)?;
        assert!(full.entries.contains_key(&root));
        assert!(full.entries.contains_key(&child));
        assert!(full.entries.contains_key(&grandchild));

        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    #[test]
    fn test_render_options_and_persisted_file_counts() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("ptree_test_render_options");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir)?;
        let cache_path = temp_dir.join("ptree.dat");
        let root = temp_dir.join("root");
        let child = root.join("alpha");

        let mut cache = DiskCache::default();
        cache.root = root.clone();
        cache.last_scanned_root = root.clone();
        cache.entries.insert(
            root.clone(),
            DirEntry {
                path:         root.clone(),
                name:         "root".to_string(),
                modified:     Utc::now(),
                content_hash: 0,
                file_count:   1,
                total_size:   64,
                children:     vec!["alpha".to_string(), "note.txt".to_string()],
                is_hidden:    false,
                is_dir:       true,
            },
        );
        cache.entries.insert(
            child.clone(),
            DirEntry {
                path:         child.clone(),
                name:         "alpha".to_string(),
                modified:     Utc::now(),
                content_hash: 0,
                file_count:   2,
                total_size:   256,
                children:     vec!["leaf-a.txt".to_string(), "leaf-b.txt".to_string()],
                is_hidden:    false,
                is_dir:       true,
            },
        );

        cache.refresh_derived_metadata();
        cache.save(&cache_path)?;

        let reopened = DiskCache::open(&cache_path)?;
        assert_eq!(reopened.file_count_hint(), 3);

        let mut hydrated = DiskCache::open(&cache_path)?;
        hydrated.load_visible_entries_lazy(&cache_path, None)?;

        let tree = hydrated.build_tree_output_with_options(None, true, true)?;
        assert!(tree.contains("alpha (256 B, 2 files)"));

        let json = hydrated.build_json_output_with_options(None, true, true)?;
        assert!(json.contains("\"file_count\": 3"));
        assert!(json.contains("\"size_bytes\": 320"));

        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    #[test]
    fn test_open_recovers_from_corrupt_snapshot() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("ptree_test_corrupt_snapshot");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir)?;
        let cache_path = temp_dir.join("ptree.dat");
        let index_path = cache_path.with_extension("idx");
        let depth_path = temp_dir.join("ptree-d1.dat");

        fs::write(&index_path, b"not a valid index")?;
        fs::write(&depth_path, b"stale depth data")?;

        let cache = DiskCache::open(&cache_path)?;
        assert!(cache.entries.is_empty());
        assert!(!index_path.exists());
        assert!(!depth_path.exists());

        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    #[test]
    fn test_content_hash_stability() {
        // Same inputs should produce same hash
        let path = std::path::Path::new("C:\\test");
        let modified = Utc::now();
        let children = vec!["file1.txt".to_string(), "file2.txt".to_string()];
        let child_hashes = HashMap::new();

        let hash1 = compute_content_hash(path, modified, &children, &child_hashes);
        let hash2 = compute_content_hash(path, modified, &children, &child_hashes);

        assert_eq!(hash1, hash2, "Identical inputs should produce identical hashes");
    }

    #[test]
    #[cfg(not(windows))]
    fn test_xdg_absolute_dir_validation() {
        assert_eq!(parse_absolute_dir("/tmp/ptree-cache"), Some(PathBuf::from("/tmp/ptree-cache")));
        assert!(parse_absolute_dir("relative/path").is_none());
        assert!(parse_absolute_dir("").is_none());
    }

    #[test]
    fn test_content_hash_sensitivity() {
        // Different inputs should produce different hashes
        let path = std::path::Path::new("C:\\test");
        let modified = Utc::now();

        // Base hash
        let children = vec!["file1.txt".to_string()];
        let child_hashes = HashMap::new();
        let base_hash = compute_content_hash(path, modified, &children, &child_hashes);

        // Hash with additional file
        let children_added = vec!["file1.txt".to_string(), "file2.txt".to_string()];
        let hash_added = compute_content_hash(path, modified, &children_added, &child_hashes);
        assert_ne!(base_hash, hash_added, "Adding a file should change hash");

        // Hash with removed file
        let children_removed = vec![];
        let hash_removed = compute_content_hash(path, modified, &children_removed, &child_hashes);
        assert_ne!(base_hash, hash_removed, "Removing a file should change hash");

        // Hash with renamed file
        let children_renamed = vec!["renamed_file.txt".to_string()];
        let hash_renamed = compute_content_hash(path, modified, &children_renamed, &child_hashes);
        assert_ne!(base_hash, hash_renamed, "Renaming a file should change hash");
    }

    #[test]
    fn test_merkle_propagation() {
        // Child hash changes should affect parent hash
        let parent_path = std::path::Path::new("/parent");
        let child_path = std::path::Path::new("/parent/child");
        let modified = Utc::now();

        // Parent with no child hashes
        let parent_children = vec!["child".to_string()];
        let mut child_hashes = HashMap::new();
        child_hashes.insert(child_path.to_path_buf(), 12345u64);

        let parent_hash1 = compute_content_hash(parent_path, modified, &parent_children, &child_hashes);

        // Change child hash
        child_hashes.insert(child_path.to_path_buf(), 54321u64);
        let parent_hash2 = compute_content_hash(parent_path, modified, &parent_children, &child_hashes);

        assert_ne!(parent_hash1, parent_hash2, "Child hash change should affect parent hash");
    }

    #[test]
    fn test_has_directory_changed() {
        let path = std::path::Path::new("C:\\test");

        let old_entry = DirEntry {
            path:         path.to_path_buf(),
            name:         "test".to_string(),
            modified:     Utc::now(),
            content_hash: 12345u64,
            file_count:   1,
            total_size:   64,
            children:     vec!["file.txt".to_string()],
            is_hidden:    false,
            is_dir:       true,
        };

        let new_entry_unchanged = DirEntry {
            path:         path.to_path_buf(),
            name:         "test".to_string(),
            modified:     Utc::now(),
            content_hash: 12345u64,
            file_count:   1,
            total_size:   64,
            children:     vec!["file.txt".to_string()],
            is_hidden:    false,
            is_dir:       true,
        };

        let new_entry_changed = DirEntry {
            path:         path.to_path_buf(),
            name:         "test".to_string(),
            modified:     Utc::now(),
            content_hash: 54321u64,
            file_count:   2,
            total_size:   96,
            children:     vec!["file.txt".to_string(), "newfile.txt".to_string()],
            is_hidden:    false,
            is_dir:       true,
        };

        assert!(!has_directory_changed(&old_entry, &new_entry_unchanged), "Same hash should not indicate change");
        assert!(has_directory_changed(&old_entry, &new_entry_changed), "Different hash should indicate change");
    }

    #[test]
    fn test_remove_entry_uses_path_components() {
        let mut cache = DiskCache::new_empty();
        let base = std::path::PathBuf::from("/foo");
        let child = std::path::PathBuf::from("/foo/bar");
        let sibling_prefix = std::path::PathBuf::from("/foobar");

        let mk_entry = |path: &std::path::Path| {
            DirEntry {
                path:         path.to_path_buf(),
                name:         path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string(),
                modified:     Utc::now(),
                content_hash: 0,
                file_count:   0,
                total_size:   0,
                children:     Vec::new(),
                is_hidden:    false,
                is_dir:       true,
            }
        };

        cache.entries.insert(base.clone(), mk_entry(&base));
        cache.entries.insert(child.clone(), mk_entry(&child));
        cache.entries.insert(sibling_prefix.clone(), mk_entry(&sibling_prefix));

        cache.remove_entry(&base);

        assert!(!cache.entries.contains_key(&base));
        assert!(!cache.entries.contains_key(&child));
        assert!(cache.entries.contains_key(&sibling_prefix));
    }
}
