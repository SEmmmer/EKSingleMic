use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiPage {
    Devices,
    Training,
    Inference,
    Debug,
}

impl UiPage {
    pub const ALL: [Self; 4] = [Self::Devices, Self::Training, Self::Inference, Self::Debug];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Devices => "设备",
            Self::Training => "训练",
            Self::Inference => "推理",
            Self::Debug => "调试",
        }
    }
}

impl Default for UiPage {
    fn default() -> Self {
        Self::Devices
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceMode {
    Passthrough,
    BasicFilter,
    StrongIsolation,
}

impl InferenceMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Passthrough => "Passthrough",
            Self::BasicFilter => "Basic Filter",
            Self::StrongIsolation => "Strong Isolation (实验性)",
        }
    }
}

impl Default for InferenceMode {
    fn default() -> Self {
        Self::Passthrough
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    pub selected_page: UiPage,
    pub selected_input_device: Option<String>,
    pub selected_output_device: Option<String>,
    pub inference_mode: InferenceMode,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            selected_page: UiPage::Devices,
            selected_input_device: None,
            selected_output_device: None,
            inference_mode: InferenceMode::Passthrough,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new() -> Result<Self> {
        let project_dirs = ProjectDirs::from("com", "SUPERXLB", "EKSingleMic")
            .ok_or_else(|| anyhow!("failed to resolve Windows config directory"))?;

        let config_dir = project_dirs.config_dir();
        fs::create_dir_all(config_dir).with_context(|| {
            format!(
                "failed to create config directory: {}",
                config_dir.display()
            )
        })?;

        Ok(Self {
            path: config_dir.join("settings.json"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<AppSettings> {
        if !self.path.exists() {
            return Ok(AppSettings::default());
        }

        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read settings file: {}", self.path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse settings file: {}", self.path.display()))
    }

    pub fn save(&self, settings: &AppSettings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create settings parent directory: {}",
                    parent.display()
                )
            })?;
        }

        let json =
            serde_json::to_string_pretty(settings).context("failed to serialize settings")?;
        fs::write(&self.path, json)
            .with_context(|| format!("failed to write settings file: {}", self.path.display()))?;
        Ok(())
    }
}
