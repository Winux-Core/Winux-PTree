pub mod cli;
pub mod error;

pub const SCHEDULED_REFRESH_ARGS: &str = "--quiet --cache-ttl 30";
pub const SCHEDULED_REFRESH_CACHE_TTL_SECS: u64 = 30;

pub use cli::{parse_args, Args, ColorMode, OutputFormat};
pub use error::{PTreeError, PTreeResult};

#[cfg(test)]
mod tests {
    use super::SCHEDULED_REFRESH_ARGS;

    #[test]
    fn windows_installer_uses_shared_refresh_args_default() {
        let script = include_str!("../../../scripts/windows/install-windows.ps1");
        let expected = format!("[string]$RefreshArgs = \"{}\"", SCHEDULED_REFRESH_ARGS);

        assert!(
            script.contains(&expected),
            "Windows installer RefreshArgs default drifted from shared scheduler args"
        );
    }
}
