# Install the Nanny CLI on Windows
# Usage: irm https://install.nanny.run/windows | iex
#
# Downloads the latest nanny-windows-x86_64.zip from GitHub Releases,
# extracts to $Env:LOCALAPPDATA\nanny\, and adds it to the user PATH.

$ErrorActionPreference = "Stop"

$Repo     = "nanny-run/nanny"
$Artifact = "nanny-windows-x86_64"
$Url      = "https://github.com/$Repo/releases/latest/download/$Artifact.zip"
$InstallDir = Join-Path $Env:LOCALAPPDATA "nanny"

# ── Download ──────────────────────────────────────────────────────────────────
Write-Host "Downloading $Artifact..."
$Zip = Join-Path $Env:TEMP "$Artifact.zip"
Invoke-WebRequest -Uri $Url -OutFile $Zip -UseBasicParsing

# ── Extract ───────────────────────────────────────────────────────────────────
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir | Out-Null
}
Expand-Archive -Path $Zip -DestinationPath $InstallDir -Force
Remove-Item $Zip

# ── Add to user PATH (persistent) ─────────────────────────────────────────────
$CurrentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($CurrentPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable(
        "PATH",
        "$InstallDir;$CurrentPath",
        "User"
    )
    Write-Host ""
    Write-Host "Added $InstallDir to your PATH."
    Write-Host "Restart your terminal for the change to take effect."
} else {
    Write-Host ""
    Write-Host "$InstallDir is already in your PATH."
}

Write-Host ""
Write-Host "nanny installed to $InstallDir\nanny.exe"
Write-Host ""
Write-Host "Run 'nanny --version' to confirm (after restarting your terminal)."
