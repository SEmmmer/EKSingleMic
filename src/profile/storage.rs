use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const PROFILE_FILENAME: &str = "speaker_profile.json";
pub const DEFAULT_PROFILE_ID: &str = "default";

fn default_quality_severity() -> String {
    "未知".to_owned()
}

fn default_speech_activity_threshold_dbfs() -> f32 {
    -42.0
}

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
    #[serde(default = "default_speech_activity_threshold_dbfs")]
    pub speech_activity_threshold_dbfs: f32,
    #[serde(default = "default_quality_severity")]
    pub quality_severity: String,
    #[serde(default)]
    pub quality_warning_count: usize,
    #[serde(default)]
    pub quality_error_count: usize,
    #[serde(default)]
    pub source_recordings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProfileSummary {
    pub profile_id: String,
    pub created_at_utc: String,
    pub model_version: String,
    pub embedding_count: usize,
    pub suggested_threshold: f32,
    pub prompt_count: usize,
    pub quality_severity: String,
    pub quality_warning_count: usize,
    pub quality_error_count: usize,
}

impl ProfileSummary {
    pub fn label(&self) -> String {
        format!(
            "{} | {} | {} 条提示词 | {} 个 embedding",
            self.profile_id, self.created_at_utc, self.prompt_count, self.embedding_count
        )
    }

    pub fn is_metadata_only(&self) -> bool {
        self.embedding_count == 0
    }
}

#[derive(Debug, Clone)]
pub struct SpeakerProfileStore {
    root: PathBuf,
}

impl SpeakerProfileStore {
    pub fn new() -> Result<Self> {
        Self::new_in(PathBuf::from("profiles"))
    }

    pub fn new_in(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
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
            model_version: profile.model_version,
            embedding_count: profile.embedding_count,
            suggested_threshold: profile.suggested_threshold,
            prompt_count: profile.prompt_count,
            quality_severity: profile.quality_severity,
            quality_warning_count: profile.quality_warning_count,
            quality_error_count: profile.quality_error_count,
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{DEFAULT_PROFILE_ID, SpeakerProfile, SpeakerProfileStore};

    fn unique_test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ek-single-mic-profile-store-{nonce}"))
    }

    #[test]
    fn save_and_reload_default_profile_roundtrip() {
        let root = unique_test_root();
        let store = SpeakerProfileStore::new_in(&root).expect("store should initialize");
        let profile = SpeakerProfile {
            profile_id: DEFAULT_PROFILE_ID.to_owned(),
            created_at_utc: "2026-03-12T12:00:00Z".to_owned(),
            model_version: "metadata-only-registration-v0".to_owned(),
            embedding_count: 0,
            embedding_dimension: None,
            centroid: Vec::new(),
            dispersion: None,
            suggested_threshold: 0.0,
            prompt_locale: "zh-CN".to_owned(),
            prompt_count: 10,
            free_speech_seconds: Some(30),
            raw_audio_path: Some("profiles/default/recordings".to_owned()),
            cleaned_audio_path: None,
            speech_activity_threshold_dbfs: -39.5,
            quality_severity: "警告".to_owned(),
            quality_warning_count: 2,
            quality_error_count: 1,
            source_recordings: vec![
                "profiles/default/recordings/ambient_silence.wav".to_owned(),
                "profiles/default/recordings/free_speech.wav".to_owned(),
            ],
        };

        store
            .save_default(&profile)
            .expect("default profile should save");

        let loaded = store.load_default().expect("saved profile should load");
        assert_eq!(loaded.profile_id, DEFAULT_PROFILE_ID);
        assert_eq!(loaded.model_version, "metadata-only-registration-v0");
        assert_eq!(loaded.quality_error_count, 1);
        assert_eq!(loaded.speech_activity_threshold_dbfs, -39.5);

        let summary = store
            .load_default_profile_summary()
            .expect("summary load should succeed")
            .expect("summary should exist");
        assert!(summary.is_metadata_only());
        assert_eq!(summary.embedding_count, 0);
        assert_eq!(summary.quality_warning_count, 2);
        assert_eq!(summary.quality_severity, "警告");

        let _ = fs::remove_dir_all(root);
    }
}
