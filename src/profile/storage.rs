use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const PROFILE_FILENAME: &str = "speaker_profile.json";
pub const DEFAULT_PROFILE_ID: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerProfile {
    pub profile_id: String,
    pub created_at_utc: String,
    pub model_version: String,
    pub embedding_count: usize,
    pub embedding_dimension: Option<usize>,
    pub centroid: Vec<f32>,
    pub dispersion: Option<f32>,
    pub suggested_threshold: f32,
    pub prompt_locale: String,
    pub prompt_count: usize,
    pub free_speech_seconds: Option<u32>,
    pub raw_audio_path: Option<String>,
    pub cleaned_audio_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProfileSummary {
    pub profile_id: String,
    pub created_at_utc: String,
    pub suggested_threshold: f32,
    pub prompt_count: usize,
}

impl ProfileSummary {
    pub fn label(&self) -> String {
        format!(
            "{} | {} | {} 条提示词",
            self.profile_id, self.created_at_utc, self.prompt_count
        )
    }
}

#[derive(Debug, Clone)]
pub struct SpeakerProfileStore {
    root: PathBuf,
}

impl SpeakerProfileStore {
    pub fn new() -> Result<Self> {
        let root = PathBuf::from("profiles");
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create profiles directory: {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn default_profile_path(&self) -> PathBuf {
        self.root.join(DEFAULT_PROFILE_ID).join(PROFILE_FILENAME)
    }

    pub fn load_default_profile_summary(&self) -> Result<Option<ProfileSummary>> {
        let path = self.default_profile_path();
        if !path.exists() {
            return Ok(None);
        }

        let profile = self
            .load_from_path(&path)
            .with_context(|| format!("failed to load default profile summary: {}", path.display()))?;

        Ok(Some(ProfileSummary {
            profile_id: profile.profile_id,
            created_at_utc: profile.created_at_utc,
            suggested_threshold: profile.suggested_threshold,
            prompt_count: profile.prompt_count,
        }))
    }

    pub fn load_default(&self) -> Result<SpeakerProfile> {
        let path = self.default_profile_path();
        self.load_from_path(&path)
    }

    pub fn save_default(&self, profile: &SpeakerProfile) -> Result<()> {
        let profile_dir = self.root.join(DEFAULT_PROFILE_ID);
        fs::create_dir_all(&profile_dir).with_context(|| {
            format!("failed to create profile directory: {}", profile_dir.display())
        })?;

        let path = profile_dir.join(PROFILE_FILENAME);
        let json = serde_json::to_string_pretty(profile).context("failed to serialize speaker profile")?;
        fs::write(&path, json)
            .with_context(|| format!("failed to write profile file: {}", path.display()))?;

        Ok(())
    }

    fn load_from_path(&self, path: &Path) -> Result<SpeakerProfile> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read profile file: {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse profile file: {}", path.display()))
    }
}
