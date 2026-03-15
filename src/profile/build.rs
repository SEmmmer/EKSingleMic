use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::{
    ml::speaker::SpeakerEngine,
    profile::{
        quality::QualityReport,
        record::{
            EnrollmentScript, FREE_SPEECH_SECONDS, TrainingRecordingManifest,
            source_recordings_from_manifest,
        },
        storage::{DEFAULT_PROFILE_ID, SpeakerProfile},
    },
    util::time::current_utc_timestamp_rfc3339,
};

#[derive(Debug, Default)]
pub struct SpeakerProfileBuilder;

impl SpeakerProfileBuilder {
    pub fn build_default(
        manifest: &TrainingRecordingManifest,
        quality_report: &QualityReport,
        enrollment_script: &EnrollmentScript,
    ) -> Result<SpeakerProfile> {
        ensure!(
            enrollment_script.prompts.len() == manifest.fixed_prompts.len(),
            "prompt count mismatch between enrollment script and training manifest"
        );

        let ambient = manifest
            .ambient_silence
            .as_ref()
            .context("cannot build default profile without ambient silence recording")?;
        manifest
            .free_speech
            .as_ref()
            .context("cannot build default profile without free speech recording")?;
        ensure!(
            manifest.recorded_prompt_count() == manifest.fixed_prompts.len(),
            "cannot build default profile until all fixed prompts are recorded"
        );

        let source_recordings = source_recordings_from_manifest(manifest);
        ensure!(
            source_recordings.len() == manifest.fixed_prompts.len() + 2,
            "cannot build default profile while some recordings are still missing"
        );

        let raw_audio_path = Path::new(&ambient.relative_path)
            .parent()
            .map(normalize_relative_path);
        let embeddings = SpeakerEngine::extract_manifest_embeddings(
            manifest,
            quality_report.speech_activity_threshold_dbfs,
        )?;
        let aggregated = SpeakerEngine::aggregate_embeddings(&embeddings)?;

        Ok(SpeakerProfile {
            profile_id: DEFAULT_PROFILE_ID.to_owned(),
            created_at_utc: current_utc_timestamp_rfc3339()?,
            model_version: aggregated.model_version.to_owned(),
            embedding_count: aggregated.embeddings.len(),
            embedding_dimension: Some(aggregated.centroid.len()),
            centroid: aggregated.centroid,
            dispersion: Some(aggregated.dispersion),
            suggested_threshold: aggregated.suggested_threshold,
            prompt_locale: enrollment_script.locale.to_owned(),
            prompt_count: enrollment_script.prompts.len(),
            free_speech_seconds: Some(FREE_SPEECH_SECONDS),
            raw_audio_path,
            cleaned_audio_path: None,
            speech_activity_threshold_dbfs: quality_report.speech_activity_threshold_dbfs,
            quality_severity: quality_report.severity().label().to_owned(),
            quality_warning_count: quality_report.warning_count(),
            quality_error_count: quality_report.error_count(),
            source_recordings,
        })
    }
}

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::SpeakerProfileBuilder;
    use crate::profile::{
        quality::{QualityIssue, QualityReport, QualitySeverity},
        record::{EnrollmentScript, RecordedClip, RecordingTakeKind, TrainingRecordingManifest},
    };

    fn dummy_clip(
        kind: RecordingTakeKind,
        relative_path: &Path,
        duration_seconds: f32,
    ) -> RecordedClip {
        let sample_rate_hz = 16_000;
        let sample_count = (sample_rate_hz as f32 * duration_seconds) as usize;
        RecordedClip {
            kind,
            label: kind.label(),
            relative_path: relative_path.to_string_lossy().replace('\\', "/"),
            duration_seconds,
            sample_rate_hz,
            sample_count,
            peak_linear: 0.3,
        }
    }

    #[test]
    fn builder_emits_embedding_ready_default_profile() {
        let root = unique_test_root();
        let recordings_dir = root.join("profiles").join("default").join("recordings");
        fs::create_dir_all(&recordings_dir).expect("recordings dir should exist");
        let ambient_path = recordings_dir.join("ambient_silence.wav");
        let prompt_1_path = recordings_dir.join("fixed_prompt_01.wav");
        let prompt_2_path = recordings_dir.join("fixed_prompt_02.wav");
        let free_path = recordings_dir.join("free_speech.wav");
        write_constant_wav(&ambient_path, 16_000, 0.0, 5.0);
        write_voiced_wav(&prompt_1_path, 16_000, 215.0, 1.2);
        write_voiced_wav(&prompt_2_path, 16_000, 205.0, 1.2);
        write_voiced_wav(&free_path, 16_000, 210.0, 3.0);

        let script = EnrollmentScript {
            locale: "zh-CN",
            prompts: vec!["一句".to_owned(), "两句".to_owned()],
        };
        let mut manifest = TrainingRecordingManifest::new(script.prompts.len());
        manifest.register(dummy_clip(
            RecordingTakeKind::AmbientSilence,
            &ambient_path,
            5.0,
        ));
        manifest.register(dummy_clip(
            RecordingTakeKind::FixedPrompt { index: 0 },
            &prompt_1_path,
            1.2,
        ));
        manifest.register(dummy_clip(
            RecordingTakeKind::FixedPrompt { index: 1 },
            &prompt_2_path,
            1.2,
        ));
        manifest.register(dummy_clip(RecordingTakeKind::FreeSpeech, &free_path, 3.0));

        let report = QualityReport {
            expected_prompt_count: 2,
            recorded_prompt_count: 2,
            ambient_rms_dbfs: Some(-58.0),
            speech_activity_threshold_dbfs: -42.0,
            total_active_speech_seconds: 18.0,
            clip_reports: Vec::new(),
            issues: vec![QualityIssue {
                severity: QualitySeverity::Warning,
                message: "测试提醒".to_owned(),
            }],
        };

        let profile = SpeakerProfileBuilder::build_default(&manifest, &report, &script)
            .expect("builder should succeed for complete manifest");
        let _ = fs::remove_dir_all(root);

        assert_eq!(profile.profile_id, "default");
        assert_eq!(profile.model_version, "heuristic-speaker-embedding-v0");
        assert_eq!(profile.embedding_count, 3);
        assert_eq!(profile.prompt_count, 2);
        assert_eq!(profile.quality_warning_count, 1);
        assert_eq!(profile.quality_error_count, 0);
        assert!(profile.embedding_dimension.is_some());
        assert!(!profile.centroid.is_empty());
        assert!(profile.dispersion.is_some());
        assert!(profile.suggested_threshold > 0.0);
        assert_eq!(profile.source_recordings.len(), 4);
    }

    #[test]
    fn builder_rejects_incomplete_manifest() {
        let script = EnrollmentScript {
            locale: "zh-CN",
            prompts: vec!["一句".to_owned()],
        };
        let manifest = TrainingRecordingManifest::new(script.prompts.len());
        let report = QualityReport {
            expected_prompt_count: 1,
            recorded_prompt_count: 0,
            ambient_rms_dbfs: None,
            speech_activity_threshold_dbfs: -42.0,
            total_active_speech_seconds: 0.0,
            clip_reports: Vec::new(),
            issues: Vec::new(),
        };

        let error = SpeakerProfileBuilder::build_default(&manifest, &report, &script)
            .expect_err("builder should reject missing recordings");
        assert!(error.to_string().contains("ambient silence"));
    }

    fn unique_test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ek-single-mic-profile-build-{nonce}"))
    }

    fn write_constant_wav(path: &Path, sample_rate_hz: u32, sample: f32, duration_seconds: f32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sample_rate_hz,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("wav should create");
        let sample_count = (sample_rate_hz as f32 * duration_seconds) as usize;
        for _ in 0..sample_count {
            writer.write_sample(sample).expect("sample should write");
        }
        writer.finalize().expect("wav should finalize");
    }

    fn write_voiced_wav(
        path: &Path,
        sample_rate_hz: u32,
        frequency_hz: f32,
        duration_seconds: f32,
    ) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sample_rate_hz,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("wav should create");
        let sample_count = (sample_rate_hz as f32 * duration_seconds) as usize;

        for index in 0..sample_count {
            let time = index as f32 / sample_rate_hz as f32;
            let carrier = (2.0 * std::f32::consts::PI * frequency_hz * time).sin();
            let modulator = (2.0 * std::f32::consts::PI * 2.5 * time).sin() * 0.15 + 0.85;
            writer
                .write_sample((carrier * modulator * 0.25).clamp(-1.0, 1.0))
                .expect("sample should write");
        }

        writer.finalize().expect("wav should finalize");
    }
}
