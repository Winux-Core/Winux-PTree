// Windows service implementation for ptree-driver
// Runs as a system service monitoring file system changes via USN Journal

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(windows)]
use std::time::Duration;
use std::time::Instant;

#[cfg(windows)]
use log::{debug, error, info};
#[cfg(windows)]
use ptree_cache::DiskCache;
#[cfg(windows)]
use ptree_core::{Args, ColorMode, OutputFormat};
#[cfg(windows)]
use ptree_incremental::IncrementalChange;
#[cfg(windows)]
use ptree_traversal::traverse_disk_incremental;

use crate::error::DriverResult;
#[cfg(windows)]
use crate::usn_journal::{ChangeType, USNTracker, UsnRecord};

/// Service configuration
pub struct ServiceConfig {
    /// Drive letter to monitor (e.g., 'C')
    pub drive_letter: char,

    /// Interval between journal checks (seconds)
    pub check_interval: u64,

    /// Cache file path
    pub cache_path: std::path::PathBuf,

    /// Log file path
    pub log_path: std::path::PathBuf,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        ServiceConfig {
            drive_letter:   'C',
            check_interval: 60,
            cache_path:     std::path::PathBuf::from(
                std::env::var("APPDATA").unwrap_or_else(|_| "C:\\Users\\User\\AppData\\Roaming".to_string()),
            )
            .join("ptree")
            .join("cache")
            .join("ptree.dat"),
            log_path:       std::path::PathBuf::from("C:\\ProgramData\\ptree").join("service.log"),
        }
    }
}

/// Service state
pub struct PtreeService {
    config:          ServiceConfig,
    pub should_exit: Arc<AtomicBool>,
    last_update:     Instant,
}

impl PtreeService {
    /// Create a new service instance
    pub fn new(config: ServiceConfig) -> Self {
        PtreeService {
            config,
            should_exit: Arc::new(AtomicBool::new(false)),
            last_update: Instant::now(),
        }
    }

    /// Main service loop - runs continuously
    pub fn run(&mut self) -> DriverResult<()> {
        #[cfg(not(windows))]
        {
            return Err(crate::error::DriverError::Windows(
                "ptree-driver service is only available on Windows".to_string(),
            ));
        }

        #[cfg(windows)]
        {
            info!("ptree-driver service starting");
            info!("Monitoring drive: {}", self.config.drive_letter);
            info!("Check interval: {} seconds", self.config.check_interval);

            // Create tracker for the specified drive
            let mut tracker = USNTracker::new(self.config.drive_letter, Default::default());

            // Check if journal is available
            if !tracker.is_available()? {
                error!("USN Journal not available on drive {}. Service cannot start.", self.config.drive_letter);
                return Err(crate::error::DriverError::JournalNotFound(
                    "Service requires NTFS volume with active USN Journal".to_string(),
                ));
            }

            info!("USN Journal is active. Starting monitoring loop.");

            let check_interval = Duration::from_secs(self.config.check_interval);

            // Main service loop
            while !self.should_exit.load(Ordering::Relaxed) {
                let loop_start = Instant::now();

                // Read changes from journal
                match tracker.read_changes() {
                    Ok(changes) => {
                        if !changes.is_empty() {
                            info!("Detected {} changes", changes.len());

                            // Apply changes to cache
                            if let Err(e) = self.apply_changes(&changes) {
                                error!("Failed to apply changes to cache: {}", e);
                            } else {
                                debug!("Successfully updated cache with {} changes", changes.len());
                                self.last_update = Instant::now();
                            }
                        } else {
                            debug!("No changes detected");
                        }
                    }
                    Err(e) => {
                        error!("Failed to read journal: {}", e);

                        // Check if journal is still valid
                        if let Err(validity_err) = tracker.check_journal_validity() {
                            error!("Journal validity check failed: {}", validity_err);
                            error!("Service will retry in next cycle");
                        }
                    }
                }

                // Sleep until next check
                let elapsed = loop_start.elapsed();
                if elapsed < check_interval {
                    let sleep_duration = check_interval - elapsed;
                    std::thread::sleep(sleep_duration);
                }
            }

            info!("ptree-driver service stopping");
            Ok(())
        }
    }

    /// Signal the service to stop
    pub fn stop(&self) {
        self.should_exit.store(true, Ordering::Relaxed);
    }

    /// Apply changes to the ptree cache
    #[cfg(windows)]
    fn apply_changes(&self, changes: &[UsnRecord]) -> DriverResult<()> {
        let mut creates = 0;
        let mut modifies = 0;
        let mut deletes = 0;
        let mut incremental_changes = Vec::new();

        for record in changes {
            match record.change_type {
                ChangeType::Created => creates += 1,
                ChangeType::Modified => modifies += 1,
                ChangeType::Deleted => deletes += 1,
                _ => {}
            }

            incremental_changes.push(match record.change_type {
                ChangeType::Created => IncrementalChange::created(record.path.clone(), record.is_directory),
                ChangeType::Deleted => IncrementalChange::deleted(record.path.clone(), record.is_directory),
                ChangeType::Renamed => IncrementalChange::renamed(record.path.clone(), record.is_directory),
                _ => IncrementalChange::modified(record.path.clone(), record.is_directory),
            });
        }

        if incremental_changes.is_empty() {
            return Ok(());
        }

        let scan_root = std::path::PathBuf::from(format!("{}:\\", self.config.drive_letter));
        let args = Args {
            path:                Some(scan_root.clone()),
            drive:               self.config.drive_letter,
            admin:               true,
            force:               false,
            cache_ttl:           Some(3600),
            cache_dir:           self
                .config
                .cache_path
                .parent()
                .map(|path| path.to_string_lossy().to_string()),
            no_cache:            false,
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
        };

        let mut cache =
            DiskCache::open(&self.config.cache_path).map_err(|e| crate::error::DriverError::Windows(e.to_string()))?;
        traverse_disk_incremental(
            &self.config.drive_letter,
            &mut cache,
            &args,
            &self.config.cache_path,
            &incremental_changes,
        )
        .map_err(|e| crate::error::DriverError::Windows(e.to_string()))?;

        debug!("Changes: {} created, {} modified, {} deleted", creates, modifies, deletes);

        Ok(())
    }

    /// Get service status
    pub fn status(&self) -> ServiceStatus {
        ServiceStatus {
            is_running:  !self.should_exit.load(Ordering::Relaxed),
            last_update: self.last_update,
            drive:       self.config.drive_letter,
            cache_path:  self.config.cache_path.clone(),
        }
    }
}

/// Service status information
pub struct ServiceStatus {
    pub is_running:  bool,
    pub last_update: Instant,
    pub drive:       char,
    pub cache_path:  std::path::PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_creation() {
        let config = ServiceConfig::default();
        let service = PtreeService::new(config);
        assert_eq!(service.config.drive_letter, 'C');
    }

    #[test]
    fn test_service_stop_signal() {
        let config = ServiceConfig::default();
        let service = PtreeService::new(config);
        assert!(!service.should_exit.load(Ordering::Relaxed));
        service.stop();
        assert!(service.should_exit.load(Ordering::Relaxed));
    }
}
