# Build the release binary and Windows installer.
#
# Requires Rust and Inno Setup.
#   winget install JRSoftware.InnoSetup

$ErrorActionPreference = "Stop"

function Get-CargoVersion {
    $line = Get-Content "Cargo.toml" | Where-Object { $_ -match '^version\s*=' } | Select-Object -First 1
    if (-not $line) { throw "Could not find package version in Cargo.toml" }
    return ($line -replace '^version\s*=\s*"', '' -replace '"\s*$', '').Trim()
}

# Find iscc.exe (Inno Setup compiler)
$candidates = @(
    "${env:LOCALAPPDATA}\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles}\Inno Setup 6\ISCC.exe"
)
try { $found = (Get-Command iscc -ErrorAction SilentlyContinue).Source; if ($found) { $candidates += $found } } catch {}
$iscc = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $iscc) {
    Write-Host "ERROR: Inno Setup not found." -ForegroundColor Red
    Write-Host "Install it with:  winget install JRSoftware.InnoSetup" -ForegroundColor Yellow
    exit 1
}

Write-Host "=== Building Taz Reader release binary ===" -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host ""
Write-Host "=== Creating installer with Inno Setup ===" -ForegroundColor Cyan
Write-Host "Using: $iscc"
$version = Get-CargoVersion
& $iscc "/DMyAppVersion=$version" "installer\taz-reader.iss"
if ($LASTEXITCODE -ne 0) { throw "Inno Setup compilation failed" }

$exe = Get-Item "installer\output\taz-reader-setup.exe"
$hash = (Get-FileHash -Path $exe.FullName -Algorithm SHA256).Hash
Write-Host ""
Write-Host "=== Done! ===" -ForegroundColor Green
Write-Host "Installer: $($exe.FullName)" -ForegroundColor Yellow
Write-Host "File size: $([math]::Round($exe.Length / 1MB, 1)) MB"
Write-Host "SHA256: $hash"
