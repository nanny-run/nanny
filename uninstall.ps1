# Uninstall the Nanny CLI on Windows
# Usage: irm https://install.nanny.run/uninstall | iex

$ErrorActionPreference = "Stop"

$InstallDir = Join-Path $Env:LOCALAPPDATA "nanny"

# ── Remove from user PATH ──────────────────────────────────────────────────────
$CurrentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($CurrentPath -like "*$InstallDir*") {
    $NewPath = ($CurrentPath -split ";" | Where-Object { $_ -ne $InstallDir }) -join ";"
    [Environment]::SetEnvironmentVariable("PATH", $NewPath, "User")
    Write-Host "Removed $InstallDir from PATH."
}

# ── Remove the install directory ───────────────────────────────────────────────
if (Test-Path $InstallDir) {
    Remove-Item -Force -Recurse $InstallDir
    Write-Host ""
    Write-Host "nanny uninstalled."
} else {
    Write-Host "Nanny installation not found at $InstallDir."
}
