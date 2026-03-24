#[cfg(windows)]
use std::process::Command;

use anyhow::{anyhow, Result};
#[cfg(any(windows, test))]
use ptree_core::SCHEDULED_REFRESH_ARGS;

#[cfg(any(windows, test))]
fn scheduled_task_script(exe_path_str: &str, task_name: &str) -> String {
    format!(
        r#"
$action = New-ScheduledTaskAction -Execute "{}" -Argument "{}"
$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date) -RepetitionInterval (New-TimeSpan -Minutes 30) -RepetitionDuration (New-TimeSpan -Days 36500)
$principal = New-ScheduledTaskPrincipal -UserID "$env:USERNAME" -LogonType Interactive -RunLevel Highest
$task = New-ScheduledTask -Action $action -Trigger $trigger -Principal $principal -Description "Automatic ptree cache refresh every 30 minutes"
Register-ScheduledTask -TaskName "{}" -InputObject $task -Force
Write-Host "✓ Scheduled task '{}' created successfully"
"#,
        exe_path_str.replace("\\", "\\\\"),
        SCHEDULED_REFRESH_ARGS,
        task_name,
        task_name
    )
}

/// Install a scheduled task that refreshes the cache every 30 minutes.
#[cfg(windows)]
pub fn install_scheduler() -> Result<()> {
    let exe_path = std::env::current_exe()?;
    let exe_path_str = exe_path.display().to_string();

    let task_name = "PTreeCacheRefresh";
    let ps_script = scheduled_task_script(&exe_path_str, task_name);

    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(&ps_script)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to create scheduled task: {}", stderr));
    }

    println!("✓ Cache refresh scheduled for every 30 minutes");
    println!("  Scheduled args: {}", SCHEDULED_REFRESH_ARGS);
    println!("  Run 'ptree --scheduler-status' to verify installation");
    Ok(())
}

#[cfg(not(windows))]
pub fn install_scheduler() -> Result<()> {
    Err(anyhow!("Windows scheduler is only available on Windows targets"))
}

/// Remove the scheduled task.
#[cfg(windows)]
pub fn uninstall_scheduler() -> Result<()> {
    let task_name = "PTreeCacheRefresh";

    let ps_script = format!(
        r#"
$task = Get-ScheduledTask -TaskName "{}" -ErrorAction SilentlyContinue
if ($task) {{
    Unregister-ScheduledTask -TaskName "{}" -Confirm:$false
    Write-Host "✓ Scheduled task removed"
}} else {{
    Write-Host "✗ Task not found"
}}
"#,
        task_name, task_name
    );

    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(&ps_script)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to remove scheduled task: {}", stderr));
    }

    println!("✓ Cache refresh scheduler removed");
    Ok(())
}

#[cfg(not(windows))]
pub fn uninstall_scheduler() -> Result<()> {
    Err(anyhow!("Windows scheduler is only available on Windows targets"))
}

/// Display task scheduler status.
#[cfg(windows)]
pub fn check_scheduler_status() -> Result<()> {
    let task_name = "PTreeCacheRefresh";

    let ps_script = format!(
        r#"
$task = Get-ScheduledTask -TaskName "{}" -ErrorAction SilentlyContinue
if ($task) {{
    Write-Host "✓ Scheduler installed and active"
    Write-Host ""
    Write-Host "Task Details:"
    Write-Host "  Name:        $($task.TaskName)"
    Write-Host "  State:       $($task.State)"
    Write-Host "  Path:        $($task.TaskPath)"
    Write-Host "  Last Run:    $($task.LastRunTime)"
    Write-Host "  Next Run:    $($task.NextRunTime)"
    Write-Host ""
    Write-Host "Run 'Get-ScheduledTask -TaskName \"{}\" | Format-List *' for more details"
}} else {{
    Write-Host "✗ Scheduler not installed"
    Write-Host ""
    Write-Host "Install with: ptree --scheduler"
}}
"#,
        task_name, task_name
    );

    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(&ps_script)
        .output()?;

    println!("{}", String::from_utf8_lossy(&output.stdout));

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("{}", stderr);
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn check_scheduler_status() -> Result<()> {
    Err(anyhow!("Windows scheduler is only available on Windows targets"))
}

#[cfg(test)]
mod tests {
    use ptree_core::SCHEDULED_REFRESH_ARGS;

    use super::scheduled_task_script;

    #[test]
    fn source_uses_shared_non_force_refresh_args() {
        let script = scheduled_task_script(r"C:\Program Files\PTree\ptree.exe", "PTreeCacheRefresh");

        assert!(script.contains(SCHEDULED_REFRESH_ARGS));
        assert!(!script.contains("--force"));
        assert!(!SCHEDULED_REFRESH_ARGS.contains("--force"));
    }
}
