// Platform-specific scheduler implementations are split into OS-targeted crates
// to keep dependencies and code paths minimal per platform.

#[cfg(unix)]
pub use ptree_scheduler_unix::{check_scheduler_status, install_scheduler, uninstall_scheduler};
#[cfg(windows)]
pub use ptree_scheduler_windows::{check_scheduler_status, install_scheduler, uninstall_scheduler};
