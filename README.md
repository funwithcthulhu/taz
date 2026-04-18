# taz LingQ Tool

A Rust-based replacement for the old `zeit_lingq_tool`, aimed at the same workflow:

- browse `taz.de` section pages
- extract clean full article text
- save articles into a local SQLite library
- upload saved articles to LingQ

The current version is intentionally CLI-first so the scraping and LingQ plumbing are solid before a GUI is added.

## Why Rust

Rust is the best fit of the requested stacks in this environment:

- `rustc` and `cargo` are installed already
- HTTP + HTML parsing + SQLite are straightforward and fast
- the code can later be wrapped in a desktop UI if we want to match the old Electron app more closely

## Commands

```powershell
# List built-in section shortcuts
cargo run -- sections

# Browse a built-in taz section
cargo run -- browse --section politik --limit 15

# Browse any arbitrary taz URL directly
cargo run -- browse-url --url https://taz.de/Politik/!p4615/ --limit 15

# Fetch a single article and print the cleaned text
cargo run -- fetch --url https://taz.de/Vertrauen-in-die-Politik/!6073221/

# Fetch and also save it into the local SQLite library
cargo run -- fetch --url https://taz.de/Vertrauen-in-die-Politik/!6073221/ --save

# Show saved articles
cargo run -- library --limit 20

# Upload a saved article to LingQ
cargo run -- upload --id 1 --api-key YOUR_LINGQ_API_KEY
```

You can also provide the LingQ token through `LINGQ_API_KEY`.

## Storage

The SQLite database is created at:

`%LOCALAPPDATA%\taz_lingq_tool\taz_lingq_tool.db`

## Current Scope

This first pass ports the core workflow:

- section browsing
- article extraction
- local storage
- LingQ upload

The old Electron app had more features like session-based publisher login, audio handling, and known-word syncing. `taz.de` currently exposes articles without a paywall, so this Rust version focuses first on the high-value path and avoids unnecessary login machinery.
