use anyhow::{Context, Result};
use log::warn;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    // Navigation
    pub last_view: String,
    pub browse_section: String,

    // Browse filters
    pub browse_only_new: bool,
    pub browse_date_from: String,
    pub browse_date_to: String,

    // Bulk fetch params
    pub bulk_max_articles: String,
    pub bulk_per_section_cap: String,
    pub bulk_stop_after_old: String,

    // Library filters
    pub library_sort: String,
    pub library_only_not_uploaded: bool,
    pub library_min_words: String,
    pub library_max_words: String,
    pub library_duplicate_only: bool,

    // LingQ settings
    pub lingq_language: String,
    pub lingq_api_key: String,
    pub lingq_collection_id: Option<i64>,
    pub lingq_only_not_uploaded: bool,
    pub lingq_min_words: String,
    pub lingq_max_words: String,

    // UI toggles
    pub show_library_filters: bool,
    pub show_upload_tools: bool,
    pub preview_wide: bool,
    pub article_density: String,

    // Auto-fetch
    pub auto_fetch_on_startup: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_view: "browse".to_owned(),
            browse_section: "politik".to_owned(),
            browse_only_new: true,
            browse_date_from: String::new(),
            browse_date_to: String::new(),
            bulk_max_articles: "60".to_owned(),
            bulk_per_section_cap: "30".to_owned(),
            bulk_stop_after_old: "12".to_owned(),
            library_sort: "newest".to_owned(),
            library_only_not_uploaded: false,
            library_min_words: String::new(),
            library_max_words: String::new(),
            library_duplicate_only: false,
            lingq_language: "de".to_owned(),
            lingq_api_key: String::new(),
            lingq_collection_id: None,
            lingq_only_not_uploaded: true,
            lingq_min_words: String::new(),
            lingq_max_words: String::new(),
            show_library_filters: true,
            show_upload_tools: true,
            preview_wide: false,
            article_density: "compact".to_owned(),
            auto_fetch_on_startup: false,
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    data: AppSettings,
}

impl SettingsStore {
    /// Create a store with defaults that saves to a temp-dir path.
    /// Used when the normal settings path is inaccessible.
    pub fn in_memory_default() -> Self {
        Self {
            path: std::env::temp_dir().join("taz_reader_settings.json"),
            data: AppSettings::default(),
        }
    }

    pub fn load_default() -> Result<Self> {
        let path = crate::app_data_dir()?.join("settings.json");
        Self::load(path)
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let data = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            match serde_json::from_str(&raw) {
                Ok(settings) => settings,
                Err(err) => {
                    eprintln!(
                        "warning: settings file {} is malformed ({}), using defaults",
                        path.display(),
                        err
                    );
                    AppSettings::default()
                }
            }
        } else {
            AppSettings::default()
        };

        Ok(Self { path, data })
    }

    pub fn data(&self) -> &AppSettings {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut AppSettings {
        &mut self.data
    }

    pub fn update<F>(&mut self, updater: F) -> Result<()>
    where
        F: FnOnce(&mut AppSettings),
    {
        updater(&mut self.data);
        self.save()
    }

    pub fn save(&self) -> Result<()> {
        let raw =
            serde_json::to_string_pretty(&self.data).context("failed to serialize settings")?;
        std::fs::write(&self.path, raw)
            .with_context(|| format!("failed to write {}", self.path.display()))?;
        Ok(())
    }
}

// ── Separate API key storage ──
//
// The LingQ API key is stored in its own file rather than in settings.json
// to reduce exposure. On Linux/macOS the file gets restricted permissions (0600).
// If the OS keyring crate becomes available, this can be upgraded to use it.

fn api_key_path() -> Result<PathBuf> {
    Ok(crate::app_data_dir()?.join("lingq_token"))
}

/// Load the LingQ API key from its dedicated file, falling back to settings.json
/// for migration from older versions.
pub fn load_api_key(settings: &mut AppSettings) -> String {
    // Try the dedicated token file first
    if let Ok(path) = api_key_path() {
        if path.exists() {
            if let Ok(token) = std::fs::read_to_string(&path) {
                let token = token.trim().to_owned();
                if !token.is_empty() {
                    return token;
                }
            }
        }
    }
    // Fall back to the legacy settings.json field and migrate it out
    let legacy = std::mem::take(&mut settings.lingq_api_key);
    if !legacy.trim().is_empty() {
        if let Err(err) = save_api_key(&legacy) {
            warn!("Failed to migrate API key to token file: {err:#}");
        }
    }
    legacy
}

/// Save the LingQ API key to its dedicated file with restricted permissions.
pub fn save_api_key(key: &str) -> Result<()> {
    let path = api_key_path()?;
    std::fs::write(&path, key)
        .with_context(|| format!("failed to write token to {}", path.display()))?;
    // Restrict permissions on Unix (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn settings_round_trip() {
        let dir = std::env::temp_dir().join("taz_test_settings");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_settings.json");

        // Save non-default settings
        let store = SettingsStore::load(path.clone()).unwrap();
        store.save().unwrap();

        // Update and re-save
        let mut store = SettingsStore::load(path.clone()).unwrap();
        store.update(|s| {
            s.browse_section = "kultur".to_owned();
            s.browse_only_new = false;
            s.library_sort = "title".to_owned();
            s.library_duplicate_only = true;
            s.article_density = "comfortable".to_owned();
            s.lingq_language = "fr".to_owned();
            s.bulk_max_articles = "100".to_owned();
        }).unwrap();

        // Reload and verify
        let loaded = SettingsStore::load(path.clone()).unwrap();
        let d = loaded.data();
        assert_eq!(d.browse_section, "kultur");
        assert_eq!(d.browse_only_new, false);
        assert_eq!(d.library_sort, "title");
        assert!(d.library_duplicate_only);
        assert_eq!(d.article_density, "comfortable");
        assert_eq!(d.lingq_language, "fr");
        assert_eq!(d.bulk_max_articles, "100");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_malformed_json_uses_defaults() {
        let dir = std::env::temp_dir().join("taz_test_malformed");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("bad_settings.json");
        std::fs::write(&path, "not json at all").unwrap();

        let store = SettingsStore::load(path).unwrap();
        let d = store.data();
        assert_eq!(d.browse_section, "politik");
        assert_eq!(d.lingq_language, "de");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_missing_file_uses_defaults() {
        let path = PathBuf::from("/tmp/taz_nonexistent_12345.json");
        let store = SettingsStore::load(path).unwrap();
        assert_eq!(store.data().browse_section, "politik");
    }

    #[test]
    fn settings_partial_json_fills_defaults() {
        let dir = std::env::temp_dir().join("taz_test_partial");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("partial.json");
        std::fs::write(&path, r#"{"browse_section":"sport"}"#).unwrap();

        let store = SettingsStore::load(path).unwrap();
        let d = store.data();
        assert_eq!(d.browse_section, "sport");
        assert_eq!(d.lingq_language, "de"); // default for missing field

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_memory_default_has_correct_defaults() {
        let store = SettingsStore::in_memory_default();
        assert_eq!(store.data().browse_section, "politik");
        assert_eq!(store.data().lingq_language, "de");
    }
}
