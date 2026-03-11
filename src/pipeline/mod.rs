use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::{
    ml::{
        enhancement::EnhancementEngine,
        speaker::SpeakerEngine,
        vad::VadEngine,
    },
    pipeline::frames::AudioClip,
    profile::storage::{SpeakerProfile, SpeakerProfileStore},
    util::{
        audio_math::lerp,
        time::MODEL_SAMPLE_RATE,
    },
};

pub mod frames;
pub mod realtime;

const SIMILARITY_CONTEXT_SECONDS: f32 = 0.24;
const BACKGROUND_GAIN: f32 = 0.03;
const MIN_ACTIVE_GAIN: f32 = 0.08;
const KEPT_SPEECH_GAIN: f32 = 0.75;
const SUPPRESSED_SPEECH_GAIN: f32 = 0.20;

#[derive(Debug, Clone)]
pub struct OfflineBasicFilterMetrics {
    pub input_sample_rate_hz: u32,
    pub output_sample_rate_hz: u32,
    pub input_duration_seconds: f32,
    pub output_duration_seconds: f32,
    pub analyzed_frame_count: usize,
    pub active_frame_count: usize,
    pub kept_active_frame_count: usize,
    pub suppressed_active_frame_count: usize,
    pub mean_similarity: f32,
    pub min_similarity: f32,
    pub max_similarity: f32,
    pub operating_similarity_threshold: f32,
    pub average_frame_gain: f32,
}

#[derive(Debug, Clone)]
pub struct OfflineBasicFilterOutcome {
    pub output_clip: AudioClip,
    pub metrics: OfflineBasicFilterMetrics,
}

#[derive(Debug, Default)]
pub struct OfflineBasicFilterProcessor {
    enhancer: EnhancementEngine,
}

impl OfflineBasicFilterProcessor {
    pub fn process_default_profile_wav(
        &self,
        input_path: &Path,
        output_path: &Path,
    ) -> Result<OfflineBasicFilterMetrics> {
        let store = SpeakerProfileStore::new()?;
        let profile = store
            .load_default()
            .context("failed to load default speaker profile for offline basic filter")?;
        self.process_wav_file(&profile, input_path, output_path)
    }

    pub fn process_wav_file(
        &self,
        profile: &SpeakerProfile,
        input_path: &Path,
        output_path: &Path,
    ) -> Result<OfflineBasicFilterMetrics> {
        let input_clip = AudioClip::read_wav_mono(input_path)?;
        let outcome = self.process_clip(profile, &input_clip)?;
        outcome.output_clip.write_wav_mono(output_path)?;
        Ok(outcome.metrics)
    }

    pub fn process_clip(
        &self,
        profile: &SpeakerProfile,
        input_clip: &AudioClip,
    ) -> Result<OfflineBasicFilterOutcome> {
        ensure!(
            profile.embedding_count > 0 && !profile.centroid.is_empty(),
            "offline basic filter requires an embedding-ready speaker profile"
        );
        ensure!(
            profile.embedding_dimension == Some(profile.centroid.len()),
            "speaker profile centroid size does not match embedding_dimension"
        );

        let model_clip = input_clip.resample_to_model_rate();
        let vad = VadEngine::new(profile.speech_activity_threshold_dbfs);
        let decisions = vad.analyze(&model_clip.samples, model_clip.sample_rate_hz);
        ensure!(
            !decisions.is_empty(),
            "offline basic filter could not frame the input audio"
        );
        let operating_threshold = (
            profile.suggested_threshold
                + profile.dispersion.unwrap_or_default() * 2.0
                + 0.06
        )
            .clamp(0.70, 0.98);

        let context_radius = ((MODEL_SAMPLE_RATE as f32) * SIMILARITY_CONTEXT_SECONDS * 0.5)
            .round()
            .max(1.0) as usize;
        let mut desired_gains = Vec::with_capacity(decisions.len());
        let mut similarities = Vec::new();
        let mut active_frame_count = 0_usize;

        for decision in &decisions {
            if !decision.is_active {
                desired_gains.push(BACKGROUND_GAIN);
                continue;
            }

            active_frame_count += 1;
            let center = (decision.frame.start_sample + decision.frame.end_sample) / 2;
            let start = center.saturating_sub(context_radius);
            let end = (center + context_radius).min(model_clip.samples.len());
            let context = &model_clip.samples[start..end];

            let similarity = SpeakerEngine::extract_embedding_from_samples(
                context,
                model_clip.sample_rate_hz,
                vad.activity_threshold_dbfs(),
            )
            .map(|(embedding, _active_frames)| SpeakerEngine::match_score(&embedding, &profile.centroid))
            .unwrap_or(0.0);

            similarities.push(similarity);
            desired_gains.push(similarity_to_gain(similarity, operating_threshold));
        }

        let smoothed_gains = self.enhancer.smooth_gains(&desired_gains);
        let output_samples = self
            .enhancer
            .apply_frame_gains(&model_clip.samples, &decisions.iter().map(|decision| decision.frame).collect::<Vec<_>>(), &smoothed_gains);

        let kept_active_frame_count = decisions
            .iter()
            .zip(smoothed_gains.iter().copied())
            .filter(|(decision, gain)| decision.is_active && *gain >= KEPT_SPEECH_GAIN)
            .count();
        let suppressed_active_frame_count = decisions
            .iter()
            .zip(smoothed_gains.iter().copied())
            .filter(|(decision, gain)| decision.is_active && *gain <= SUPPRESSED_SPEECH_GAIN)
            .count();

        let mean_similarity = if similarities.is_empty() {
            0.0
        } else {
            similarities.iter().sum::<f32>() / similarities.len() as f32
        };
        let min_similarity = similarities.iter().copied().fold(1.0_f32, f32::min);
        let max_similarity = similarities.iter().copied().fold(0.0_f32, f32::max);
        let average_frame_gain = if smoothed_gains.is_empty() {
            0.0
        } else {
            smoothed_gains.iter().sum::<f32>() / smoothed_gains.len() as f32
        };

        Ok(OfflineBasicFilterOutcome {
            output_clip: AudioClip {
                sample_rate_hz: MODEL_SAMPLE_RATE,
                samples: output_samples,
            },
            metrics: OfflineBasicFilterMetrics {
                input_sample_rate_hz: input_clip.sample_rate_hz,
                output_sample_rate_hz: MODEL_SAMPLE_RATE,
                input_duration_seconds: input_clip.samples.len() as f32 / input_clip.sample_rate_hz as f32,
                output_duration_seconds: model_clip.samples.len() as f32 / MODEL_SAMPLE_RATE as f32,
                analyzed_frame_count: decisions.len(),
                active_frame_count,
                kept_active_frame_count,
                suppressed_active_frame_count,
                mean_similarity,
                min_similarity,
                max_similarity,
                operating_similarity_threshold: operating_threshold,
                average_frame_gain,
            },
        })
    }
}

fn similarity_to_gain(similarity: f32, threshold: f32) -> f32 {
    let reject_threshold = (threshold - 0.16).max(0.20);
    if similarity >= threshold {
        1.0
    } else if similarity <= reject_threshold {
        MIN_ACTIVE_GAIN
    } else {
        let t = (similarity - reject_threshold) / (threshold - reject_threshold).max(1e-4);
        lerp(MIN_ACTIVE_GAIN, 1.0, t.clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::OfflineBasicFilterProcessor;
    use crate::profile::{
        build::SpeakerProfileBuilder,
        quality::QualityReport,
        record::{RecordedClip, RecordingTakeKind, TrainingRecordingManifest},
    };
    use crate::util::{audio_math::rms_linear, time::MODEL_SAMPLE_RATE};

    #[test]
    fn offline_basic_filter_preserves_target_like_segments_more_than_off_target_segments() {
        let root = unique_test_root();
        let recordings_dir = root.join("profiles").join("default").join("recordings");
        fs::create_dir_all(&recordings_dir).expect("recordings dir should exist");

        let ambient_path = recordings_dir.join("ambient_silence.wav");
        let prompt_1_path = recordings_dir.join("fixed_prompt_01.wav");
        let prompt_2_path = recordings_dir.join("fixed_prompt_02.wav");
        let free_path = recordings_dir.join("free_speech.wav");
        write_constant_wav(&ambient_path, MODEL_SAMPLE_RATE, 0.0, 5.0);
        write_voiced_wav(&prompt_1_path, MODEL_SAMPLE_RATE, 210.0, 1.1);
        write_voiced_wav(&prompt_2_path, MODEL_SAMPLE_RATE, 220.0, 1.1);
        write_voiced_wav(&free_path, MODEL_SAMPLE_RATE, 215.0, 3.0);

        let mut manifest = TrainingRecordingManifest::new(2);
        manifest.register(test_clip(
            RecordingTakeKind::AmbientSilence,
            &ambient_path,
            5.0,
        ));
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 0 },
            &prompt_1_path,
            1.1,
        ));
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 1 },
            &prompt_2_path,
            1.1,
        ));
        manifest.register(test_clip(RecordingTakeKind::FreeSpeech, &free_path, 3.0));

        let profile = SpeakerProfileBuilder::build_default(
            &manifest,
            &QualityReport {
                expected_prompt_count: 2,
                recorded_prompt_count: 2,
                ambient_rms_dbfs: Some(-60.0),
                speech_activity_threshold_dbfs: -42.0,
                total_active_speech_seconds: 18.0,
                clip_reports: Vec::new(),
                issues: Vec::new(),
            },
            &crate::profile::record::EnrollmentScript {
                locale: "zh-CN",
                prompts: vec!["一".to_owned(), "二".to_owned()],
            },
        )
        .expect("profile build should succeed");

        let input_clip = crate::pipeline::frames::AudioClip {
            sample_rate_hz: MODEL_SAMPLE_RATE,
            samples: [
                synth_segment(MODEL_SAMPLE_RATE, 215.0, 0.8),
                synth_segment(MODEL_SAMPLE_RATE, 300.0, 0.8),
                synth_segment(MODEL_SAMPLE_RATE, 210.0, 0.8),
            ]
            .concat(),
        };

        let processor = OfflineBasicFilterProcessor::default();
        let outcome = processor
            .process_clip(&profile, &input_clip)
            .expect("offline processing should succeed");

        let segment_samples = (MODEL_SAMPLE_RATE as f32 * 0.8) as usize;
        let input_target_a = rms_linear(&input_clip.samples[0..segment_samples]);
        let input_intruder =
            rms_linear(&input_clip.samples[segment_samples..segment_samples * 2]);
        let input_target_b =
            rms_linear(&input_clip.samples[segment_samples * 2..segment_samples * 3]);
        let output_target_a = rms_linear(&outcome.output_clip.samples[0..segment_samples]);
        let output_intruder =
            rms_linear(&outcome.output_clip.samples[segment_samples..segment_samples * 2]);
        let output_target_b =
            rms_linear(&outcome.output_clip.samples[segment_samples * 2..segment_samples * 3]);

        let _ = fs::remove_dir_all(root);

        let target_retention = (output_target_a / input_target_a + output_target_b / input_target_b) * 0.5;
        let intruder_retention = output_intruder / input_intruder;
        assert!(
            target_retention > intruder_retention,
            "target_retention={target_retention:.4}, intruder_retention={intruder_retention:.4}, mean_similarity={:.4}, min_similarity={:.4}, max_similarity={:.4}, threshold={:.4}, avg_gain={:.4}",
            outcome.metrics.mean_similarity,
            outcome.metrics.min_similarity,
            outcome.metrics.max_similarity,
            outcome.metrics.operating_similarity_threshold,
            outcome.metrics.average_frame_gain
        );
        assert!(target_retention > 0.20);
        assert!(outcome.metrics.active_frame_count > 0);
    }

    fn test_clip(kind: RecordingTakeKind, path: &Path, duration_seconds: f32) -> RecordedClip {
        let sample_count = (MODEL_SAMPLE_RATE as f32 * duration_seconds) as usize;
        RecordedClip {
            kind,
            label: kind.label(),
            relative_path: path.to_string_lossy().replace('\\', "/"),
            duration_seconds,
            sample_rate_hz: MODEL_SAMPLE_RATE,
            sample_count,
            peak_linear: 0.25,
        }
    }

    fn unique_test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ek-single-mic-offline-basic-filter-{nonce}"))
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

    fn write_voiced_wav(path: &Path, sample_rate_hz: u32, frequency_hz: f32, duration_seconds: f32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sample_rate_hz,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("wav should create");
        for sample in synth_segment(sample_rate_hz, frequency_hz, duration_seconds) {
            writer.write_sample(sample).expect("sample should write");
        }
        writer.finalize().expect("wav should finalize");
    }

    fn synth_segment(sample_rate_hz: u32, frequency_hz: f32, duration_seconds: f32) -> Vec<f32> {
        let sample_count = (sample_rate_hz as f32 * duration_seconds) as usize;
        let mut samples = Vec::with_capacity(sample_count);

        for index in 0..sample_count {
            let time = index as f32 / sample_rate_hz as f32;
            let carrier = (2.0 * std::f32::consts::PI * frequency_hz * time).sin();
            let modulator = (2.0 * std::f32::consts::PI * 3.2 * time).sin() * 0.15 + 0.85;
            samples.push((carrier * modulator * 0.25).clamp(-1.0, 1.0));
        }

        samples
    }
}
