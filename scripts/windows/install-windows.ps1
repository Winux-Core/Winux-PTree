#requires -Version 5.1
<#
    Install PTree on Windows.
    - Builds the release binary via Cargo
    - Copies ptree.exe to the install directory (default: %ProgramFiles%\PTree)
    - Optionally registers a scheduled task to refresh the cache every 30 minutes

    Examples:
      powershell -ExecutionPolicy Bypass -File scripts/windows/install-windows.ps1
      powershell -ExecutionPolicy Bypass -File scripts/windows/install-windows.ps1 -RegisterScheduledTask
#>

[CmdletBinding()]
param(
    [string]$InstallDir = "$Env:ProgramFiles\PTree",
    [switch]$RegisterScheduledTask,
    [string]$RefreshArgs = "--quiet --cache-ttl 30"
)

if (-not $IsWindows) {
    Write-Error "This installer is for Windows only."; exit 1
}

$repoRoot = (Split-Path -Path $PSScriptRoot -Parent -Resolve)
$repoRoot = (Split-Path -Path $repoRoot -Parent -Resolve) # go to repo root

Write-Host "Building ptree (release)..."
cargo build --release | Write-Host

$src = Join-Path $repoRoot "target\release\ptree.exe"
if (-not (Test-Path $src)) {
    Write-Error "Build did not produce $src"; exit 1
}

Write-Host "Installing to $InstallDir"
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
Copy-Item $src (Join-Path $InstallDir "ptree.exe") -Force

# Add a convenience shim for uppercase invocation (parity with Linux install)
Copy-Item (Join-Path $InstallDir "ptree.exe") (Join-Path $InstallDir "Ptree.exe") -Force

# Optionally register scheduled cache refresh
if ($RegisterScheduledTask) {
    Write-Host "Registering scheduled task 'PTreeCacheRefresh'"
    $action = New-ScheduledTaskAction -Execute (Join-Path $InstallDir "ptree.exe") -Argument $RefreshArgs
    $trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 30) -RepetitionDuration ([TimeSpan]::MaxValue)
    $principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -RunLevel Highest
    Register-ScheduledTask -TaskName "PTreeCacheRefresh" -Action $action -Trigger $trigger -Principal $principal -Description "Automatic PTree cache refresh" -Force
}

Write-Host "Done. Add $InstallDir to PATH or call with full path."
