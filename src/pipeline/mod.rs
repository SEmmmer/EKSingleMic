use std::path::Path;

use anyhow::{Context, Result, ensure};

use crate::{
    ml::{enhancement::EnhancementEngine, speaker::SpeakerEngine, vad::VadEngine},
    pipeline::frames::AudioClip,
    profile::storage::{SpeakerProfile, SpeakerProfileStore},
    util::{audio_math::lerp, time::MODEL_SAMPLE_RATE},
};

pub mod frames;
pub mod realtime;

const SIMILARITY_CONTEXT_SECONDS: f32 = 0.40;
const BACKGROUND_GAIN: f32 = 0.03;
const MIN_ACTIVE_GAIN: f32 = 0.08;
const KEPT_SPEECH_GAIN: f32 = 0.75;
const SUPPRESSED_SPEECH_GAIN: f32 = 0.20;
const OPERATING_THRESHOLD_MARGIN: f32 = 0.04;
const MAX_OPERATING_SIMILARITY_THRESHOLD: f32 = 0.93;
const MIN_OPERATING_SIMILARITY_THRESHOLD: f32 = 0.72;
const SIMILARITY_TRANSITION_BAND: f32 = 0.08;
const TARGET_PRESENCE_ENTER_MARGIN: f32 = 0.005;
const TARGET_PRESENCE_EXIT_MARGIN: f32 = 0.09;
const TARGET_PRESENCE_HOLD_FRAMES: usize = 12;
const TARGET_PRESENCE_GAIN_FLOOR: f32 = 0.35;

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

#[derive(Debug, Clone, Copy, Default)]
pub struct BasicFilterChunkMetrics {
    pub analyzed_frame_count: usize,
    pub active_frame_count: usize,
    pub kept_active_frame_count: usize,
    pub suppressed_active_frame_count: usize,
    pub mean_similarity: f32,
    pub min_similarity: f32,
    pub max_similarity: f32,
    pub operating_similarity_threshold: f32,
    pub average_frame_gain: f32,
    pub latest_similarity: f32,
    pub latest_frame_gain: f32,
}

#[derive(Debug, Clone)]
pub struct BasicFilterChunkOutcome {
    pub output_samples: Vec<f32>,
    pub metrics: BasicFilterChunkMetrics,
}

#[derive(Debug, Clone)]
pub struct BasicFilterEngine {
    centroid: Vec<f32>,
    reference_embeddings: Vec<Vec<f32>>,
    operating_similarity_threshold: f32,
    vad: VadEngine,
    enhancer: EnhancementEngine,
    target_presence: TargetPresenceHold,
    previous_gain: Option<f32>,
}

#[derive(Debug, Default)]
pub struct OfflineBasicFilterProcessor;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TargetPresenceHold {
    enter_threshold: f32,
    exit_threshold: f32,
    hold_frames: usize,
    overlap_gain_floor: f32,
    remaining_hold_frames: usize,
}

impl TargetPresenceHold {
    fn new(operating_threshold: f32) -> Self {
        let reject_threshold = (operating_threshold - SIMILARITY_TRANSITION_BAND).max(0.20);
        Self {
            enter_threshold: (operating_threshold - TARGET_PRESENCE_ENTER_MARGIN)
                .max(reject_threshold),
            exit_threshold: (operating_threshold - TARGET_PRESENCE_EXIT_MARGIN)
                .max(reject_threshold),
            hold_frames: TARGET_PRESENCE_HOLD_FRAMES,
            overlap_gain_floor: TARGET_PRESENCE_GAIN_FLOOR.max(MIN_ACTIVE_GAIN),
            remaining_hold_frames: 0,
        }
    }

    fn update_active_frame(&mut self, similarity: f32, base_gain: f32) -> f32 {
        if similarity >= self.enter_threshold {
            self.remaining_hold_frames = self.hold_frames;
        } else if self.remaining_hold_frames > 0 && similarity < self.exit_threshold {
            self.remaining_hold_frames -= 1;
        }

        if self.remaining_hold_frames > 0 && similarity >= self.exit_threshold {
            let t = ((similarity - self.exit_threshold)
                / (self.enter_threshold - self.exit_threshold).max(1e-4))
            .clamp(0.0, 1.0);
            let dynamic_floor = lerp(MIN_ACTIVE_GAIN, self.overlap_gain_floor, t);
            base_gain.max(dynamic_floor)
        } else {
            base_gain
        }
    }

    fn update_inactive_frame(&mut self) {
        if self.remaining_hold_frames > 0 {
            self.remaining_hold_frames -= 1;
        }
    }
}

impl BasicFilterEngine {
    pub fn from_profile(profile: &SpeakerProfile) -> Result<Self> {
        ensure!(
            profile.embedding_count > 0 && !profile.centroid.is_empty(),
            "basic filter requires an embedding-ready speaker profile"
        );
        ensure!(
            profile.embedding_dimension == Some(profile.centroid.len()),
            "speaker profile centroid size does not match embedding_dimension"
        );

        let operating_similarity_threshold = operating_similarity_threshold(profile);
        let reference_embeddings = SpeakerEngine::extract_reference_embeddings(
            &profile.source_recordings,
            profile.speech_activity_threshold_dbfs,
        )
        .unwrap_or_default();

        Ok(Self {
            centroid: profile.centroid.clone(),
            reference_embeddings,
            operating_similarity_threshold,
            vad: VadEngine::new(profile.speech_activity_threshold_dbfs),
            enhancer: EnhancementEngine::default(),
            target_presence: TargetPresenceHold::new(operating_similarity_threshold),
            previous_gain: None,
        })
    }

    pub fn process_model_rate_samples(
        &mut self,
        samples: &[f32],
    ) -> Result<BasicFilterChunkOutcome> {
        let decisions = self.vad.analyze(samples, MODEL_SAMPLE_RATE);
        ensure!(
            !decisions.is_empty(),
            "basic filter could not frame the input audio"
        );

        let context_radius = ((MODEL_SAMPLE_RATE as f32) * SIMILARITY_CONTEXT_SECONDS * 0.5)
            .round()
            .max(1.0) as usize;
        let mut desired_gains = Vec::with_capacity(decisions.len());
        let mut similarities = Vec::new();
        let mut active_frame_count = 0_usize;

        for decision in &decisions {
            if !decision.is_active {
                self.target_presence.update_inactive_frame();
                desired_gains.push(BACKGROUND_GAIN);
                continue;
            }

            active_frame_count += 1;
            let center = (decision.frame.start_sample + decision.frame.end_sample) / 2;
            let start = center.saturating_sub(context_radius);
            let end = (center + context_radius).min(samples.len());
            let context = &samples[start..end];

            let similarity = SpeakerEngine::extract_embedding_from_samples(
                context,
                MODEL_SAMPLE_RATE,
                self.vad.activity_threshold_dbfs(),
            )
            .map(|(embedding, _active_frames)| {
                SpeakerEngine::profile_match_score(
                    &embedding,
                    &self.centroid,
                    &self.reference_embeddings,
                )
            })
            .unwrap_or(0.0);

            similarities.push(similarity);
            let base_gain = similarity_to_gain(similarity, self.operating_similarity_threshold);
            desired_gains.push(
                self.target_presence
                    .update_active_frame(similarity, base_gain),
            );
        }

        let smoothed_gains = if let Some(previous_gain) = self.previous_gain {
            self.enhancer
                .smooth_gains_from(&desired_gains, Some(previous_gain))
        } else {
            self.enhancer.smooth_gains(&desired_gains)
        };
        self.previous_gain = smoothed_gains.last().copied();

        let output_samples = self.enhancer.apply_frame_gains(
            samples,
            &decisions
                .iter()
                .map(|decision| decision.frame)
                .collect::<Vec<_>>(),
            &smoothed_gains,
        );

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
        let min_similarity = if similarities.is_empty() {
            0.0
        } else {
            similarities.iter().copied().fold(1.0_f32, f32::min)
        };
        let max_similarity = similarities.iter().copied().fold(0.0_f32, f32::max);
        let average_frame_gain = if smoothed_gains.is_empty() {
            0.0
        } else {
            smoothed_gains.iter().sum::<f32>() / smoothed_gains.len() as f32
        };

        Ok(BasicFilterChunkOutcome {
            output_samples,
            metrics: BasicFilterChunkMetrics {
                analyzed_frame_count: decisions.len(),
                active_frame_count,
                kept_active_frame_count,
                suppressed_active_frame_count,
                mean_similarity,
                min_similarity,
                max_similarity,
                operating_similarity_threshold: self.operating_similarity_threshold,
                average_frame_gain,
                latest_similarity: similarities.last().copied().unwrap_or(0.0),
                latest_frame_gain: smoothed_gains.last().copied().unwrap_or(0.0),
            },
        })
    }
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
        let model_clip = input_clip.resample_to_model_rate();
        let mut engine = BasicFilterEngine::from_profile(profile)?;
        let outcome = engine.process_model_rate_samples(&model_clip.samples)?;

        Ok(OfflineBasicFilterOutcome {
            output_clip: AudioClip {
                sample_rate_hz: MODEL_SAMPLE_RATE,
                samples: outcome.output_samples,
            },
            metrics: OfflineBasicFilterMetrics {
                input_sample_rate_hz: input_clip.sample_rate_hz,
                output_sample_rate_hz: MODEL_SAMPLE_RATE,
                input_duration_seconds: input_clip.samples.len() as f32
                    / input_clip.sample_rate_hz as f32,
                output_duration_seconds: model_clip.samples.len() as f32 / MODEL_SAMPLE_RATE as f32,
                analyzed_frame_count: outcome.metrics.analyzed_frame_count,
                active_frame_count: outcome.metrics.active_frame_count,
                kept_active_frame_count: outcome.metrics.kept_active_frame_count,
                suppressed_active_frame_count: outcome.metrics.suppressed_active_frame_count,
                mean_similarity: outcome.metrics.mean_similarity,
                min_similarity: outcome.metrics.min_similarity,
                max_similarity: outcome.metrics.max_similarity,
                operating_similarity_threshold: outcome.metrics.operating_similarity_threshold,
                average_frame_gain: outcome.metrics.average_frame_gain,
            },
        })
    }
}

fn operating_similarity_threshold(profile: &SpeakerProfile) -> f32 {
    (profile.suggested_threshold
        + profile.dispersion.unwrap_or_default()
        + OPERATING_THRESHOLD_MARGIN)
        .clamp(
            MIN_OPERATING_SIMILARITY_THRESHOLD,
            MAX_OPERATING_SIMILARITY_THRESHOLD,
        )
}

fn similarity_to_gain(similarity: f32, threshold: f32) -> f32 {
    let reject_threshold = (threshold - SIMILARITY_TRANSITION_BAND).max(0.20);
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

    use super::{
        MAX_OPERATING_SIMILARITY_THRESHOLD, MIN_ACTIVE_GAIN, OfflineBasicFilterProcessor,
        TARGET_PRESENCE_HOLD_FRAMES, TargetPresenceHold, operating_similarity_threshold,
    };
    use crate::profile::{
        build::SpeakerProfileBuilder,
        quality::QualityReport,
        record::{RecordedClip, RecordingTakeKind, TrainingRecordingManifest},
        storage::SpeakerProfile,
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
        let input_intruder = rms_linear(&input_clip.samples[segment_samples..segment_samples * 2]);
        let input_target_b =
            rms_linear(&input_clip.samples[segment_samples * 2..segment_samples * 3]);
        let output_target_a = rms_linear(&outcome.output_clip.samples[0..segment_samples]);
        let output_intruder =
            rms_linear(&outcome.output_clip.samples[segment_samples..segment_samples * 2]);
        let output_target_b =
            rms_linear(&outcome.output_clip.samples[segment_samples * 2..segment_samples * 3]);

        let _ = fs::remove_dir_all(root);

        let target_retention =
            (output_target_a / input_target_a + output_target_b / input_target_b) * 0.5;
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

    #[test]
    fn operating_threshold_stays_close_to_profile_suggestion() {
        let profile = SpeakerProfile {
            profile_id: "default".to_owned(),
            created_at_utc: "2026-03-15T00:00:00Z".to_owned(),
            model_version: "heuristic-speaker-embedding-v0".to_owned(),
            embedding_count: 11,
            embedding_dimension: Some(8),
            centroid: vec![0.0; 8],
            dispersion: Some(0.0114),
            suggested_threshold: 0.9001,
            prompt_locale: "zh-CN".to_owned(),
            prompt_count: 10,
            free_speech_seconds: Some(30),
            raw_audio_path: Some("profiles/default/recordings".to_owned()),
            cleaned_audio_path: None,
            speech_activity_threshold_dbfs: -42.0,
            quality_severity: "通过".to_owned(),
            quality_warning_count: 0,
            quality_error_count: 0,
            source_recordings: Vec::new(),
        };

        let threshold = operating_similarity_threshold(&profile);
        assert!(threshold > profile.suggested_threshold);
        assert!(threshold <= MAX_OPERATING_SIMILARITY_THRESHOLD);
        assert!(threshold < 0.96);
    }

    #[test]
    fn target_presence_hold_preserves_recent_target_frames() {
        let mut hold = TargetPresenceHold::new(0.93);
        let confident_gain = hold.update_active_frame(0.95, 1.0);
        assert_eq!(confident_gain, 1.0);

        let preserved_gain = hold.update_active_frame(0.88, MIN_ACTIVE_GAIN);
        assert!(preserved_gain > MIN_ACTIVE_GAIN);

        let low_similarity_gain = hold.update_active_frame(0.82, MIN_ACTIVE_GAIN);
        assert_eq!(low_similarity_gain, MIN_ACTIVE_GAIN);

        for _ in 0..TARGET_PRESENCE_HOLD_FRAMES {
            hold.update_inactive_frame();
        }

        let released_gain = hold.update_active_frame(0.82, MIN_ACTIVE_GAIN);
        assert_eq!(released_gain, MIN_ACTIVE_GAIN);
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
