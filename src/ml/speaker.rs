use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};

use crate::{
    profile::record::{RecordedClip, RecordingTakeKind, TrainingRecordingManifest},
    util::audio_math::{dbfs_from_linear, rms_linear},
};

const FRAME_WINDOW_SECONDS: f32 = 0.025;
const FRAME_HOP_SECONDS: f32 = 0.010;
const MIN_PITCH_HZ: f32 = 80.0;
const MAX_PITCH_HZ: f32 = 320.0;
const MIN_ACTIVE_FRAME_COUNT: usize = 3;
pub const HEURISTIC_SPEAKER_MODEL_VERSION: &str = "heuristic-speaker-embedding-v0";
pub const HEURISTIC_SPEAKER_EMBEDDING_DIMENSION: usize = 8;
const ZERO_CROSSING_MEAN_INDEX: usize = 0;
const SLOPE_RATIO_MEAN_INDEX: usize = 2;
const NORMALIZED_PITCH_MEAN_INDEX: usize = 6;
const REFERENCE_SUPPORT_BLEND: f32 = 0.60;
const STRONGEST_REFERENCE_WEIGHT: f32 = 0.75;
const TOP_REFERENCE_MEAN_WEIGHT: f32 = 0.25;
const TOP_REFERENCE_COUNT: usize = 3;

#[derive(Debug, Clone)]
pub struct SpeakerEmbedding {
    pub kind: RecordingTakeKind,
    pub vector: Vec<f32>,
    pub active_frame_count: usize,
}

#[derive(Debug, Clone)]
pub struct AggregatedSpeakerProfile {
    pub model_version: &'static str,
    pub embeddings: Vec<SpeakerEmbedding>,
    pub centroid: Vec<f32>,
    pub dispersion: f32,
    pub suggested_threshold: f32,
}

#[derive(Debug, Clone, Copy)]
struct FrameFeatures {
    zero_crossing_rate: f32,
    slope_ratio: f32,
    pitch_strength: f32,
    normalized_pitch: f32,
}

#[derive(Debug, Clone)]
struct FrameObservation {
    rms_dbfs: f32,
    features: FrameFeatures,
}

#[derive(Debug, Default)]
pub struct SpeakerEngine;

impl SpeakerEngine {
    pub fn profile_match_score(
        embedding: &[f32],
        centroid: &[f32],
        reference_embeddings: &[Vec<f32>],
    ) -> f32 {
        profile_embedding_match_score(embedding, centroid, reference_embeddings)
    }

    pub fn extract_embedding_from_samples(
        samples: &[f32],
        sample_rate_hz: u32,
        activity_threshold_dbfs: f32,
    ) -> Result<(Vec<f32>, usize)> {
        extract_embedding_vector(samples, sample_rate_hz, activity_threshold_dbfs)
    }

    pub fn extract_manifest_embeddings(
        manifest: &TrainingRecordingManifest,
        activity_threshold_dbfs: f32,
    ) -> Result<Vec<SpeakerEmbedding>> {
        let mut embeddings = Vec::with_capacity(manifest.fixed_prompts.len() + 1);

        for clip in manifest.fixed_prompts.iter().flatten() {
            embeddings.push(Self::extract_clip_embedding(clip, activity_threshold_dbfs)?);
        }

        if let Some(clip) = &manifest.free_speech {
            embeddings.push(Self::extract_clip_embedding(clip, activity_threshold_dbfs)?);
        }

        ensure!(
            !embeddings.is_empty(),
            "speaker embedding extraction requires at least one speech recording"
        );

        Ok(embeddings)
    }

    pub fn extract_reference_embeddings(
        source_recordings: &[String],
        activity_threshold_dbfs: f32,
    ) -> Result<Vec<Vec<f32>>> {
        let mut embeddings = Vec::new();

        for recording_path in source_recordings {
            if is_ambient_recording_path(recording_path) {
                continue;
            }

            let (samples, sample_rate_hz) =
                read_mono_wav_with_sample_rate(Path::new(recording_path)).with_context(|| {
                format!(
                    "failed to open source recording for reference embedding extraction: {recording_path}"
                )
            })?;
            let (embedding, _active_frame_count) = extract_embedding_vector(
                &samples,
                sample_rate_hz,
                activity_threshold_dbfs,
            )
            .with_context(|| {
                format!(
                    "failed to extract reference embedding from source recording: {recording_path}"
                )
            })?;
            embeddings.push(embedding);
        }

        ensure!(
            !embeddings.is_empty(),
            "reference embedding extraction requires at least one speech source recording"
        );

        Ok(embeddings)
    }

    pub fn aggregate_embeddings(
        embeddings: &[SpeakerEmbedding],
    ) -> Result<AggregatedSpeakerProfile> {
        ensure!(
            !embeddings.is_empty(),
            "cannot aggregate empty speaker embedding set"
        );
        ensure!(
            embeddings
                .iter()
                .all(|embedding| !matches!(embedding.kind, RecordingTakeKind::AmbientSilence)),
            "ambient silence embeddings cannot participate in speaker profile aggregation"
        );

        let dimension = embeddings[0].vector.len();
        ensure!(
            dimension == HEURISTIC_SPEAKER_EMBEDDING_DIMENSION,
            "unexpected speaker embedding dimension {dimension}"
        );
        ensure!(
            embeddings
                .iter()
                .all(|embedding| embedding.vector.len() == dimension),
            "speaker embedding dimensions must match before aggregation"
        );

        let mut centroid = vec![0.0_f32; dimension];
        let mut total_weight = 0.0_f32;
        for embedding in embeddings {
            let weight = embedding.active_frame_count.max(1) as f32;
            for (slot, value) in centroid.iter_mut().zip(&embedding.vector) {
                *slot += *value * weight;
            }
            total_weight += weight;
        }

        for slot in &mut centroid {
            *slot /= total_weight.max(1.0);
        }
        normalize_vector(&mut centroid);

        let similarities = embeddings
            .iter()
            .map(|embedding| embedding_match_score(&embedding.vector, &centroid))
            .collect::<Vec<_>>();
        let mean_similarity = similarities.iter().sum::<f32>() / similarities.len() as f32;
        let min_similarity = similarities.iter().copied().fold(f32::INFINITY, f32::min);
        let similarity_std = std_dev(&similarities, mean_similarity);
        let dispersion = similarities
            .iter()
            .map(|similarity| 1.0 - similarity)
            .sum::<f32>()
            / similarities.len() as f32;
        let suggested_threshold =
            (min_similarity - similarity_std.max(0.04) - 0.03).clamp(0.45, 0.95);

        Ok(AggregatedSpeakerProfile {
            model_version: HEURISTIC_SPEAKER_MODEL_VERSION,
            embeddings: embeddings.to_vec(),
            centroid,
            dispersion: dispersion.max(0.0),
            suggested_threshold: suggested_threshold.min(mean_similarity),
        })
    }

    fn extract_clip_embedding(
        clip: &RecordedClip,
        activity_threshold_dbfs: f32,
    ) -> Result<SpeakerEmbedding> {
        ensure!(
            !matches!(clip.kind, RecordingTakeKind::AmbientSilence),
            "ambient silence recording cannot be used for speaker embedding extraction"
        );

        let samples = read_mono_wav(Path::new(&clip.relative_path))
            .with_context(|| format!("failed to open speech clip WAV: {}", clip.relative_path))?;
        let (vector, active_frame_count) =
            extract_embedding_vector(&samples, clip.sample_rate_hz, activity_threshold_dbfs)
                .with_context(|| {
                    format!(
                        "failed to extract speaker embedding from {}",
                        clip.relative_path
                    )
                })?;

        Ok(SpeakerEmbedding {
            kind: clip.kind,
            vector,
            active_frame_count,
        })
    }
}

fn extract_embedding_vector(
    samples: &[f32],
    sample_rate_hz: u32,
    activity_threshold_dbfs: f32,
) -> Result<(Vec<f32>, usize)> {
    ensure!(
        sample_rate_hz > 0,
        "speaker embedding sample rate must be positive"
    );

    let frame_size = ((sample_rate_hz as f32) * FRAME_WINDOW_SECONDS)
        .round()
        .max(64.0) as usize;
    let hop_size = ((sample_rate_hz as f32) * FRAME_HOP_SECONDS)
        .round()
        .max(32.0) as usize;

    ensure!(
        samples.len() >= frame_size,
        "speech clip is too short to extract speaker embedding"
    );

    let observations = collect_frame_observations(samples, sample_rate_hz, frame_size, hop_size);
    ensure!(
        !observations.is_empty(),
        "no frame observations available for speaker embedding extraction"
    );

    let mut selected = observations
        .iter()
        .filter(|observation| observation.rms_dbfs >= activity_threshold_dbfs)
        .cloned()
        .collect::<Vec<_>>();

    if selected.len() < MIN_ACTIVE_FRAME_COUNT {
        let mut fallback = observations.clone();
        fallback.sort_by(|left, right| right.rms_dbfs.total_cmp(&left.rms_dbfs));
        selected = fallback
            .into_iter()
            .take(MIN_ACTIVE_FRAME_COUNT.min(observations.len()))
            .collect();
    }

    ensure!(
        !selected.is_empty(),
        "no active speech frames available for speaker embedding extraction"
    );

    let vector = aggregate_frame_features(&selected);
    Ok((vector, selected.len()))
}

fn collect_frame_observations(
    samples: &[f32],
    sample_rate_hz: u32,
    frame_size: usize,
    hop_size: usize,
) -> Vec<FrameObservation> {
    let mut observations = Vec::new();

    for start in (0..=samples.len() - frame_size).step_by(hop_size.max(1)) {
        let frame = &samples[start..start + frame_size];
        let rms_linear = rms_linear(frame);
        let rms_dbfs = dbfs_from_linear(rms_linear);
        let features = compute_frame_features(frame, sample_rate_hz, rms_linear);
        observations.push(FrameObservation { rms_dbfs, features });
    }

    observations
}

fn aggregate_frame_features(observations: &[FrameObservation]) -> Vec<f32> {
    let zero_crossing = observations
        .iter()
        .map(|observation| observation.features.zero_crossing_rate)
        .collect::<Vec<_>>();
    let slope_ratio = observations
        .iter()
        .map(|observation| observation.features.slope_ratio)
        .collect::<Vec<_>>();
    let pitch_strength = observations
        .iter()
        .map(|observation| observation.features.pitch_strength)
        .collect::<Vec<_>>();
    let normalized_pitch = observations
        .iter()
        .map(|observation| observation.features.normalized_pitch)
        .collect::<Vec<_>>();

    let mut vector = vec![
        mean(&zero_crossing),
        std_dev(&zero_crossing, mean(&zero_crossing)),
        mean(&slope_ratio),
        std_dev(&slope_ratio, mean(&slope_ratio)),
        mean(&pitch_strength),
        std_dev(&pitch_strength, mean(&pitch_strength)),
        mean(&normalized_pitch),
        std_dev(&normalized_pitch, mean(&normalized_pitch)),
    ];
    normalize_vector(&mut vector);
    vector
}

fn compute_frame_features(frame: &[f32], sample_rate_hz: u32, rms_linear: f32) -> FrameFeatures {
    let zero_crossing_rate = zero_crossing_rate(frame);
    let slope_ratio = spectral_slope_proxy(frame, rms_linear);
    let (pitch_strength, normalized_pitch) = pitch_features(frame, sample_rate_hz);

    FrameFeatures {
        zero_crossing_rate,
        slope_ratio,
        pitch_strength,
        normalized_pitch,
    }
}

fn zero_crossing_rate(frame: &[f32]) -> f32 {
    if frame.len() < 2 {
        return 0.0;
    }

    let changes = frame
        .windows(2)
        .filter(|pair| (pair[0] >= 0.0 && pair[1] < 0.0) || (pair[0] < 0.0 && pair[1] >= 0.0))
        .count();
    changes as f32 / (frame.len() - 1) as f32
}

fn spectral_slope_proxy(frame: &[f32], rms_linear: f32) -> f32 {
    if frame.len() < 2 {
        return 0.0;
    }

    let slope = frame
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .sum::<f32>()
        / (frame.len() - 1) as f32;
    (slope / rms_linear.max(1e-4)).clamp(0.0, 8.0)
}

fn pitch_features(frame: &[f32], sample_rate_hz: u32) -> (f32, f32) {
    let min_lag = ((sample_rate_hz as f32) / MAX_PITCH_HZ).floor().max(1.0) as usize;
    let max_lag = ((sample_rate_hz as f32) / MIN_PITCH_HZ)
        .ceil()
        .max(min_lag as f32) as usize;

    if frame.len() <= max_lag + 1 {
        return (0.0, 0.0);
    }

    let mut best_strength = 0.0_f32;
    let mut best_lag = min_lag;

    for lag in min_lag..=max_lag {
        let mut numerator = 0.0_f32;
        let mut left_energy = 0.0_f32;
        let mut right_energy = 0.0_f32;

        for index in 0..(frame.len() - lag) {
            let left = frame[index];
            let right = frame[index + lag];
            numerator += left * right;
            left_energy += left * left;
            right_energy += right * right;
        }

        let denominator = (left_energy * right_energy).sqrt().max(1e-6);
        let strength = (numerator / denominator).max(0.0);
        if strength > best_strength {
            best_strength = strength;
            best_lag = lag;
        }
    }

    if best_strength <= 0.0 {
        return (0.0, 0.0);
    }

    let pitch_hz = sample_rate_hz as f32 / best_lag as f32;
    let normalized_pitch =
        ((pitch_hz - MIN_PITCH_HZ) / (MAX_PITCH_HZ - MIN_PITCH_HZ)).clamp(0.0, 1.0);
    (best_strength.clamp(0.0, 1.0), normalized_pitch)
}

fn mean(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f32>() / values.len() as f32
}

fn std_dev(values: &[f32], mean: f32) -> f32 {
    if values.len() < 2 {
        return 0.0;
    }

    let variance = values
        .iter()
        .map(|value| {
            let delta = *value - mean;
            delta * delta
        })
        .sum::<f32>()
        / values.len() as f32;
    variance.sqrt()
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt()
        .max(1e-6);
    for value in vector {
        *value /= norm;
    }
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn embedding_match_score(left: &[f32], right: &[f32]) -> f32 {
    let cosine = cosine_similarity(left, right);
    let pitch_penalty =
        (left[NORMALIZED_PITCH_MEAN_INDEX] - right[NORMALIZED_PITCH_MEAN_INDEX]).abs() * 0.45;
    let zcr_penalty =
        (left[ZERO_CROSSING_MEAN_INDEX] - right[ZERO_CROSSING_MEAN_INDEX]).abs() * 0.20;
    let slope_penalty = (left[SLOPE_RATIO_MEAN_INDEX] - right[SLOPE_RATIO_MEAN_INDEX]).abs() * 0.15;
    (cosine - pitch_penalty - zcr_penalty - slope_penalty).clamp(0.0, 1.0)
}

fn profile_embedding_match_score(
    embedding: &[f32],
    centroid: &[f32],
    reference_embeddings: &[Vec<f32>],
) -> f32 {
    let centroid_score = embedding_match_score(embedding, centroid);
    let reference_support = reference_support_score(embedding, reference_embeddings);
    if reference_support <= 0.0 {
        return centroid_score;
    }

    (centroid_score * (1.0 - REFERENCE_SUPPORT_BLEND) + reference_support * REFERENCE_SUPPORT_BLEND)
        .clamp(0.0, 1.0)
}

fn reference_support_score(embedding: &[f32], reference_embeddings: &[Vec<f32>]) -> f32 {
    let mut scores = reference_embeddings
        .iter()
        .filter(|reference| reference.len() == embedding.len())
        .map(|reference| embedding_match_score(embedding, reference))
        .collect::<Vec<_>>();

    if scores.is_empty() {
        return 0.0;
    }

    scores.sort_by(|left, right| right.total_cmp(left));
    let strongest = scores[0];
    let top_count = TOP_REFERENCE_COUNT.min(scores.len());
    let top_mean = scores.iter().take(top_count).sum::<f32>() / top_count as f32;

    (strongest * STRONGEST_REFERENCE_WEIGHT + top_mean * TOP_REFERENCE_MEAN_WEIGHT).clamp(0.0, 1.0)
}

fn is_ambient_recording_path(path: &str) -> bool {
    path.to_ascii_lowercase().contains("ambient_silence")
}

fn read_mono_wav(path: &Path) -> Result<Vec<f32>> {
    let (samples, _sample_rate_hz) = read_mono_wav_with_sample_rate(path)?;
    Ok(samples)
}

fn read_mono_wav_with_sample_rate(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to create speaker WAV reader: {}", path.display()))?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1,
        "speaker embedding extractor only supports mono WAV input, got {} channels at {}",
        spec.channels,
        path.display()
    );

    let samples = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| {
                sample
                    .map(|value| value.clamp(-1.0, 1.0))
                    .map_err(|error| anyhow!("failed to decode speaker WAV sample: {error}"))
            })
            .collect::<Result<Vec<_>, _>>(),
        hound::SampleFormat::Int => {
            ensure!(
                spec.bits_per_sample > 0 && spec.bits_per_sample <= 32,
                "unsupported speaker WAV bits per sample {} at {}",
                spec.bits_per_sample,
                path.display()
            );
            let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| (value as f32 / scale).clamp(-1.0, 1.0))
                        .map_err(|error| anyhow!("failed to decode speaker WAV sample: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()
        }
    }?;

    Ok((samples, spec.sample_rate))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{HEURISTIC_SPEAKER_EMBEDDING_DIMENSION, SpeakerEngine};
    use crate::profile::record::{RecordedClip, RecordingTakeKind, TrainingRecordingManifest};

    #[test]
    fn extract_manifest_embeddings_returns_prompt_and_free_speech_vectors() {
        let root = unique_test_root();
        let recordings_dir = root.join("profiles").join("default").join("recordings");
        fs::create_dir_all(&recordings_dir).expect("recordings dir should exist");

        let prompt_path = recordings_dir.join("fixed_prompt_01.wav");
        let free_path = recordings_dir.join("free_speech.wav");
        write_sine_wav(&prompt_path, 16_000, 220.0, 1.2);
        write_sine_wav(&free_path, 16_000, 180.0, 3.0);

        let mut manifest = TrainingRecordingManifest::new(1);
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 0 },
            &prompt_path,
        ));
        manifest.register(test_clip(RecordingTakeKind::FreeSpeech, &free_path));

        let embeddings = SpeakerEngine::extract_manifest_embeddings(&manifest, -40.0)
            .expect("embedding extraction should succeed");
        let aggregated = SpeakerEngine::aggregate_embeddings(&embeddings)
            .expect("embedding aggregation should succeed");

        let _ = fs::remove_dir_all(root);

        assert_eq!(embeddings.len(), 2);
        assert!(
            embeddings
                .iter()
                .all(|embedding| embedding.active_frame_count > 0)
        );
        assert_eq!(aggregated.embeddings.len(), 2);
        assert_eq!(
            aggregated.centroid.len(),
            HEURISTIC_SPEAKER_EMBEDDING_DIMENSION
        );
        assert!(aggregated.suggested_threshold > 0.0);
    }

    #[test]
    fn extract_reference_embeddings_skips_ambient_silence_recordings() {
        let root = unique_test_root();
        let recordings_dir = root.join("profiles").join("default").join("recordings");
        fs::create_dir_all(&recordings_dir).expect("recordings dir should exist");

        let ambient_path = recordings_dir.join("ambient_silence.wav");
        let prompt_path = recordings_dir.join("fixed_prompt_01.wav");
        let free_path = recordings_dir.join("free_speech.wav");
        write_sine_wav(&ambient_path, 16_000, 220.0, 1.2);
        write_sine_wav(&prompt_path, 16_000, 220.0, 1.2);
        write_sine_wav(&free_path, 16_000, 180.0, 3.0);

        let embeddings = SpeakerEngine::extract_reference_embeddings(
            &[
                ambient_path.to_string_lossy().replace('\\', "/"),
                prompt_path.to_string_lossy().replace('\\', "/"),
                free_path.to_string_lossy().replace('\\', "/"),
            ],
            -40.0,
        )
        .expect("reference extraction should succeed");

        let _ = fs::remove_dir_all(root);

        assert_eq!(embeddings.len(), 2);
        assert!(
            embeddings
                .iter()
                .all(|embedding| embedding.len() == HEURISTIC_SPEAKER_EMBEDDING_DIMENSION)
        );
    }

    #[test]
    fn profile_match_score_uses_reference_support_when_available() {
        let centroid = vec![0.10, 0.05, 0.10, 0.10, 0.93, 0.24, 0.14, 0.12];
        let embedding = vec![0.02, 0.05, 0.08, 0.10, 0.91, 0.30, 0.16, 0.17];
        let references = vec![
            embedding.clone(),
            vec![0.03, 0.05, 0.09, 0.12, 0.89, 0.29, 0.18, 0.17],
            vec![0.04, 0.06, 0.10, 0.11, 0.90, 0.28, 0.17, 0.18],
        ];

        let centroid_only = SpeakerEngine::profile_match_score(&embedding, &centroid, &[]);
        let supported = SpeakerEngine::profile_match_score(&embedding, &centroid, &references);
        let fallback = SpeakerEngine::profile_match_score(&embedding, &centroid, &[]);

        assert!(supported > centroid_only);
        assert_eq!(fallback, centroid_only);
    }

    fn test_clip(kind: RecordingTakeKind, path: &Path) -> RecordedClip {
        let sample_rate_hz = 16_000;
        let sample_count = if matches!(kind, RecordingTakeKind::FreeSpeech) {
            sample_rate_hz as usize * 3
        } else {
            sample_rate_hz as usize
        };

        RecordedClip {
            kind,
            label: kind.label(),
            relative_path: path.to_string_lossy().replace('\\', "/"),
            duration_seconds: sample_count as f32 / sample_rate_hz as f32,
            sample_rate_hz,
            sample_count,
            peak_linear: 0.25,
        }
    }

    fn unique_test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ek-single-mic-speaker-test-{nonce}"))
    }

    fn write_sine_wav(path: &Path, sample_rate_hz: u32, frequency_hz: f32, duration_seconds: f32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sample_rate_hz,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("test wav should create");
        let sample_count = (sample_rate_hz as f32 * duration_seconds) as usize;

        for index in 0..sample_count {
            let time = index as f32 / sample_rate_hz as f32;
            let envelope =
                (time / duration_seconds).min(1.0) * (1.0 - (time / duration_seconds - 0.5).abs());
            let sample =
                (2.0 * std::f32::consts::PI * frequency_hz * time).sin() * 0.25 * envelope.max(0.2);
            writer
                .write_sample(sample)
                .expect("test sample should write");
        }

        writer.finalize().expect("test wav should finalize");
    }
}
