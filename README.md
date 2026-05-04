# Taz Reader

Taz Reader is a Windows desktop app for browsing articles from [`taz.de`](https://taz.de), saving them in a local library, and uploading selected texts to LingQ.

This repository ships a GUI application. It does not include a supported CLI or a stable library API.

## What It Does

- Browse built-in `taz.de` sections and related topic pages.
- Search `taz.de` from inside the app.
- Save cleaned article text, metadata, word counts, and paywall hints in a local SQLite library.
- Filter the library by title, section, upload status, duplicate likelihood, and word count.
- Preview cleaned article text before uploading it.
- Upload selected articles to a LingQ course or refresh an existing LingQ lesson with cleaned text.
- Sync LingQ lesson status back into the local library.
- Build a Windows installer.

## Quick Start

### Run the app from source

```powershell
cargo run
```

This launches the GUI.

### Build a release binary

```powershell
cargo build --release
```

### Validate the repo

```powershell
cargo test -- --test-threads=1
cargo clippy --all-targets -- -D warnings
```

## LingQ Authentication

Taz Reader stores the LingQ token in the app data directory as a separate file, not inside `settings.json`.

You can:

- paste a LingQ token into the GUI settings
- log in through the GUI and let the app save the token for you
- refresh course lists and sync status from inside the GUI

## Local Data

Taz Reader stores app data under:

`%LOCALAPPDATA%\taz-reader\`

That includes:

- the SQLite library database
- settings and UI state
- the saved LingQ token file

Older installs that used `%LOCALAPPDATA%\taz_lingq_tool\` are migrated automatically on first run after the rename.

The GUI includes an **Open library folder** action for jumping directly to this location.

## Windows Installer

One-time prerequisite:

```powershell
winget install JRSoftware.InnoSetup
```

Build the installer:

```powershell
.\scripts\build-installer.ps1
```

Expected output:

`installer\output\taz-reader-setup.exe`

The installer build script reads the version from `Cargo.toml`, builds the release binary, and passes the version through to Inno Setup so packaging metadata stays in sync.

## Release Workflow

To validate, build the installer, and optionally publish a GitHub release:

```powershell
# Validate and build only
.\scripts\release.ps1

# Validate, build, and publish/update the GitHub release for the Cargo.toml version
.\scripts\release.ps1 -Publish
```

The release helper:

- runs the test suite serially
- runs strict Clippy checks
- builds the Windows installer
- computes a SHA256 checksum
- creates or updates the GitHub release asset through GitHub CLI

## Project Layout

```text
src/
  gui/                  Slint UI state, callbacks, background work, view sync
  taz/                  taz.de discovery, extraction, cleanup, shared models
  database.rs           SQLite storage, queries, migrations
  lingq.rs              LingQ login, course listing, uploads, lesson sync
  settings.rs           Persistent settings and token storage
  lib.rs                App-data paths and migration helpers
ui/
  app-window.slint      Main Slint UI definition
assets/
  taz.ico               Embedded Windows app icon
installer/
  taz-reader.iss        Inno Setup installer definition
scripts/
  build-installer.ps1   Installer build helper
  release.ps1           Test, lint, package, and optional GitHub release helper
```

## Tech Stack

- Rust 2024
- Slint for the desktop UI
- Tokio + Reqwest for async networking
- Scraper + Regex for extraction and cleanup
- Rusqlite for the local article library
- Inno Setup for Windows packaging

## Notes

- The project targets Windows and runs as a native desktop executable.
- The release binary hides the console window on Windows.
- Public builds are currently unsigned, so Windows may show an Unknown publisher or SmartScreen warning.
