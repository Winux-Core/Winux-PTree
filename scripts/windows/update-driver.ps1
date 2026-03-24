#requires -Version 5.1
<#
    Update the installed ptree binaries on Windows.
    - Rebuilds in release mode
    - Copies ptree.exe (and Ptree.exe shim) to the install directory

    Example:
      powershell -ExecutionPolicy Bypass -File scripts/windows/update-driver.ps1
#>

[CmdletBinding()]
param(
    [string]$InstallDir = "$Env:ProgramFiles\PTree"
)

if (-not $IsWindows) {
    Write-Error "This updater is for Windows only."; exit 1
}

$repoRoot = (Split-Path -Path $PSScriptRoot -Parent -Resolve)
$repoRoot = (Split-Path -Path $repoRoot -Parent -Resolve)

Write-Host "Building ptree (release)..."
cargo build --release | Write-Host

$src = Join-Path $repoRoot "target\release\ptree.exe"
if (-not (Test-Path $src)) {
    Write-Error "Build did not produce $src"; exit 1
}

Write-Host "Updating binaries in $InstallDir"
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
Copy-Item $src (Join-Path $InstallDir "ptree.exe") -Force
Copy-Item (Join-Path $InstallDir "ptree.exe") (Join-Path $InstallDir "Ptree.exe") -Force

Write-Host "Update complete." 
