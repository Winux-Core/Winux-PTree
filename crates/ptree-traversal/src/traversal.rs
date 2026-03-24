use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Utc;
use parking_lot::RwLock;
use ptree_cache::{compute_content_hash, DirEntry, DiskCache};
use ptree_core::Args;
use ptree_incremental::{build_changed_directory_set, IncrementalChange};

fn system_time_to_utc(time: std::time::SystemTime) -> chrono::DateTime<Utc> {
    chrono::DateTime::<Utc>::from(time)
}

/// Debug timing information and statistics
#[derive(Debug, Clone)]
pub struct DebugInfo {
    pub is_first_run:        bool,
    pub incremental_refresh: bool,
    pub scan_root:           PathBuf,
    pub cache_used:          bool,
    pub lazy_load_time:      Duration,
    pub traversal_time:      Duration,
    pub save_time:           Duration,
    pub cache_index_time:    Duration,
    pub total_dirs:          usize,
    pub total_files:         usize,
    pub threads_used:        usize,
}

/// Shared state for parallel DFS traversal across worker threads
pub struct TraversalState {
    /// Work queue: directories to be processed
    pub work_queue: Arc<Mutex<VecDeque<PathBuf>>>,

    /// Shared cache across all worker threads
    pub cache: Arc<RwLock<DiskCache>>,

    /// Track directories currently being processed (prevents duplicates)
    pub in_progress: Arc<Mutex<std::collections::HashSet<PathBuf>>>,

    /// Directories to skip during traversal
    pub skip_dirs: std::collections::HashSet<String>,

    /// Directories that changed since last scan (for incremental updates)
    /// If set, only these directories will be rescanned; unset means full scan
    pub changed_dirs_filter: Option<std::collections::HashSet<PathBuf>>,

    /// Skip statistics: count of skipped directories (shared across threads)
    pub skip_stats: Arc<Mutex<std::collections::HashMap<String, usize>>>,
}

struct LiveDirectorySummary {
    content_hash: u64,
    file_count:   usize,
    total_size:   u64,
}

/// Traverse disk and update cache (per README spec)
///
/// Cache Correctness Model:
/// - First scan: Produces accurate snapshot of filesystem at scan time
/// - After scan, before USN refresh: Cache may lag live filesystem changes (eventual consistency)
/// - After 1-hour interval: USN Journal refresh synchronizes cache to current state
/// - Cache invariant: Always correct given sufficient time (1-hour refresh window)
///
/// USN Journal Management:
/// - Max size: 500MB (hardcoded)
/// - On wrap-around: Cache is increased up to 500MB limit
/// - If 500MB capacity is reached and wrap occurs: Automatic fallback to full rescan
/// - USN entries are cached; refresh interval is 1 hour from last cache write
///
/// Returns DebugInfo with timing information if --debug is enabled
///
/// Algorithm:
/// 1. On first run: Full scan of specified drive and cache results
/// 2. On subsequent runs: Check cache age and USN Journal
/// 3. If cache < 1 hour old: Use cache (instant return)
/// 4. If cache >= 1 hour old: Check USN Journal for wrap-around
/// 5. If wrap-around detected: Full rescan of specified drive
/// 6. Initialize work queue with drive root
/// 7. Spawn worker threads that process queue in parallel (iterative DFS)
/// 8. Flush all pending writes and save cache atomically
pub fn traverse_disk(drive: &char, cache: &mut DiskCache, args: &Args, cache_path: &Path) -> Result<DebugInfo> {
    traverse_disk_with_filter(drive, cache, args, cache_path, None)
}

pub fn traverse_disk_incremental(
    drive: &char,
    cache: &mut DiskCache,
    args: &Args,
    cache_path: &Path,
    changes: &[IncrementalChange],
) -> Result<DebugInfo> {
    let scan_root = resolve_scan_root(drive, args)?;
    let changed_dirs = build_changed_directory_set(&scan_root, changes);
    traverse_disk_with_filter(drive, cache, args, cache_path, Some(changed_dirs))
}

fn traverse_disk_with_filter(
    drive: &char,
    cache: &mut DiskCache,
    args: &Args,
    cache_path: &Path,
    changed_dirs_filter: Option<std::collections::HashSet<PathBuf>>,
) -> Result<DebugInfo> {
    #[cfg(not(windows))]
    let _ = drive;

    let incremental_refresh = changed_dirs_filter.is_some();
    let scan_root = resolve_scan_root(drive, args)?;
    let skip_dirs = args.skip_dirs();

    // Verify scan root exists and is a directory
    if !scan_root.exists() {
        anyhow::bail!("Scan root does not exist: {}", scan_root.display());
    }
    if !scan_root.is_dir() {
        anyhow::bail!("Scan root is not a directory: {}", scan_root.display());
    }

    let is_first_run = !cache.has_cache_snapshot();
    cache.root = scan_root.clone();

    // Ensure root directory is added to cache (important for --no-cache mode)
    if is_first_run && !cache.entries.contains_key(&scan_root) {
        let root_entry = DirEntry {
            path:         scan_root.clone(),
            name:         scan_root
                .file_name()
                .and_then(|n| n.to_str().map(|s| s.to_string()))
                .unwrap_or_default(),
            modified:     fs::metadata(&scan_root)
                .and_then(|metadata| metadata.modified())
                .map(system_time_to_utc)
                .unwrap_or_else(|_| Utc::now()),
            content_hash: 0,
            file_count:   0,
            total_size:   0,
            children:     Vec::new(),
            is_hidden:    false,
            is_dir:       true,
        };
        cache.entries.insert(scan_root.clone(), root_entry);
    }

    // ============================================================================
    // Check Cache Freshness (configurable via --cache-ttl, default 1 hour)
    // ============================================================================

    let cache_ttl_seconds = args.cache_ttl.unwrap_or(3600);

    let should_use_cache = if args.no_cache {
        false // --no-cache always triggers rescan
    } else if args.force {
        false // --force always triggers rescan
    } else if incremental_refresh {
        false // Incremental refresh must rescan affected directories immediately
    } else if is_first_run {
        false // First run always scans
    } else {
        // Check cache freshness rule (time-based only)
        let now = Utc::now();
        let age = now.signed_duration_since(cache.last_scan);
        if age.num_seconds() >= cache_ttl_seconds as i64 {
            false
        } else {
            cache_matches_live_state(cache, cache_path, &scan_root, &skip_dirs)?
        }
    };

    if should_use_cache {
        return Ok(DebugInfo {
            is_first_run:        false,
            incremental_refresh: false,
            scan_root:           cache.root.clone(),
            cache_used:          true,
            lazy_load_time:      Duration::ZERO,
            traversal_time:      Duration::from_secs(0),
            save_time:           Duration::from_secs(0),
            cache_index_time:    Duration::from_secs(0),
            total_dirs:          cache.entry_count_hint(),
            total_files:         cache.file_count_hint(),
            threads_used:        0,
        });
    }

    // ============================================================================
    // Initialize Traversal State
    // ============================================================================

    let mut work_queue = VecDeque::new();
    work_queue.push_back(scan_root.clone());

    let state = TraversalState {
        work_queue: Arc::new(Mutex::new(work_queue)),
        cache: Arc::new(RwLock::new(cache.clone())),
        in_progress: Arc::new(Mutex::new(std::collections::HashSet::new())),
        skip_dirs: skip_dirs.clone(),
        changed_dirs_filter,
        skip_stats: Arc::new(Mutex::new(std::collections::HashMap::new())),
    };

    // ============================================================================
    // Create Thread Pool & Determine Thread Count
    // ============================================================================

    let num_threads = args.threads.unwrap_or_else(|| {
        let cores = num_cpus::get().max(1);
        if args.force {
            cores
        } else {
            // Normal (non-force) scans are often small and lock-heavy.
            // Keep default worker count low to reduce contention.
            cores.min(4)
        }
    });

    let pool = rayon::ThreadPoolBuilder::new().num_threads(num_threads).build()?;

    // ============================================================================
    // Spawn Worker Threads for Parallel DFS Traversal
    // ============================================================================

    let traversal_start = Instant::now();
    let filter = state.changed_dirs_filter.clone();
    let root = scan_root.clone();
    let skip_stats_ref = Arc::clone(&state.skip_stats);
    pool.in_place_scope(|s| {
        for _ in 0..num_threads {
            let work = Arc::clone(&state.work_queue);
            let cache_ref = Arc::clone(&state.cache);
            let skip = state.skip_dirs.clone();
            let in_progress = Arc::clone(&state.in_progress);
            let filter_ref = filter.clone();
            let root_ref = root.clone();
            let stats_ref = Arc::clone(&skip_stats_ref);

            s.spawn(move |_| {
                dfs_worker(&work, &cache_ref, &skip, &in_progress, &filter_ref, &root_ref, &stats_ref);
            });
        }
    });
    let traversal_elapsed = traversal_start.elapsed();

    // ============================================================================
    // Extract & Save Final Cache
    // ============================================================================

    let mut final_cache = match Arc::try_unwrap(state.cache) {
        Ok(lock) => lock.into_inner(),
        Err(arc) => {
            let guard = arc.read();
            guard.clone()
        }
    };

    // Flush any remaining pending writes before saving
    final_cache.flush_pending_writes();
    final_cache.refresh_derived_metadata();

    let cache_index_start = Instant::now();

    *cache = final_cache;
    cache.last_scan = Utc::now();

    // Transfer skip statistics from traversal state to cache
    let skip_stats = match Arc::try_unwrap(state.skip_stats) {
        Ok(lock) => lock.into_inner().unwrap_or_default(),
        Err(arc) => {
            let guard = arc.lock().unwrap();
            guard.clone()
        }
    };
    cache.skip_stats = skip_stats;

    let cache_index_elapsed = cache_index_start.elapsed();

    let save_start = Instant::now();
    if !args.no_cache {
        cache.save(&cache_path)?;
    }
    let save_elapsed = save_start.elapsed();

    // ============================================================================
    // Return Debug Info
    // ============================================================================

    let total_files = cache
        .entries
        .get(&cache.root)
        .map(|entry| entry.file_count)
        .unwrap_or_else(|| cache.file_count_hint());

    Ok(DebugInfo {
        is_first_run,
        incremental_refresh,
        scan_root: cache.root.clone(),
        cache_used: false,
        lazy_load_time: Duration::ZERO,
        traversal_time: traversal_elapsed,
        save_time: save_elapsed,
        cache_index_time: cache_index_elapsed,
        total_dirs: cache.entries.len(),
        total_files,
        threads_used: num_threads,
    })
}

/// Worker thread for DFS traversal
///
/// Each worker thread:
/// 1. Pulls directories from shared work queue
/// 2. Acquires per-directory lock to prevent duplicate processing
/// 3. Enumerates directory, filters skipped entries
/// 4. For incremental updates: only process directories in changed_dirs_filter
/// 5. Buffers children in cache and queues directories for processing
fn dfs_worker(
    work_queue: &Arc<Mutex<VecDeque<PathBuf>>>,
    cache: &Arc<RwLock<DiskCache>>,
    skip_dirs: &std::collections::HashSet<String>,
    in_progress: &Arc<Mutex<std::collections::HashSet<PathBuf>>>,
    changed_dirs_filter: &Option<std::collections::HashSet<PathBuf>>,
    scan_root: &PathBuf,
    skip_stats: &Arc<Mutex<std::collections::HashMap<String, usize>>>,
) {
    // Thread-local buffers to batch cache writes and reduce lock contention
    let mut entry_buffer: Vec<(PathBuf, DirEntry)> = Vec::with_capacity(500);
    let mut skip_buffer: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let flush_threshold = 500;

    loop {
        // ====================================================================
        // Batch Work Stealing: Grab multiple directories at once (not just 1)
        // Reduces lock contention on work_queue significantly
        // ====================================================================

        let batch = {
            let mut queue = work_queue.lock().unwrap();
            let mut batch = Vec::new();
            for _ in 0..10 {
                // Grab up to 10 items in single lock
                if let Some(path) = queue.pop_front() {
                    batch.push(path);
                } else {
                    break;
                }
            }
            batch
        };

        if batch.is_empty() {
            // Flush remaining buffers before exiting
            if !entry_buffer.is_empty() {
                let mut cache_guard = cache.write();
                for (p, e) in entry_buffer.drain(..) {
                    cache_guard.add_entry(p, e);
                }
            }
            if !skip_buffer.is_empty() {
                let mut stats = skip_stats.lock().unwrap();
                for (name, count) in skip_buffer.drain() {
                    *stats.entry(name).or_insert(0) += count;
                }
            }
            break;
        }

        // Process batch of directories
        for path in batch {
            // ================================================================
            // Acquire Per-Directory Lock (prevents duplicate processing)
            // ================================================================

            let acquired = {
                let mut progress = in_progress.lock().unwrap();
                if !progress.contains(&path) {
                    progress.insert(path.clone());
                    true
                } else {
                    false
                }
            };

            if acquired {
                // ============================================================
                // Check Incremental Filter (if applicable)
                // ============================================================

                let should_process = if let Some(filter) = changed_dirs_filter {
                    // Incremental mode: only process directories in the exact affected path set
                    filter.contains(&path) || path == *scan_root
                } else {
                    // Full scan mode: process all directories
                    true
                };

                if should_process {
                    // ============================================================
                    // Enumerate Directory & Process Entries
                    // ============================================================

                    if let Ok(entries) = fs::read_dir(&path) {
                        let mut children = Vec::new();
                        let mut child_dirs_to_queue = Vec::new();
                        let mut skipped = Vec::new(); // Batch skipped directories
                        let mut direct_file_count = 0usize;
                        let mut direct_file_size = 0u64;

                        for entry_result in entries {
                            if let Ok(entry) = entry_result {
                                let file_name = entry.file_name();
                                let file_name_str = file_name.to_string_lossy();

                                // Skip filtered directories
                                if should_skip(&file_name_str, skip_dirs) {
                                    // Batch skip statistics (don't lock on every skip)
                                    skipped.push(file_name_str.to_string());
                                    continue;
                                }

                                let child_path = entry.path();
                                children.push(file_name_str.to_string());

                                // Check if this is a directory (avoid unnecessary metadata calls for files)
                                match entry.file_type() {
                                    Ok(ft) if ft.is_dir() => {
                                        // Queue directories for processing
                                        let should_queue = changed_dirs_filter
                                            .as_ref()
                                            .map(|filter| filter.contains(&child_path))
                                            .unwrap_or(true);
                                        if should_queue {
                                            child_dirs_to_queue.push(child_path.clone());
                                        }
                                    }
                                    Ok(ft) if ft.is_symlink() => {
                                        // Symlinks are recorded as names only; we don't traverse them.
                                        direct_file_count += 1;
                                    }
                                    Ok(_) => {
                                        // Regular file: recorded in `children`; no cache insert needed.
                                        direct_file_count += 1;
                                        if let Ok(metadata) = entry.metadata() {
                                            direct_file_size += metadata.len();
                                        }
                                    }
                                    _ => {} // Couldn't get file type, skip
                                }
                            }
                        }

                        // ========================================================
                        // Batch queue directories (reduce lock contention)
                        // ========================================================
                        if !child_dirs_to_queue.is_empty() {
                            let mut queue = work_queue.lock().unwrap();
                            for dir_path in child_dirs_to_queue {
                                queue.push_back(dir_path);
                            }
                        }

                        // ========================================================
                        // Buffer file entries (thread-local, flush periodically)
                        // (directory entries only; file names live inside `children`)
                        // ========================================================

                        // Buffer skip statistics (thread-local, flush on exit)
                        // ========================================================
                        for skip_name in skipped {
                            *skip_buffer.entry(skip_name).or_insert(0) += 1;
                        }

                        // ========================================================
                        // Skip sorting during traversal (defer to output phase)
                        // Children list stored unsorted for now
                        // ========================================================

                        // Check if directory has hidden attribute (Windows only)
                        let is_hidden = {
                            #[cfg(windows)]
                            {
                                use std::os::windows::fs::MetadataExt;
                                fs::metadata(&path)
                                    .map(|m| {
                                        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x02;
                                        (m.file_attributes() & FILE_ATTRIBUTE_HIDDEN) != 0
                                    })
                                    .unwrap_or(false)
                            }
                            #[cfg(not(windows))]
                            {
                                // Unix-like: check if name starts with dot
                                path.file_name()
                                    .and_then(|n| n.to_str())
                                    .map(|s| s.starts_with('.'))
                                    .unwrap_or(false)
                            }
                        };

                        let mut cache_guard = cache.write();
                        cache_guard.remove_missing_child_subtrees(&path, &children);
                        drop(cache_guard);

                        let dir_entry = DirEntry {
                            path: path.clone(),
                            name: path
                                .file_name()
                                .and_then(|n| n.to_str().map(|s| s.to_string()))
                                .unwrap_or_default(),
                            modified: fs::metadata(&path)
                                .and_then(|metadata| metadata.modified())
                                .map(system_time_to_utc)
                                .unwrap_or_else(|_| Utc::now()),
                            content_hash: 0,
                            file_count: direct_file_count,
                            total_size: direct_file_size,
                            children,
                            is_hidden,
                            is_dir: true,
                        };

                        // ========================================================
                        // Buffer directory entry (thread-local, flush periodically)
                        // Minimizes cache.write() lock acquisitions
                        // ========================================================
                        entry_buffer.push((path.clone(), dir_entry));

                        if entry_buffer.len() >= flush_threshold {
                            let mut cache_guard = cache.write();
                            for (p, e) in entry_buffer.drain(..) {
                                cache_guard.add_entry(p, e);
                            }
                        }
                    }

                    // ============================================================
                    // Release Per-Directory Lock
                    // ============================================================

                    {
                        let mut progress = in_progress.lock().unwrap();
                        progress.remove(&path);
                    }
                } else {
                    // Directory filtered out (incremental mode): skip it
                    {
                        let mut progress = in_progress.lock().unwrap();
                        progress.remove(&path);
                    }
                }
            }
        }
    }
}

fn should_skip(name: &str, skip_dirs: &std::collections::HashSet<String>) -> bool {
    skip_dirs.iter().any(|skip| name.eq_ignore_ascii_case(skip))
}

fn cache_matches_live_state(
    cache: &mut DiskCache,
    cache_path: &Path,
    scan_root: &Path,
    skip_dirs: &std::collections::HashSet<String>,
) -> Result<bool> {
    if !cache.entries.contains_key(scan_root) {
        cache.load_entries_lazy(&[scan_root.to_path_buf()], cache_path)?;
    }

    let Some(root_entry) = cache.get_entry(scan_root) else {
        return Ok(false);
    };

    let live = summarize_live_directory(scan_root, skip_dirs)?;
    Ok(root_entry.content_hash == live.content_hash
        && root_entry.file_count == live.file_count
        && root_entry.total_size == live.total_size)
}

fn summarize_live_directory(
    path: &Path,
    skip_dirs: &std::collections::HashSet<String>,
) -> Result<LiveDirectorySummary> {
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map(system_time_to_utc)
        .unwrap_or_else(|_| Utc::now());

    let mut children = Vec::new();
    let mut child_hashes = std::collections::HashMap::new();
    let mut file_count = 0usize;
    let mut total_size = 0u64;

    for entry_result in fs::read_dir(path)? {
        let entry = entry_result?;
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip(&name, skip_dirs) {
            continue;
        }

        children.push(name.clone());
        let child_path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {
                let child = summarize_live_directory(&child_path, skip_dirs)?;
                file_count += child.file_count;
                total_size += child.total_size;
                child_hashes.insert(child_path, child.content_hash);
            }
            Ok(ft) if ft.is_symlink() => {
                file_count += 1;
            }
            Ok(_) => {
                file_count += 1;
                if let Ok(metadata) = entry.metadata() {
                    total_size += metadata.len();
                }
            }
            Err(_) => {}
        }
    }

    let content_hash = compute_content_hash(path, modified, &children, &child_hashes);
    Ok(LiveDirectorySummary {
        content_hash,
        file_count,
        total_size,
    })
}

/// Expand leading '~' into the user's home directory. If expansion fails,
/// returns the original path.
fn expand_tilde(path: &PathBuf) -> Result<PathBuf> {
    use std::env;

    if let Some(raw) = path.to_str() {
        if raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\") {
            let home = {
                #[cfg(windows)]
                {
                    env::var("USERPROFILE").or_else(|_| {
                        let drive = env::var("HOMEDRIVE")?;
                        let path = env::var("HOMEPATH")?;
                        Ok(format!("{}{}", drive, path))
                    })
                }
                #[cfg(not(windows))]
                {
                    env::var("HOME")
                }
            };

            if let Ok(home_dir) = home {
                let mut expanded = PathBuf::from(home_dir);
                if raw.len() > 1 {
                    expanded.push(&raw[2..]); // strip "~/"
                }
                return Ok(expanded);
            }
        }
    }

    Ok(path.clone())
}

fn resolve_scan_root(drive: &char, args: &Args) -> Result<PathBuf> {
    #[cfg(not(windows))]
    let _ = drive;

    // Determine scan root precedence:
    // 1) Explicit path argument (supports ~ expansion)
    // 2) --force => full filesystem root
    // 3) Default => current working directory
    if let Some(p) = &args.path {
        expand_tilde(p)
    } else if args.force {
        #[cfg(windows)]
        {
            let root = PathBuf::from(format!("{}:\\", drive));
            if !root.exists() {
                anyhow::bail!("Drive {} does not exist", drive);
            }
            Ok(root)
        }

        #[cfg(not(windows))]
        {
            Ok(PathBuf::from("/"))
        }
    } else {
        Ok(std::env::current_dir()?)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use ptree_core::{ColorMode, OutputFormat};
    use ptree_incremental::IncrementalChange;

    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("ptree_traversal_{name}_{unique}"))
    }

    fn test_args(path: PathBuf) -> Args {
        Args {
            path:                Some(path),
            drive:               'C',
            admin:               false,
            force:               false,
            cache_ttl:           None,
            cache_dir:           None,
            no_cache:            true,
            quiet:               true,
            format:              OutputFormat::Tree,
            color:               ColorMode::Never,
            size:                false,
            file_count:          false,
            max_depth:           None,
            skip:                None,
            hidden:              false,
            threads:             Some(1),
            stats:               false,
            skip_stats:          false,
            scheduler:           false,
            scheduler_uninstall: false,
            scheduler_status:    false,
        }
    }

    #[test]
    fn test_should_skip() {
        let mut skip = std::collections::HashSet::new();
        skip.insert("System32".to_string());
        skip.insert(".git".to_string());

        assert!(should_skip("System32", &skip));
        assert!(should_skip(".git", &skip));
        assert!(!should_skip("Documents", &skip));
    }

    #[test]
    fn incremental_refresh_targets_full_paths_and_prunes_stale_subtrees() -> Result<()> {
        let root = test_root("incremental_filter");
        let left_shared = root.join("left").join("shared").join("old_left");
        let right_shared = root.join("right").join("shared").join("old_right");
        fs::create_dir_all(&left_shared)?;
        fs::create_dir_all(&right_shared)?;

        let args = test_args(root.clone());
        let cache_path = root.join("cache").join("ptree.dat");
        let mut cache = DiskCache::open(&cache_path)?;

        traverse_disk(&'C', &mut cache, &args, &cache_path)?;
        assert!(cache
            .entries
            .contains_key(&root.join("left").join("shared").join("old_left")));
        assert!(cache
            .entries
            .contains_key(&root.join("right").join("shared").join("old_right")));

        fs::remove_dir_all(root.join("left").join("shared").join("old_left"))?;
        fs::create_dir_all(root.join("left").join("shared").join("fresh_left"))?;
        fs::remove_dir_all(root.join("right").join("shared").join("old_right"))?;
        fs::create_dir_all(root.join("right").join("shared").join("fresh_right"))?;

        let changes = vec![
            IncrementalChange::deleted(root.join("left").join("shared").join("old_left"), true),
            IncrementalChange::created(root.join("left").join("shared").join("fresh_left"), true),
        ];

        let debug = traverse_disk_incremental(&'C', &mut cache, &args, &cache_path, &changes)?;

        assert!(debug.incremental_refresh);
        assert!(cache
            .entries
            .contains_key(&root.join("left").join("shared").join("fresh_left")));
        assert!(!cache
            .entries
            .contains_key(&root.join("left").join("shared").join("old_left")));
        assert!(cache
            .entries
            .contains_key(&root.join("right").join("shared").join("old_right")));
        assert!(!cache
            .entries
            .contains_key(&root.join("right").join("shared").join("fresh_right")));

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn warm_cache_revalidates_live_state_before_reuse() -> Result<()> {
        let root = test_root("warm_cache_validation");
        let nested = root.join("alpha");
        fs::create_dir_all(&nested)?;
        fs::write(nested.join("leaf.txt"), b"one")?;

        let mut args = test_args(root.clone());
        args.no_cache = false;
        args.cache_ttl = Some(3600);
        let cache_path = std::env::temp_dir().join("ptree_test_cache_validation").join(format!(
            "ptree-{}.dat",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut cache = DiskCache::open(&cache_path)?;

        let first = traverse_disk(&'C', &mut cache, &args, &cache_path)?;
        assert!(!first.cache_used);

        let warm = traverse_disk(&'C', &mut cache, &args, &cache_path)?;
        assert!(warm.cache_used);

        fs::write(nested.join("leaf.txt"), b"updated-and-larger")?;

        let invalidated = traverse_disk(&'C', &mut cache, &args, &cache_path)?;
        assert!(!invalidated.cache_used);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(cache_path.parent().unwrap_or(&cache_path));
        Ok(())
    }
}
