use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use anyhow::{Result, anyhow};
use memmap2::Mmap;
use rkyv::Deserialize as RkyvDeserialize;

use crate::cache::DirEntry;

/// Compute depth of a path (number of separators)
fn compute_depth(path: &Path) -> u32 {
    path.components().count() as u32
}

/// Lightweight index mapping path offsets to byte positions in depth-split data files
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(target_arch = "x86_64", derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize))]
pub struct CacheIndex {
     /// Sorted Vec of (PathBuf, depth, offset) for O(log n) lookup across split files
     pub offsets: Vec<(PathBuf, u32, u64)>,
    
    /// Last scan timestamp
    pub last_scan: DateTime<Utc>,
    
    /// Root path
    pub root: PathBuf,
    
    /// Last scanned directory
    pub last_scanned_root: PathBuf,
    
    /// USN Journal state (Windows only)
    #[cfg(windows)]
    pub usn_state: USNJournalState,
    
    /// Skip statistics
    pub skip_stats: HashMap<String, usize>,
}

impl CacheIndex {
     pub fn new() -> Self {
         CacheIndex {
             offsets: Vec::new(),
             last_scan: Utc::now(),
             root: PathBuf::new(),
             last_scanned_root: PathBuf::new(),
            #[cfg(windows)]
            usn_state: USNJournalState::default(),
            skip_stats: HashMap::new(),
        }
    }
}

/// Memory-mapped cache system
/// 
/// Structure:
/// - index file: contains CacheIndex (paths → offsets)
/// - data file: contains serialized DirEntry objects at indexed offsets
pub struct MmapCache {
     /// Index mapping paths to (depth, offsets)
     pub index: CacheIndex,
     
     /// Memory-mapped data files, indexed by depth (0-30)
     mmaps: Vec<Option<Mmap>>,
     
     /// Base path for data files
     base_path: PathBuf,
     
     /// Buffer for pending writes before flush
     pub pending_writes: Vec<(PathBuf, DirEntry)>,
     
     /// Flush threshold
     pub flush_threshold: usize,
}

impl MmapCache {
     /// Load cache from index and data files (depth-split strategy)
     /// Loads index from index_path, and mmaps all depth-split data files
     pub fn open(index_path: &Path, data_path: &Path) -> Result<Self> {
          fs::create_dir_all(index_path.parent().unwrap())?;
          
          // Load index via rkyv from mmap (zero-copy)
          let index = if index_path.exists() {
              match File::open(index_path) {
                  Ok(file) => {
                      match unsafe { Mmap::map(&file) } {
                          Ok(index_mmap) => {
                              // Deserialize from mmap with rkyv (zero-copy)
                              match rkyv::from_bytes::<CacheIndex>(&index_mmap) {
                                  Ok(idx) => idx,
                                  Err(_) => {
                                      // Fallback: read into memory for bincode
                                      if let Ok(data) = std::fs::read(index_path) {
                                          bincode::deserialize(&data).unwrap_or_else(|_| CacheIndex::new())
                                      } else {
                                          CacheIndex::new()
                                      }
                                  }
                              }
                          }
                          Err(_) => CacheIndex::new(),
                      }
                  }
                  Err(_) => CacheIndex::new(),
              }
          } else {
              CacheIndex::new()
          };
          
          // Load depth-split data files (ptree-d0.dat, ptree-d1.dat, etc.)
          // Support up to depth 30 (typical filesystem is 5-10 levels deep)
          let mut mmaps = Vec::with_capacity(31);
          for depth in 0..31 {
              let depth_file = Self::depth_file_path(data_path, depth);
              let mmap = if depth_file.exists() {
                  match File::open(&depth_file) {
                      Ok(file) => match unsafe { Mmap::map(&file) } {
                          Ok(m) => Some(m),
                          Err(_) => None,
                      },
                      Err(_) => None,
                  }
              } else {
                  None
              };
              mmaps.push(mmap);
          }
          
          Ok(MmapCache {
              index,
              mmaps,
              base_path: data_path.to_path_buf(),
              pending_writes: Vec::new(),
              flush_threshold: 5000,
          })
      }
      
      /// Generate depth-split data file path
      fn depth_file_path(base_path: &Path, depth: u32) -> PathBuf {
          let stem = base_path.file_stem().and_then(|s| s.to_str()).unwrap_or("ptree");
          let parent = base_path.parent().unwrap_or_else(|| Path::new("."));
          parent.join(format!("{}-d{}.dat", stem, depth))
      }
    
    /// Get a directory entry by path (deserializes from depth-specific mmap'd region)
    pub fn get(&self, path: &Path) -> Result<Option<DirEntry>> {
        // Binary search to find (path, depth, offset) in sorted index
        let (depth, offset) = match self.index.offsets.binary_search_by_key(&path, |(p, _, _)| p) {
            Ok(idx) => {
                let (_, d, o) = &self.index.offsets[idx];
                (*d, *o)
            },
            Err(_) => return Ok(None),
        };
        
        // Get mmap for this depth (0-30)
        if depth >= 31 {
            return Err(anyhow!("Path depth exceeds maximum of 30"));
        }
        let mmap = self.mmaps[depth as usize]
            .as_ref()
            .ok_or_else(|| anyhow!("No mmap loaded for depth {}", depth))?;
        
        let data_slice = &mmap[offset as usize..];
        
        // Deserialize single entry from this offset
        // Format: [4-byte length][serialized entry]
        if data_slice.len() < 4 {
            return Err(anyhow!("Invalid cache entry"));
        }
        
        let len = u32::from_le_bytes([
            data_slice[0],
            data_slice[1],
            data_slice[2],
            data_slice[3],
        ]) as usize;
        
        if data_slice.len() < 4 + len {
            return Err(anyhow!("Truncated cache entry"));
        }
        
        let entry: DirEntry = bincode::deserialize(&data_slice[4..4 + len])?;
        Ok(Some(entry))
    }
    
    /// Get all entries (loads entire mmap into memory - only for output generation)
    pub fn get_all(&self) -> Result<HashMap<PathBuf, DirEntry>> {
        let mut entries = HashMap::new();
        
        for path in self.index.offsets.keys() {
            if let Some(entry) = self.get(path)? {
                entries.insert(path.clone(), entry);
            }
        }
        
        Ok(entries)
    }
    
    /// Add a pending write
    pub fn add_entry(&mut self, path: PathBuf, entry: DirEntry) {
        self.pending_writes.push((path, entry));
        if self.pending_writes.len() >= self.flush_threshold {
            let _ = self.flush_pending_writes();
        }
    }
    
    /// Flush pending writes to disk (depth-split files)
    pub fn flush_pending_writes(&mut self) -> Result<()> {
        if self.pending_writes.is_empty() {
            return Ok(());
        }
        
        // Group writes by depth to minimize file handle juggling
        let mut writes_by_depth: std::collections::HashMap<u32, Vec<(PathBuf, DirEntry)>> = 
            std::collections::HashMap::new();
        
        for (path, entry) in self.pending_writes.drain(..) {
            let depth = compute_depth(&path);
            writes_by_depth.entry(depth).or_insert_with(Vec::new).push((path, entry));
        }
        
        // Write each depth's entries to its depth-specific file
        for (depth, entries) in writes_by_depth {
            if depth >= 31 {
                anyhow::bail!("Path depth {} exceeds maximum of 30", depth);
            }
            
            let depth_file = Self::depth_file_path(&self.base_path, depth);
            let mut data_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&depth_file)?;
            
            for (path, entry) in entries {
                let serialized = bincode::serialize(&entry)?;
                let len = serialized.len() as u32;
                
                // Record offset before writing
                let offset = data_file.seek(SeekFrom::End(0))?;
                self.index.offsets.push((path.to_path_buf(), depth, offset));
                
                // Write length + data
                data_file.write_all(&len.to_le_bytes())?;
                data_file.write_all(&serialized)?;
            }
            
            data_file.sync_all()?;
            
            // Reload mmap for this depth to include new data
            if let Ok(file) = File::open(&depth_file) {
                if let Ok(mmap) = unsafe { Mmap::map(&file) } {
                    self.mmaps[depth as usize] = Some(mmap);
                }
            }
        }
        
        Ok(())
    }
    
    /// Save index to disk using rkyv (fast serialization)
    /// Sorts offsets by path for binary search (depth is secondary sort key for stability)
    pub fn save_index(&mut self, path: &Path) -> Result<()> {
        // Sort offsets by path, then by depth for binary search compatibility
        // Binary search only uses path, but depth ordering ensures deterministic output
        self.index.offsets.sort_by(|a, b| {
            match a.0.cmp(&b.0) {
                std::cmp::Ordering::Equal => a.1.cmp(&b.1),
                other => other,
            }
        });
        
        // Use rkyv for fast zero-copy serialization
        let bytes = rkyv::to_bytes::<_, 256>(&self.index)
            .map_err(|e| anyhow!("rkyv serialization failed: {}", e))?;
        
        let temp_path = path.with_extension("tmp");
        let mut file = File::create(&temp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        
        fs::rename(&temp_path, path)?;
        Ok(())
    }
    
    /// Get number of cached entries
    pub fn len(&self) -> usize {
        self.index.offsets.len()
    }
    
    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.index.offsets.is_empty()
    }
}
