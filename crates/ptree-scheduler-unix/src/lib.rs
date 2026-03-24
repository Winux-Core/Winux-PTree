use anyhow::{anyhow, Result};
use ptree_core::SCHEDULED_REFRESH_ARGS;

const LEGACY_SCHEDULED_REFRESH_ARGS: &str = "--force --quiet";

fn cron_entry(exe_path: &str, args: &str) -> String {
    format!("*/30 * * * * {} {}", exe_path, args)
}

fn replace_or_append_scheduler_entry(crontab_content: &str, exe_path: &str) -> (String, bool) {
    let desired_entry = cron_entry(exe_path, SCHEDULED_REFRESH_ARGS);
    let legacy_entry = cron_entry(exe_path, LEGACY_SCHEDULED_REFRESH_ARGS);

    let mut changed = false;
    let mut found_desired = false;
    let mut new_lines = Vec::new();

    for line in crontab_content.lines() {
        if line == desired_entry {
            if !found_desired {
                new_lines.push(desired_entry.clone());
                found_desired = true;
            } else {
                changed = true;
            }
            continue;
        }

        if line == legacy_entry {
            if !found_desired {
                new_lines.push(desired_entry.clone());
                found_desired = true;
            }
            changed = true;
            continue;
        }

        new_lines.push(line.to_string());
    }

    if !found_desired {
        new_lines.push(desired_entry);
        changed = true;
    }

    (format!("{}\n", new_lines.join("\n")), changed)
}

fn remove_scheduler_entries(crontab_content: &str, exe_path: &str) -> (String, bool) {
    let desired_entry = cron_entry(exe_path, SCHEDULED_REFRESH_ARGS);
    let legacy_entry = cron_entry(exe_path, LEGACY_SCHEDULED_REFRESH_ARGS);

    let mut removed = false;
    let mut new_lines = Vec::new();

    for line in crontab_content.lines() {
        if line == desired_entry || line == legacy_entry {
            removed = true;
            continue;
        }
        new_lines.push(line.to_string());
    }

    if new_lines.is_empty() {
        (String::new(), removed)
    } else {
        (format!("{}\n", new_lines.join("\n")), removed)
    }
}

/// Install a cron entry that refreshes the cache every 30 minutes.
#[cfg(unix)]
pub fn install_scheduler() -> Result<()> {
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    let exe_path: PathBuf = std::env::current_exe()?;
    let exe_path_str = exe_path.display().to_string();

    let crontab_check = Command::new("which").arg("crontab").output();
    if crontab_check.is_err() || !crontab_check?.status.success() {
        return Err(anyhow!("crontab not found. Please install cron: sudo apt-get install cron (Ubuntu/Debian)"));
    }

    let current_crontab = Command::new("crontab").arg("-l").output().unwrap_or_else(|_| {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    });

    let crontab_content = if current_crontab.status.success() {
        String::from_utf8_lossy(&current_crontab.stdout).to_string()
    } else {
        String::new()
    };

    let (new_crontab, changed) = replace_or_append_scheduler_entry(&crontab_content, &exe_path_str);
    if !changed {
        println!("✓ Scheduler already installed");
        return Ok(());
    }

    let mut child = Command::new("crontab").arg("-").stdin(Stdio::piped()).spawn()?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("Failed to open crontab stdin"))?;
        stdin.write_all(new_crontab.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to install cron job: {}", stderr));
    }

    println!("✓ Cache refresh scheduled for every 30 minutes");
    println!("  Scheduled args: {}", SCHEDULED_REFRESH_ARGS);
    println!("  Run 'ptree --scheduler-status' to verify installation");
    Ok(())
}

#[cfg(not(unix))]
pub fn install_scheduler() -> Result<()> {
    Err(anyhow!("Unix scheduler is only available on Unix targets"))
}

/// Remove the ptree cron entry.
#[cfg(unix)]
pub fn uninstall_scheduler() -> Result<()> {
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    let exe_path: PathBuf = std::env::current_exe()?;
    let exe_path_str = exe_path.display().to_string();

    let current_crontab = Command::new("crontab").arg("-l").output().unwrap_or_else(|_| {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    });

    if !current_crontab.status.success() {
        println!("✗ No crontab found");
        return Ok(());
    }

    let crontab_content = String::from_utf8_lossy(&current_crontab.stdout);
    let (new_crontab, removed) = remove_scheduler_entries(&crontab_content, &exe_path_str);

    if !removed {
        println!("✗ ptree scheduler not found in crontab");
        return Ok(());
    }

    let mut child = Command::new("crontab").arg("-").stdin(Stdio::piped()).spawn()?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("Failed to open crontab stdin"))?;
        stdin.write_all(new_crontab.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to remove cron job: {}", stderr));
    }

    println!("✓ Cache refresh scheduler removed");
    Ok(())
}

#[cfg(not(unix))]
pub fn uninstall_scheduler() -> Result<()> {
    Err(anyhow!("Unix scheduler is only available on Unix targets"))
}

/// Check cron entry status.
#[cfg(unix)]
pub fn check_scheduler_status() -> Result<()> {
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::Command;

    let exe_path: PathBuf = std::env::current_exe()?;
    let exe_path_str = exe_path.display().to_string();

    let output = Command::new("crontab").arg("-l").output().unwrap_or_else(|_| {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    });

    let crontab_content = String::from_utf8_lossy(&output.stdout);
    if crontab_content.contains(&exe_path_str) {
        println!("✓ Scheduler installed and active\n");
        println!("Cron entry:");
        for line in crontab_content.lines() {
            if line.contains(&exe_path_str) {
                println!("  {}", line);
            }
        }
    } else {
        println!("✗ Scheduler not installed\n");
        println!("Install with: ptree --scheduler");
    }

    Ok(())
}

#[cfg(not(unix))]
pub fn check_scheduler_status() -> Result<()> {
    Err(anyhow!("Unix scheduler is only available on Unix targets"))
}

#[cfg(test)]
mod tests {
    use ptree_core::SCHEDULED_REFRESH_ARGS;

    use super::{cron_entry, remove_scheduler_entries, replace_or_append_scheduler_entry};

    #[test]
    fn install_migrates_legacy_force_entry() {
        let exe = "/usr/local/bin/ptree";
        let legacy = format!("{}\n", cron_entry(exe, "--force --quiet"));

        let (updated, changed) = replace_or_append_scheduler_entry(&legacy, exe);

        assert!(changed);
        assert!(updated.contains(&cron_entry(exe, SCHEDULED_REFRESH_ARGS)));
        assert!(!updated.contains("--force"));
    }

    #[test]
    fn install_is_noop_when_desired_entry_exists() {
        let exe = "/usr/local/bin/ptree";
        let current = format!("{}\n", cron_entry(exe, SCHEDULED_REFRESH_ARGS));

        let (updated, changed) = replace_or_append_scheduler_entry(&current, exe);

        assert!(!changed);
        assert_eq!(updated, current);
    }

    #[test]
    fn uninstall_removes_both_current_and_legacy_entries() {
        let exe = "/usr/local/bin/ptree";
        let current = cron_entry(exe, SCHEDULED_REFRESH_ARGS);
        let legacy = cron_entry(exe, "--force --quiet");
        let crontab = format!("{}\n{}\nMAILTO=root\n", current, legacy);

        let (updated, removed) = remove_scheduler_entries(&crontab, exe);

        assert!(removed);
        assert_eq!(updated, "MAILTO=root\n");
    }
}
