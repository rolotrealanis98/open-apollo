# setup.ps1 — Install WinRing0 for Apollo PCIe register capture on Windows
#
# WinRing0 is an open-source library that provides userspace access to
# PCIe configuration and memory-mapped registers. It installs a small
# kernel-mode driver that bridges userspace requests to hardware.
#
# This script downloads, verifies, and installs WinRing0 so that
# capture.py can read Apollo device registers.
#
# IMPORTANT: Run this script as Administrator.
#
# Usage:
#   Right-click PowerShell -> Run as Administrator
#   .\setup.ps1

#Requires -RunAsAdministrator

$ErrorActionPreference = "Stop"

Write-Host ""
Write-Host "Open Apollo — WinRing0 Setup" -ForegroundColor Cyan
Write-Host "=============================" -ForegroundColor Cyan
Write-Host ""

# ============================================================================
# What is WinRing0?
# ============================================================================
Write-Host "WinRing0 is an open-source kernel driver that allows reading"
Write-Host "PCIe device registers from userspace. It is used by many hardware"
Write-Host "monitoring tools (HWiNFO, Open Hardware Monitor, etc.)."
Write-Host ""
Write-Host "The capture script uses WinRing0 to READ (never write) register"
Write-Host "values from your Apollo interface."
Write-Host ""

# ============================================================================
# Configuration
# ============================================================================

# TODO: Replace with actual download URL and SHA256 hash once a trusted
# release is identified. WinRing0 source is available at:
#   https://github.com/GermanAizek/WinRing0
#
# The binary release should be verified against a known hash.

$DownloadUrl = "https://github.com/GermanAizek/WinRing0/releases/download/TODO/WinRing0.zip"
$ExpectedSHA256 = "TODO_REPLACE_WITH_ACTUAL_SHA256_HASH"
$InstallDir = "$env:ProgramFiles\OpenApollo\WinRing0"
$ZipPath = "$env:TEMP\WinRing0.zip"
$DriverName = "WinRing0_1_2_0"

# ============================================================================
# Check if already installed
# ============================================================================
$existingService = Get-Service -Name $DriverName -ErrorAction SilentlyContinue
if ($existingService) {
    Write-Host "WinRing0 is already installed as a service." -ForegroundColor Green
    Write-Host "Status: $($existingService.Status)"
    Write-Host ""
    $response = Read-Host "Reinstall? (y/N)"
    if ($response -ne "y") {
        Write-Host "Keeping existing installation."
        exit 0
    }
    # Stop and remove existing service
    Write-Host "Removing existing installation..."
    Stop-Service -Name $DriverName -Force -ErrorAction SilentlyContinue
    sc.exe delete $DriverName | Out-Null
}

# ============================================================================
# Download
# ============================================================================
Write-Host "Downloading WinRing0..."
Write-Host "  URL: $DownloadUrl"

# TODO: Uncomment when URL is finalized
# Invoke-WebRequest -Uri $DownloadUrl -OutFile $ZipPath -UseBasicParsing
Write-Host ""
Write-Host "ERROR: Download URL not yet configured." -ForegroundColor Red
Write-Host "This is a template script. The WinRing0 download URL and SHA256" -ForegroundColor Yellow
Write-Host "hash need to be filled in before this script can run." -ForegroundColor Yellow
Write-Host ""
Write-Host "If you want to help set this up, please open an issue at:" -ForegroundColor Yellow
Write-Host "  https://github.com/rolotrealanis98/open-apollo/issues" -ForegroundColor Yellow
exit 1

# ============================================================================
# Verify SHA256
# ============================================================================
Write-Host "Verifying SHA256 hash..."
$actualHash = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash
if ($actualHash -ne $ExpectedSHA256) {
    Write-Host "SHA256 MISMATCH!" -ForegroundColor Red
    Write-Host "  Expected: $ExpectedSHA256"
    Write-Host "  Got:      $actualHash"
    Write-Host ""
    Write-Host "The download may be corrupted or tampered with. Aborting."
    Remove-Item $ZipPath -Force
    exit 1
}
Write-Host "SHA256 verified." -ForegroundColor Green

# ============================================================================
# Extract and install
# ============================================================================
Write-Host "Installing to $InstallDir..."
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
Expand-Archive -Path $ZipPath -DestinationPath $InstallDir -Force
Remove-Item $ZipPath -Force

# Determine architecture
$driverFile = if ([Environment]::Is64BitOperatingSystem) {
    Join-Path $InstallDir "WinRing0x64.sys"
} else {
    Join-Path $InstallDir "WinRing0.sys"
}

if (-not (Test-Path $driverFile)) {
    Write-Host "Driver file not found: $driverFile" -ForegroundColor Red
    exit 1
}

# Install as kernel service
Write-Host "Creating kernel service..."
sc.exe create $DriverName binPath= $driverFile type= kernel start= demand | Out-Null
sc.exe start $DriverName | Out-Null

Write-Host ""
Write-Host "WinRing0 installed and running." -ForegroundColor Green
Write-Host ""
Write-Host "You can now run the capture script:"
Write-Host "  python capture.py"
Write-Host ""

# ============================================================================
# Uninstall instructions
# ============================================================================
Write-Host "============================="
Write-Host "  To uninstall WinRing0:" -ForegroundColor Cyan
Write-Host "============================="
Write-Host ""
Write-Host "  1. Open PowerShell as Administrator"
Write-Host "  2. Run:"
Write-Host "     Stop-Service -Name $DriverName -Force"
Write-Host "     sc.exe delete $DriverName"
Write-Host "     Remove-Item -Recurse '$InstallDir'"
Write-Host ""
