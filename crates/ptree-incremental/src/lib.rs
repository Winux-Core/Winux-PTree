pub mod incremental;

pub use incremental::{build_changed_directory_set, try_incremental_update, IncrementalChange, IncrementalChangeKind};
