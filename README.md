# taz Reader

Desktop app and CLI for discovering articles from `taz.de`, saving them into a local library, and uploading selected texts to LingQ.

This project started as a Rust replacement for `zeit_lingq_tool`, but it now has its own Windows-native workflow:

- Slint desktop GUI
- async `taz.de` article discovery and extraction
- local SQLite article library
- LingQ login or saved API token support
- Windows installer build with Inno Setup

## Current Highlights

- Browse built-in `taz.de` sections and load more article candidates.
- Search and discover across section pages and related topic pages.
- Save articles locally with metadata, clean text, and word counts.
- Filter the library by heading, section, upload status, and word count.
- Preview cleaned article text before uploading.
- Upload selected articles to a LingQ course/collection.
- Save LingQ credentials/settings in the local app data area.
- Build a Windows installer and desktop/start-menu shortcuts.

## Tech Stack

- Rust 2024
- Slint for the desktop UI
- Tokio + Reqwest for async networking
- Scraper + Regex for HTML extraction
- Rusqlite for the local library
- Inno Setup for the Windows installer

## Running The App

Launch the GUI:

```powershell
cargo run -- gui
```

Or just:

```powershell
cargo run
```

## CLI Commands

```powershell
# List built-in section shortcuts
cargo run -- sections

# Browse a built-in taz section
cargo run -- browse --section politik --limit 15

# Browse an arbitrary taz URL directly
cargo run -- browse-url --url https://taz.de/Politik/!p4615/ --limit 15

# Fetch a single article and print the cleaned text
cargo run -- fetch --url https://taz.de/Vertrauen-in-die-Politik/!6073221/

# Fetch and also save it into the local library
cargo run -- fetch --url https://taz.de/Vertrauen-in-die-Politik/!6073221/ --save

# Show saved articles
cargo run -- library --limit 20

# Upload a saved article to LingQ
cargo run -- upload --id 1 --api-key YOUR_LINGQ_API_KEY
```

## LingQ Authentication

The app supports multiple ways to get a LingQ token:

- pass `--api-key` on the CLI
- set `LINGQ_API_KEY`
- save a token in the GUI settings
- log in from the GUI and let the app save the token locally

## Local Storage

App data is stored under:

`%LOCALAPPDATA%\taz_lingq_tool\`

That includes:

- the SQLite database
- GUI/settings data
- saved LingQ token information

## Project Layout

```text
src/
  gui/                  Slint GUI state, callbacks, actions, sync
  database.rs           SQLite storage and queries
  lingq.rs              LingQ login, course listing, upload logic
  settings.rs           Persistent app settings and token loading
  taz.rs                taz.de discovery and article extraction
ui/
  app-window.slint      Main Slint UI definition
assets/
  taz.ico               Embedded Windows app icon
installer/
  taz-reader.iss        Inno Setup installer definition
scripts/
  build-installer.ps1   Release + installer build helper
```

## Building

Debug build:

```powershell
cargo build
```

Release build:

```powershell
cargo build --release
```

## Building The Windows Installer

One-time prerequisite:

```powershell
winget install JRSoftware.InnoSetup
```

Then build the installer:

```powershell
.\scripts\build-installer.ps1
```

Expected output:

`installer\output\taz-reader-setup.exe`

## Notes

- The executable embeds the `taz` icon on Windows via `build.rs`.
- The release binary is configured to hide the console window on Windows.
- The app is designed as a native desktop executable, not a local web server.
