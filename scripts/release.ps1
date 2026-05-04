# Validate the repo, build the installer, and optionally publish a GitHub release.
#
# Usage:
#   .\scripts\release.ps1
#   .\scripts\release.ps1 -Publish

param(
    [switch]$Publish,
    [string]$Repo = "funwithcthulhu/taz-reader"
)

$ErrorActionPreference = "Stop"

function Get-CargoVersion {
    $line = Get-Content "Cargo.toml" | Where-Object { $_ -match '^version\s*=' } | Select-Object -First 1
    if (-not $line) { throw "Could not find package version in Cargo.toml" }
    return ($line -replace '^version\s*=\s*"', '' -replace '"\s*$', '').Trim()
}

$version = Get-CargoVersion
$tag = "v$version"
$installer = "installer\output\taz-reader-setup.exe"

Write-Host "=== Validating Taz Reader $version ===" -ForegroundColor Cyan
cargo test -- --test-threads=1
if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }

Write-Host ""
Write-Host "=== Running Clippy ===" -ForegroundColor Cyan
cargo clippy --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }

Write-Host ""
Write-Host "=== Building installer ===" -ForegroundColor Cyan
& "$PSScriptRoot\build-installer.ps1"
if ($LASTEXITCODE -ne 0) { throw "installer build failed" }

$hash = (Get-FileHash -Path $installer -Algorithm SHA256).Hash
Write-Host ""
Write-Host "Installer SHA256: $hash" -ForegroundColor Yellow

if (-not $Publish) {
    Write-Host ""
    Write-Host "Release not published. Re-run with -Publish to create/upload $tag." -ForegroundColor Gray
    exit 0
}

Write-Host ""
Write-Host "=== Publishing GitHub release $tag ===" -ForegroundColor Cyan
gh auth status | Out-Host
if ($LASTEXITCODE -ne 0) { throw "GitHub CLI is not authenticated" }

$notes = @"
Windows installer for Taz Reader $version.

SHA256:
$hash

This build is unsigned, so Windows may show an Unknown publisher or SmartScreen warning on first install.
"@

$existing = gh release view $tag --repo $Repo --json tagName 2>$null
if ($LASTEXITCODE -eq 0 -and $existing) {
    gh release upload $tag "$installer#taz-reader-setup.exe" --repo $Repo --clobber
} else {
    gh release create $tag "$installer#taz-reader-setup.exe" --repo $Repo --target main --title "Taz Reader $version" --notes $notes --latest
}

Write-Host ""
Write-Host "Published $tag to $Repo" -ForegroundColor Green
