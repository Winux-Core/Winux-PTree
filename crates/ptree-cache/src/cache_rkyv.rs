use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};

#[cfg(windows)]
use crate::cache::USNJournalState;

/// Compute depth of a path (number of separators)
fn compute_depth(path: &Path) -> u32 {
    path.components().count() as u32
}

/// Serializable directory entry (serde-based for compatibility)
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RkyvDirEntry {
    pub path:         PathBuf,
    pub name:         String,
    pub modified:     DateTime<Utc>,
    pub content_hash: u64, // NEW FIELD - Merkle tree hash
    pub file_count:   usize,
    pub total_size:   u64,
    pub children:     Vec<String>,
    pub is_hidden:    bool,
    pub is_dir:       bool,
}

/// Serializable cache index (serde-based for compatibility)
/// Maps paths → (depth, offset) for depth-split file access
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RkyvCacheIndex {
    /// Offsets mapping: (path, depth, offset) for lazy depth-aware access
    pub offsets:           HashMap<PathBuf, (u32, u64)>,
    pub total_files:       usize,
    pub last_scan:         DateTime<Utc>,
    pub root:              PathBuf,
    pub last_scanned_root: PathBuf,
    #[cfg(windows)]
    pub usn_state:         USNJournalState,
    pub skip_stats:        HashMap<String, usize>,
}

impl RkyvCacheIndex {
    pub fn new() -> Self {
        RkyvCacheIndex {
            offsets:                   HashMap::new(),
            total_files:               0,
            last_scan:                 Utc::now(),
            root:                      PathBuf::new(),
            last_scanned_root:         PathBuf::new(),
            #[cfg(windows)]
            usn_state:                 USNJournalState::default(),
            skip_stats:                HashMap::new(),
        }
    }
}

/// Memory-mapped cache using rkyv for zero-copy single-node O(1) access
///
/// Architecture (depth-split strategy):
/// - index file (.idx): contains RkyvCacheIndex with (depth, offset) tuples
/// - data files (ptree-d0.dat, ptree-d1.dat, etc.): split by directory depth
///
/// Single-node access is O(1): load (depth, offset) from index, access depth-specific mmap
/// No allocation or copying for field access during traversal
pub struct RkyvMmapCache {
    pub index: RkyvCacheIndex,
    mmaps:     Vec<Option<Mmap>>,
    base_path: PathBuf,
}

impl RkyvMmapCache {
    /// Load cache from index and depth-split data files
    /// Index is fully deserialized (small), data is mmap'd (large, lazy access)
    pub fn open(index_path: &std::path::Path, data_path: &std::path::Path) -> Result<Self> {
        fs::create_dir_all(index_path.parent().unwrap())?;

        // Load index (small, safe to fully deserialize using serde)
        let index = if index_path.exists() {
            let mut file = File::open(index_path)?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;

            // Deserialize index using serde bincode
            bincode::deserialize::<RkyvCacheIndex>(&data)
                .map_err(|e| anyhow::anyhow!("failed to deserialize cache index: {e}"))?
        } else {
            RkyvCacheIndex::new()
        };

        // Load depth-split data files (ptree-d0.dat, ptree-d1.dat, etc.)
        // Support up to depth 30 (typical filesystem is 5-10 levels deep)
        let mut mmaps = Vec::with_capacity(31);
        for depth in 0..31 {
            let depth_file = Self::depth_file_path(data_path, depth);
            let mmap = if depth_file.exists() {
                match File::open(&depth_file) {
                    Ok(file) => {
                        match unsafe { Mmap::map(&file) } {
                            Ok(m) => Some(m),
                            Err(_) => None,
                        }
                    }
                    Err(_) => None,
                }
            } else {
                None
            };
            mmaps.push(mmap);
        }

        Self::validate_index_offsets(&index, &mmaps, data_path)?;

        Ok(RkyvMmapCache {
            index,
            mmaps,
            base_path: data_path.to_path_buf(),
        })
    }

    /// Generate depth-split data file path
    fn depth_file_path(base_path: &Path, depth: u32) -> PathBuf {
        let stem = base_path.file_stem().and_then(|s| s.to_str()).unwrap_or("ptree");
        let parent = base_path.parent().unwrap_or_else(|| Path::new("."));
        parent.join(format!("{}-d{}.dat", stem, depth))
    }

    fn validate_index_offsets(index: &RkyvCacheIndex, mmaps: &[Option<Mmap>], data_path: &Path) -> Result<()> {
        for (path, (depth, offset)) in &index.offsets {
            if *depth >= 31 {
                anyhow::bail!("indexed depth {} for {} exceeds supported maximum", depth, path.display());
            }

            let Some(mmap) = mmaps[*depth as usize].as_ref() else {
                anyhow::bail!(
                    "missing cache shard {} for indexed path {}",
                    Self::depth_file_path(data_path, *depth).display(),
                    path.display()
                );
            };

            let offset = *offset as usize;
            if offset + 4 > mmap.len() {
                anyhow::bail!("offset out of bounds for {}", path.display());
            }

            let len = u32::from_le_bytes([mmap[offset], mmap[offset + 1], mmap[offset + 2], mmap[offset + 3]]) as usize;

            if offset + 4 + len > mmap.len() {
                anyhow::bail!("truncated cache record for {}", path.display());
            }
        }

        Ok(())
    }

    /// O(1) lookup: get single directory entry via depth-specific mmap offset
    /// Deserializes from depth-split mmap'd region
    pub fn get_entry(&self, path: &std::path::Path) -> Result<Option<RkyvDirEntry>> {
        let (depth, offset) = match self.index.offsets.get(path) {
            Some((d, o)) => (*d, *o),
            None => return Ok(None),
        };

        // Get mmap for this depth (0-30)
        if depth >= 31 {
            return Err(anyhow::anyhow!("Path depth {} exceeds maximum of 30", depth));
        }
        let mmap = self.mmaps[depth as usize]
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No mmap loaded for depth {}", depth))?;

        let data_slice = &mmap[offset as usize..];

        // Read length prefix
        if data_slice.len() < 4 {
            return Ok(None);
        }

        let len = u32::from_le_bytes([data_slice[0], data_slice[1], data_slice[2], data_slice[3]]) as usize;

        if data_slice.len() < 4 + len {
            return Ok(None);
        }

        // Deserialize entry from mmap'd region
        let entry: RkyvDirEntry = bincode::deserialize(&data_slice[4..4 + len])?;
        Ok(Some(entry))
    }

    /// Get all entries (full deserialization - only for batch operations or output)
    /// Used for tree building where we need owned data
    pub fn get_all(&self) -> Result<HashMap<PathBuf, crate::cache::DirEntry>> {
        let mut entries = HashMap::new();

        for path in self.index.offsets.keys() {
            if let Some(entry) = self.get_entry(path)? {
                entries.insert(
                    entry.path.clone(),
                    crate::cache::DirEntry {
                        path:         entry.path,
                        name:         entry.name,
                        modified:     entry.modified,
                        content_hash: entry.content_hash,
                        file_count:   entry.file_count,
                        total_size:   entry.total_size,
                        children:     entry.children,
                        is_hidden:    entry.is_hidden,
                        is_dir:       entry.is_dir,
                    },
                );
            }
        }

        Ok(entries)
    }

    /// Add entry to index and append to depth-split data file
    /// Returns offset for bookkeeping
    pub fn append_entry(&mut self, entry: &RkyvDirEntry) -> Result<(u32, u64)> {
        let depth = compute_depth(&entry.path);
        if depth >= 31 {
            anyhow::bail!("Path depth {} exceeds maximum of 30", depth);
        }

        let depth_file = Self::depth_file_path(&self.base_path, depth);
        let mut data_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&depth_file)?;

        let serialized = bincode::serialize(entry)?;
        let len = serialized.len() as u32;

        let offset = data_file.seek(SeekFrom::End(0))?;

        data_file.write_all(&len.to_le_bytes())?;
        data_file.write_all(&serialized)?;
        data_file.sync_all()?;

        // Update index with (depth, offset)
        self.index.offsets.insert(entry.path.clone(), (depth, offset));

        Ok((depth, offset))
    }

    /// Save index to disk (bincode serialized)
    pub fn save_index(&self, path: &std::path::Path) -> Result<()> {
        let data = bincode::serialize(&self.index)?;
        let temp_path = path.with_extension("tmp");

        let mut file = File::create(&temp_path)?;
        file.write_all(&data)?;
        file.sync_all()?;

        fs::rename(&temp_path, path)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.index.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.offsets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    #[test]
    fn test_rkyv_dir_entry_serialization() -> Result<()> {
        let entry = RkyvDirEntry {
            path:         PathBuf::from("C:\\test"),
            name:         "test".to_string(),
            modified:     Utc::now(),
            content_hash: 12345u64,
            file_count:   2,
            total_size:   4096,
            children:     vec!["child1".to_string(), "child2".to_string()],
            is_hidden:    false,
            is_dir:       true,
        };

        let serialized = bincode::serialize(&entry)?;
        let deserialized: RkyvDirEntry = bincode::deserialize(&serialized)?;

        assert_eq!(entry.name, deserialized.name);
        assert_eq!(entry.content_hash, deserialized.content_hash);
        assert_eq!(entry.children.len(), deserialized.children.len());

        Ok(())
    }

    #[test]
    fn test_rkyv_cache_open() -> Result<()> {
        let temp_dir = env::temp_dir().join("ptree_rkyv_test");
        fs::create_dir_all(&temp_dir)?;
        let index_path = temp_dir.join("test.idx");
        let data_path = temp_dir.join("test.dat");

        let _cache = RkyvMmapCache::open(&index_path, &data_path)?;
        assert!(_cache.is_empty());

        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }
}
