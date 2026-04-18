use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub last_view: String,
    pub browse_section: String,
    pub lingq_api_key: String,
    pub lingq_collection_id: Option<i64>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_view: "browse".to_owned(),
            browse_section: "politik".to_owned(),
            lingq_api_key: String::new(),
            lingq_collection_id: None,
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    data: AppSettings,
}

impl SettingsStore {
    pub fn load_default() -> Result<Self> {
        let mut base_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from(r"C:\Users\Admin\AppData\Local"));
        base_dir.push("taz_lingq_tool");
        std::fs::create_dir_all(&base_dir)
            .with_context(|| format!("failed to create {}", base_dir.display()))?;
        let path = base_dir.join("settings.json");
        Self::load(path)
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let data = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            AppSettings::default()
        };

        Ok(Self { path, data })
    }

    pub fn data(&self) -> &AppSettings {
        &self.data
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
