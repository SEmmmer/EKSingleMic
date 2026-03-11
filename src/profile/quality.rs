use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};

use crate::{
    profile::record::{RecordedClip, RecordingTakeKind, TrainingRecordingManifest},
    util::audio_math::dbfs_from_linear,
};

const MIN_AMBIENT_DURATION_SECONDS: f32 = 4.5;
const MIN_FIXED_PROMPT_DURATION_SECONDS: f32 = 0.8;
const MIN_FREE_SPEECH_DURATION_SECONDS: f32 = 24.0;
const MIN_FIXED_PROMPT_ACTIVE_SECONDS: f32 = 0.35;
const MIN_FREE_SPEECH_ACTIVE_SECONDS: f32 = 12.0;
const LOW_SPEECH_RMS_DBFS: f32 = -32.0;
const HIGH_AMBIENT_RMS_DBFS: f32 = -40.0;
const HIGH_AMBIENT_PEAK_DBFS: f32 = -6.0;
const ACTIVITY_THRESHOLD_FLOOR_DBFS: f32 = -42.0;
const ACTIVITY_THRESHOLD_CEILING_DBFS: f32 = -24.0;
const AMBIENT_ACTIVITY_MARGIN_DB: f32 = 10.0;
const FRAME_RATE_HZ: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QualitySeverity {
    Ok,
    Warning,
    Error,
}

impl QualitySeverity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "通过",
            Self::Warning => "警告",
            Self::Error => "错误",
        }
    }
}

#[derive(Debug, Clone)]
pub struct QualityIssue {
    pub severity: QualitySeverity,
    pub message: String,
}

impl QualityIssue {
    fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: QualitySeverity::Warning,
            message: message.into(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            severity: QualitySeverity::Error,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClipQualityReport {
    pub kind: RecordingTakeKind,
    pub label: String,
    pub relative_path: String,
    pub duration_seconds: f32,
    pub sample_rate_hz: u32,
    pub rms_dbfs: f32,
    pub active_seconds: f32,
    pub active_ratio: f32,
    pub activity_segment_count: usize,
    pub issues: Vec<QualityIssue>,
}

impl ClipQualityReport {
    pub fn severity(&self) -> QualitySeverity {
        highest_issue_severity(&self.issues)
    }
}

#[derive(Debug, Clone, Default)]
pub struct QualityReport {
    pub expected_prompt_count: usize,
    pub recorded_prompt_count: usize,
    pub ambient_rms_dbfs: Option<f32>,
    pub speech_activity_threshold_dbfs: f32,
    pub total_active_speech_seconds: f32,
    pub clip_reports: Vec<ClipQualityReport>,
    pub issues: Vec<QualityIssue>,
}

impl QualityReport {
    pub fn analyze_manifest(manifest: &TrainingRecordingManifest) -> Result<Self> {
        let expected_prompt_count = manifest.fixed_prompts.len();
        let recorded_prompt_count = manifest.recorded_prompt_count();
        let mut clip_reports = Vec::new();
        let mut issues = Vec::new();
        let mut ambient_rms_dbfs = None;

        if let Some(ambient_clip) = &manifest.ambient_silence {
            let ambient_report = analyze_clip(ambient_clip, ACTIVITY_THRESHOLD_FLOOR_DBFS)
                .with_context(|| {
                    format!("failed to analyze ambient recording: {}", ambient_clip.relative_path)
                })?;
            ambient_rms_dbfs = Some(ambient_report.rms_dbfs);
            clip_reports.push(ambient_report);
        } else {
            issues.push(QualityIssue::error("缺少环境静音录音。"));
        }

        let speech_activity_threshold_dbfs = speech_activity_threshold_from_ambient(ambient_rms_dbfs);
        let mut total_active_speech_seconds = 0.0_f32;

        for (index, clip) in manifest.fixed_prompts.iter().enumerate() {
            let Some(clip) = clip else {
                issues.push(QualityIssue::error(format!("缺少固定短句 {:02} 的录音。", index + 1)));
                continue;
            };

            let report = analyze_clip(clip, speech_activity_threshold_dbfs).with_context(|| {
                format!(
                    "failed to analyze fixed prompt recording {:02}: {}",
                    index + 1,
                    clip.relative_path
                )
            })?;
            total_active_speech_seconds += report.active_seconds;
            clip_reports.push(report);
        }

        if let Some(free_speech_clip) = &manifest.free_speech {
            let report = analyze_clip(free_speech_clip, speech_activity_threshold_dbfs)
                .with_context(|| {
                    format!(
                        "failed to analyze free speech recording: {}",
                        free_speech_clip.relative_path
                    )
                })?;
            total_active_speech_seconds += report.active_seconds;
            clip_reports.push(report);
        } else {
            issues.push(QualityIssue::error("缺少自由发挥录音。"));
        }

        let minimum_expected_active_seconds =
            expected_prompt_count as f32 * MIN_FIXED_PROMPT_ACTIVE_SECONDS + MIN_FREE_SPEECH_ACTIVE_SECONDS;
        if recorded_prompt_count == expected_prompt_count
            && manifest.free_speech.is_some()
            && total_active_speech_seconds < minimum_expected_active_seconds
        {
            issues.push(QualityIssue::warning(format!(
                "当前有效语音总时长仅 {:.1} 秒，低于建议值 {:.1} 秒。",
                total_active_speech_seconds, minimum_expected_active_seconds
            )));
        }

        Ok(Self {
            expected_prompt_count,
            recorded_prompt_count,
            ambient_rms_dbfs,
            speech_activity_threshold_dbfs,
            total_active_speech_seconds,
            clip_reports,
            issues,
        })
    }

    pub fn severity(&self) -> QualitySeverity {
        let clip_severity = self
            .clip_reports
            .iter()
            .map(ClipQualityReport::severity)
            .max()
            .unwrap_or(QualitySeverity::Ok);
        clip_severity.max(highest_issue_severity(&self.issues))
    }

    pub fn warning_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|issue| issue.severity == QualitySeverity::Warning)
            .count()
            + self
                .clip_reports
                .iter()
                .flat_map(|report| report.issues.iter())
                .filter(|issue| issue.severity == QualitySeverity::Warning)
                .count()
    }

    pub fn error_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|issue| issue.severity == QualitySeverity::Error)
            .count()
            + self
                .clip_reports
                .iter()
                .flat_map(|report| report.issues.iter())
                .filter(|issue| issue.severity == QualitySeverity::Error)
                .count()
    }
}

fn analyze_clip(clip: &RecordedClip, activity_threshold_dbfs: f32) -> Result<ClipQualityReport> {
    let samples = read_mono_wav(Path::new(&clip.relative_path))
        .with_context(|| format!("failed to open clip WAV: {}", clip.relative_path))?;
    ensure!(
        clip.sample_rate_hz > 0,
        "clip sample rate must be positive for {}",
        clip.relative_path
    );

    Ok(analyze_samples(clip, &samples, activity_threshold_dbfs))
}

fn analyze_samples(
    clip: &RecordedClip,
    samples: &[f32],
    activity_threshold_dbfs: f32,
) -> ClipQualityReport {
    let rms_linear = rms_linear(samples);
    let rms_dbfs = dbfs_from_linear(rms_linear);
    let peak_linear = peak_linear(samples);
    let peak_dbfs = dbfs_from_linear(peak_linear);
    let activity = analyze_activity(samples, clip.sample_rate_hz, activity_threshold_dbfs);

    let mut issues = Vec::new();
    populate_clip_issues(clip, rms_dbfs, peak_dbfs, &activity, &mut issues);

    ClipQualityReport {
        kind: clip.kind,
        label: clip.label.clone(),
        relative_path: clip.relative_path.clone(),
        duration_seconds: clip.duration_seconds,
        sample_rate_hz: clip.sample_rate_hz,
        rms_dbfs,
        active_seconds: activity.active_seconds,
        active_ratio: activity.active_ratio,
        activity_segment_count: activity.activity_segment_count,
        issues,
    }
}

fn populate_clip_issues(
    clip: &RecordedClip,
    rms_dbfs: f32,
    peak_dbfs: f32,
    activity: &ActivityMetrics,
    issues: &mut Vec<QualityIssue>,
) {
    match clip.kind {
        RecordingTakeKind::AmbientSilence => {
            if clip.duration_seconds < MIN_AMBIENT_DURATION_SECONDS {
                issues.push(QualityIssue::error(format!(
                    "环境静音录音只有 {:.1} 秒，短于要求的 5 秒。",
                    clip.duration_seconds
                )));
            }

            if rms_dbfs > HIGH_AMBIENT_RMS_DBFS {
                issues.push(QualityIssue::warning(format!(
                    "环境静音平均电平偏高（{:.1} dBFS），背景噪声可能较大。",
                    rms_dbfs
                )));
            }

            if peak_dbfs > HIGH_AMBIENT_PEAK_DBFS {
                issues.push(QualityIssue::warning(format!(
                    "环境静音里出现了明显瞬态峰值（{:.1} dBFS），请确认录制时没有说话或碰到麦克风。",
                    peak_dbfs
                )));
            }

            if activity.active_seconds > 0.25 || activity.activity_segment_count > 0 {
                issues.push(QualityIssue::warning(format!(
                    "环境静音中检测到约 {:.1} 秒的明显活动声，建议重录以获得更稳定的噪声基线。",
                    activity.active_seconds
                )));
            }
        }
        RecordingTakeKind::FixedPrompt { index } => {
            let insufficient_activity =
                activity.active_seconds < MIN_FIXED_PROMPT_ACTIVE_SECONDS
                    || activity.activity_segment_count == 0;

            if clip.duration_seconds < MIN_FIXED_PROMPT_DURATION_SECONDS {
                issues.push(QualityIssue::warning(format!(
                    "固定短句 {:02} 总时长只有 {:.1} 秒，可能录得过短。",
                    index + 1,
                    clip.duration_seconds
                )));
            }

            if insufficient_activity {
                issues.push(QualityIssue::error(format!(
                    "固定短句 {:02} 的有效语音不足（{:.2} 秒），建议重录。",
                    index + 1,
                    activity.active_seconds
                )));
            }

            if !insufficient_activity && rms_dbfs < LOW_SPEECH_RMS_DBFS {
                issues.push(QualityIssue::warning(format!(
                    "固定短句 {:02} 平均音量偏低（{:.1} dBFS）。",
                    index + 1,
                    rms_dbfs
                )));
            }
        }
        RecordingTakeKind::FreeSpeech => {
            let insufficient_activity = activity.active_seconds < MIN_FREE_SPEECH_ACTIVE_SECONDS;

            if clip.duration_seconds < MIN_FREE_SPEECH_DURATION_SECONDS {
                issues.push(QualityIssue::error(format!(
                    "自由发挥录音只有 {:.1} 秒，低于建议的 30 秒。",
                    clip.duration_seconds
                )));
            }

            if insufficient_activity {
                issues.push(QualityIssue::error(format!(
                    "自由发挥有效语音只有 {:.1} 秒，明显不足，建议重录。",
                    activity.active_seconds
                )));
            }

            if activity.activity_segment_count < 4 {
                issues.push(QualityIssue::warning(format!(
                    "自由发挥检测到的语音片段数量较少（{} 段），语速/停顿覆盖可能不够。",
                    activity.activity_segment_count
                )));
            }

            if !insufficient_activity && rms_dbfs < LOW_SPEECH_RMS_DBFS {
                issues.push(QualityIssue::warning(format!(
                    "自由发挥平均音量偏低（{:.1} dBFS）。",
                    rms_dbfs
                )));
            }
        }
    }
}

fn read_mono_wav(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to create WAV reader: {}", path.display()))?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1,
        "quality checker only supports mono WAV input, got {} channels at {}",
        spec.channels,
        path.display()
    );

    match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| {
                sample
                    .map(|value| value.clamp(-1.0, 1.0))
                    .map_err(|error| anyhow!("failed to decode WAV sample: {error}"))
            })
            .collect(),
        hound::SampleFormat::Int => {
            ensure!(
                spec.bits_per_sample > 0 && spec.bits_per_sample <= 32,
                "unsupported integer WAV bits per sample {} at {}",
                spec.bits_per_sample,
                path.display()
            );
            let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| (value as f32 / scale).clamp(-1.0, 1.0))
                        .map_err(|error| anyhow!("failed to decode WAV sample: {error}"))
                })
                .collect()
        }
    }
}

fn rms_linear(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let mean_square = samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
    mean_square.sqrt()
}

fn peak_linear(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

#[derive(Debug, Clone, Copy, Default)]
struct ActivityMetrics {
    active_seconds: f32,
    active_ratio: f32,
    activity_segment_count: usize,
}

fn analyze_activity(
    samples: &[f32],
    sample_rate_hz: u32,
    activity_threshold_dbfs: f32,
) -> ActivityMetrics {
    if samples.is_empty() || sample_rate_hz == 0 {
        return ActivityMetrics::default();
    }

    let frame_size = ((sample_rate_hz as usize) / FRAME_RATE_HZ as usize).max(1);
    let mut active_sample_count = 0usize;
    let mut activity_segment_count = 0usize;
    let mut previous_frame_active = false;

    for frame in samples.chunks(frame_size) {
        let frame_dbfs = dbfs_from_linear(rms_linear(frame));
        let is_active = frame_dbfs >= activity_threshold_dbfs;
        if is_active {
            active_sample_count += frame.len();
            if !previous_frame_active {
                activity_segment_count += 1;
            }
        }
        previous_frame_active = is_active;
    }

    ActivityMetrics {
        active_seconds: active_sample_count as f32 / sample_rate_hz as f32,
        active_ratio: active_sample_count as f32 / samples.len() as f32,
        activity_segment_count,
    }
}

fn speech_activity_threshold_from_ambient(ambient_rms_dbfs: Option<f32>) -> f32 {
    ambient_rms_dbfs
        .map(|value| (value + AMBIENT_ACTIVITY_MARGIN_DB).clamp(
            ACTIVITY_THRESHOLD_FLOOR_DBFS,
            ACTIVITY_THRESHOLD_CEILING_DBFS,
        ))
        .unwrap_or(ACTIVITY_THRESHOLD_FLOOR_DBFS)
}

fn highest_issue_severity(issues: &[QualityIssue]) -> QualitySeverity {
    issues
        .iter()
        .map(|issue| issue.severity)
        .max()
        .unwrap_or(QualitySeverity::Ok)
}

#[cfg(test)]
mod tests {
    use super::{QualityReport, QualitySeverity, analyze_samples};
    use crate::profile::record::{RecordedClip, RecordingTakeKind, TrainingRecordingManifest};

    #[test]
    fn quiet_complete_manifest_passes_basic_quality_check() {
        let mut manifest = TrainingRecordingManifest::new(2);
        manifest.register(test_clip(RecordingTakeKind::AmbientSilence, "ambient.wav", 5.0));
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 0 },
            "prompt01.wav",
            1.2,
        ));
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 1 },
            "prompt02.wav",
            1.3,
        ));
        manifest.register(test_clip(RecordingTakeKind::FreeSpeech, "free.wav", 30.0));

        let ambient = analyze_samples(
            manifest.ambient_silence.as_ref().unwrap(),
            &vec![0.0; 16_000 * 5],
            -42.0,
        );
        assert_eq!(ambient.severity(), QualitySeverity::Ok);

        let prompt = analyze_samples(
            manifest.fixed_prompts[0].as_ref().unwrap(),
            &alternating_speech_samples(16_000, 1.2, 0.08),
            -35.0,
        );
        assert_eq!(prompt.severity(), QualitySeverity::Ok);

        let free = analyze_samples(
            manifest.free_speech.as_ref().unwrap(),
            &alternating_speech_samples(16_000, 30.0, 0.08),
            -35.0,
        );
        assert_eq!(free.severity(), QualitySeverity::Ok);
    }

    #[test]
    fn low_activity_prompt_is_flagged_as_error() {
        let clip = test_clip(
            RecordingTakeKind::FixedPrompt { index: 0 },
            "prompt.wav",
            1.0,
        );
        let report = analyze_samples(&clip, &vec![0.0; 16_000], -35.0);

        assert_eq!(report.severity(), QualitySeverity::Error);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.message.contains("有效语音不足")));
        assert!(!report
            .issues
            .iter()
            .any(|issue| issue.message.contains("平均音量偏低")));
    }

    #[test]
    fn loud_prompt_does_not_warn_about_high_level_or_clipping() {
        let clip = test_clip(
            RecordingTakeKind::FixedPrompt { index: 0 },
            "prompt.wav",
            1.0,
        );
        let report = analyze_samples(&clip, &alternating_speech_samples(16_000, 1.0, 0.7), -35.0);

        assert!(!report
            .issues
            .iter()
            .any(|issue| issue.message.contains("平均音量偏高")));
        assert!(!report
            .issues
            .iter()
            .any(|issue| issue.message.contains("爆音风险")));
    }

    #[test]
    fn missing_required_recordings_raise_report_errors() {
        let manifest = TrainingRecordingManifest::new(2);
        let report = QualityReport::analyze_manifest(&manifest).expect("report should build");

        assert_eq!(report.severity(), QualitySeverity::Error);
        assert!(report.error_count() >= 4);
        assert_eq!(report.recorded_prompt_count, 0);
        assert_eq!(report.expected_prompt_count, 2);
    }

    fn test_clip(kind: RecordingTakeKind, relative_path: &str, duration_seconds: f32) -> RecordedClip {
        RecordedClip {
            kind,
            label: kind.label(),
            relative_path: relative_path.to_owned(),
            duration_seconds,
            sample_rate_hz: 16_000,
            sample_count: (duration_seconds * 16_000.0) as usize,
            peak_linear: 0.2,
        }
    }

    fn alternating_speech_samples(sample_rate_hz: u32, duration_seconds: f32, amplitude: f32) -> Vec<f32> {
        let frame_size = ((sample_rate_hz as usize) / 10).max(1);
        let total_samples = (sample_rate_hz as f32 * duration_seconds) as usize;
        let mut samples = Vec::with_capacity(total_samples);
        let mut speech_frame = true;

        while samples.len() < total_samples {
            let remaining = total_samples - samples.len();
            let fill_len = remaining.min(frame_size);
            let value = if speech_frame { amplitude } else { 0.0 };
            samples.extend(std::iter::repeat_n(value, fill_len));
            speech_frame = !speech_frame;
        }

        samples
    }
}
