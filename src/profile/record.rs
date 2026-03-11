use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{
    audio::{
        buffers::ring_capacity_samples,
        devices::find_input_device,
    },
    profile::storage::DEFAULT_PROFILE_ID,
};

const BUNDLED_PROMPTS_ZH_CN: &str = include_str!("../../assets/prompts_zh_cn.txt");
pub const REQUIRED_PROMPT_COUNT: usize = 10;
pub const AMBIENT_SILENCE_SECONDS: u32 = 5;
pub const FREE_SPEECH_SECONDS: u32 = 30;
const RECORDINGS_SUBDIR: &str = "recordings";
const WORKER_IDLE_SLEEP_MS: u64 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentScript {
    pub locale: &'static str,
    pub prompts: Vec<String>,
}

impl EnrollmentScript {
    pub fn load_bundled_zh_cn() -> Result<Self> {
        let prompts = BUNDLED_PROMPTS_ZH_CN
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();

        ensure!(
            prompts.len() == REQUIRED_PROMPT_COUNT,
            "bundled zh-CN enrollment prompts must contain exactly {REQUIRED_PROMPT_COUNT} items"
        );

        Ok(Self {
            locale: "zh-CN",
            prompts,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingTakeKind {
    AmbientSilence,
    FixedPrompt { index: usize },
    FreeSpeech,
}

impl RecordingTakeKind {
    pub fn label(self) -> String {
        match self {
            Self::AmbientSilence => "环境静音".to_owned(),
            Self::FixedPrompt { index } => format!("固定短句 {:02}", index + 1),
            Self::FreeSpeech => "自由发挥".to_owned(),
        }
    }

    fn file_name(self) -> String {
        match self {
            Self::AmbientSilence => "ambient_silence.wav".to_owned(),
            Self::FixedPrompt { index } => format!("fixed_prompt_{:02}.wav", index + 1),
            Self::FreeSpeech => "free_speech.wav".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordedClip {
    pub kind: RecordingTakeKind,
    pub label: String,
    pub relative_path: String,
    pub duration_seconds: f32,
    pub sample_rate_hz: u32,
    pub sample_count: usize,
    pub peak_linear: f32,
}

#[derive(Debug, Clone)]
pub struct TrainingRecordingManifest {
    pub ambient_silence: Option<RecordedClip>,
    pub fixed_prompts: Vec<Option<RecordedClip>>,
    pub free_speech: Option<RecordedClip>,
}

impl TrainingRecordingManifest {
    pub fn new(prompt_count: usize) -> Self {
        Self {
            ambient_silence: None,
            fixed_prompts: vec![None; prompt_count],
            free_speech: None,
        }
    }

    pub fn register(&mut self, clip: RecordedClip) {
        match clip.kind {
            RecordingTakeKind::AmbientSilence => self.ambient_silence = Some(clip),
            RecordingTakeKind::FixedPrompt { index } => {
                if let Some(slot) = self.fixed_prompts.get_mut(index) {
                    *slot = Some(clip);
                }
            }
            RecordingTakeKind::FreeSpeech => self.free_speech = Some(clip),
        }
    }

    pub fn recorded_prompt_count(&self) -> usize {
        self.fixed_prompts.iter().flatten().count()
    }

    pub fn clear_all(&mut self) -> Vec<RecordedClip> {
        let mut removed = Vec::new();

        if let Some(clip) = self.ambient_silence.take() {
            removed.push(clip);
        }

        for slot in &mut self.fixed_prompts {
            if let Some(clip) = slot.take() {
                removed.push(clip);
            }
        }

        if let Some(clip) = self.free_speech.take() {
            removed.push(clip);
        }

        removed
    }

    pub fn clear_from_prompt(&mut self, prompt_index: usize) -> Vec<RecordedClip> {
        let mut removed = Vec::new();

        for slot in self.fixed_prompts.iter_mut().skip(prompt_index) {
            if let Some(clip) = slot.take() {
                removed.push(clip);
            }
        }

        if let Some(clip) = self.free_speech.take() {
            removed.push(clip);
        }

        removed
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RecordingMetricsSnapshot {
    pub input_level_linear: f32,
    pub dropped_input_frames: u64,
}

pub struct RecordingSession {
    kind: RecordingTakeKind,
    absolute_path: PathBuf,
    relative_path: String,
    sample_rate_hz: u32,
    input_stream: Option<Stream>,
    metrics: Arc<RecordingMetrics>,
    running: Arc<AtomicBool>,
    worker_handle: Option<JoinHandle<RawRecording>>,
}

impl RecordingSession {
    pub fn start(kind: RecordingTakeKind, selected_input_device: Option<&str>) -> Result<Self> {
        let input_device = find_input_device(selected_input_device)?;
        let input_supported = input_device
            .default_input_config()
            .context("failed to query default training input config")?;
        let input_config = input_supported.config();
        let sample_rate_hz = input_config.sample_rate.0;
        let input_channels = input_config.channels as usize;

        let absolute_path = default_recordings_dir().join(kind.file_name());
        let relative_path = normalize_relative_path(&absolute_path);
        let capacity = ring_capacity_samples(sample_rate_hz).max(sample_rate_hz as usize);
        let (producer, consumer) = RingBuffer::<f32>::new(capacity);
        let metrics = Arc::new(RecordingMetrics::default());
        let running = Arc::new(AtomicBool::new(true));

        let worker_handle = thread::spawn({
            let running = Arc::clone(&running);
            move || drain_recording_samples(consumer, running)
        });

        let input_stream = build_recording_input_stream(
            &input_device,
            input_supported.sample_format(),
            &input_config,
            input_channels,
            producer,
            Arc::clone(&metrics),
        )?;

        input_stream
            .play()
            .context("failed to start training recording stream")?;

        Ok(Self {
            kind,
            absolute_path,
            relative_path,
            sample_rate_hz,
            input_stream: Some(input_stream),
            metrics,
            running,
            worker_handle: Some(worker_handle),
        })
    }

    pub fn metrics_snapshot(&self) -> RecordingMetricsSnapshot {
        self.metrics.snapshot()
    }

    pub fn finish(self) -> Result<RecordedClip> {
        let (raw_recording, absolute_path, relative_path, kind, sample_rate_hz) =
            self.stop_capture().context("failed to finalize training recording")?;
        write_recording_wav(&absolute_path, sample_rate_hz, &raw_recording.samples)?;

        Ok(RecordedClip {
            kind,
            label: kind.label(),
            relative_path,
            duration_seconds: raw_recording.samples.len() as f32 / sample_rate_hz as f32,
            sample_rate_hz,
            sample_count: raw_recording.samples.len(),
            peak_linear: raw_recording.peak_linear,
        })
    }

    pub fn discard(self) -> Result<()> {
        let (_, absolute_path, _, _, _) = self
            .stop_capture()
            .context("failed to discard training recording")?;

        if absolute_path.exists() {
            fs::remove_file(&absolute_path).with_context(|| {
                format!(
                    "failed to remove discarded training recording: {}",
                    absolute_path.display()
                )
            })?;
        }

        Ok(())
    }

    fn stop_capture(
        mut self,
    ) -> Result<(RawRecording, PathBuf, String, RecordingTakeKind, u32)> {
        let kind = self.kind;
        let sample_rate_hz = self.sample_rate_hz;
        let absolute_path = std::mem::take(&mut self.absolute_path);
        let relative_path = std::mem::take(&mut self.relative_path);

        drop(self.input_stream.take());
        self.running.store(false, Ordering::Release);

        let worker_handle = self
            .worker_handle
            .take()
            .ok_or_else(|| anyhow!("recording worker missing"))?;
        let raw_recording = worker_handle
            .join()
            .map_err(|_| anyhow!("training recording worker panicked"))?;

        Ok((
            raw_recording,
            absolute_path,
            relative_path,
            kind,
            sample_rate_hz,
        ))
    }
}

impl Drop for RecordingSession {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
    }
}

#[derive(Default)]
struct RecordingMetrics {
    input_level_linear_bits: AtomicU32,
    dropped_input_frames: AtomicU64,
}

impl RecordingMetrics {
    fn store_input_level(&self, value: f32) {
        self.input_level_linear_bits
            .store(value.to_bits(), Ordering::Relaxed);
    }

    fn snapshot(&self) -> RecordingMetricsSnapshot {
        RecordingMetricsSnapshot {
            input_level_linear: f32::from_bits(
                self.input_level_linear_bits.load(Ordering::Relaxed),
            ),
            dropped_input_frames: self.dropped_input_frames.load(Ordering::Relaxed),
        }
    }
}

struct RawRecording {
    samples: Vec<f32>,
    peak_linear: f32,
}

fn default_recordings_dir() -> PathBuf {
    PathBuf::from("profiles")
        .join(DEFAULT_PROFILE_ID)
        .join(RECORDINGS_SUBDIR)
}

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn drain_recording_samples(mut consumer: Consumer<f32>, running: Arc<AtomicBool>) -> RawRecording {
    let mut samples = Vec::new();
    let mut peak = 0.0_f32;

    loop {
        let mut drained_any = false;

        while let Ok(sample) = consumer.pop() {
            peak = peak.max(sample.abs());
            samples.push(sample);
            drained_any = true;
        }

        if !running.load(Ordering::Acquire) {
            break;
        }

        if !drained_any {
            thread::sleep(Duration::from_millis(WORKER_IDLE_SLEEP_MS));
        }
    }

    while let Ok(sample) = consumer.pop() {
        peak = peak.max(sample.abs());
        samples.push(sample);
    }

    RawRecording {
        samples,
        peak_linear: peak,
    }
}

fn build_recording_input_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    input_channels: usize,
    producer: Producer<f32>,
    metrics: Arc<RecordingMetrics>,
) -> Result<Stream> {
    match sample_format {
        SampleFormat::I8 => {
            build_recording_input_stream_t::<i8>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::I16 => {
            build_recording_input_stream_t::<i16>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::I24 => build_recording_input_stream_t::<cpal::I24>(
            device,
            config,
            input_channels,
            producer,
            metrics,
        ),
        SampleFormat::I32 => {
            build_recording_input_stream_t::<i32>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::I64 => {
            build_recording_input_stream_t::<i64>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::U8 => {
            build_recording_input_stream_t::<u8>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::U16 => {
            build_recording_input_stream_t::<u16>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::U32 => {
            build_recording_input_stream_t::<u32>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::U64 => {
            build_recording_input_stream_t::<u64>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::F32 => {
            build_recording_input_stream_t::<f32>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::F64 => {
            build_recording_input_stream_t::<f64>(device, config, input_channels, producer, metrics)
        }
        unsupported => Err(anyhow!(
            "unsupported input sample format for training recording: {unsupported}"
        )),
    }
}

fn build_recording_input_stream_t<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    input_channels: usize,
    mut producer: Producer<f32>,
    metrics: Arc<RecordingMetrics>,
) -> Result<Stream>
where
    T: Sample + SizedSample,
    f32: FromSample<T>,
{
    let err_fn = |error| tracing::error!(error = %error, "training input stream error");

    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mut peak = 0.0_f32;

                for frame in data.chunks(input_channels.max(1)) {
                    let mut mono = 0.0_f32;

                    for &sample in frame {
                        mono += f32::from_sample(sample);
                    }

                    mono /= frame.len().max(1) as f32;
                    peak = peak.max(mono.abs());

                    if producer.push(mono).is_err() {
                        metrics.dropped_input_frames.fetch_add(1, Ordering::Relaxed);
                    }
                }

                metrics.store_input_level(peak);
            },
            err_fn,
            None,
        )
        .context("failed to build training input stream")
}

fn write_recording_wav(path: &Path, sample_rate_hz: u32, samples: &[f32]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create recording directory: {}", parent.display()))?;
    }

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: sample_rate_hz,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("failed to create WAV writer: {}", path.display()))?;

    for &sample in samples {
        writer
            .write_sample(sample.clamp(-1.0, 1.0))
            .with_context(|| format!("failed to write WAV sample: {}", path.display()))?;
    }

    writer
        .finalize()
        .with_context(|| format!("failed to finalize WAV file: {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        EnrollmentScript, RecordedClip, RecordingTakeKind, TrainingRecordingManifest,
    };

    #[test]
    fn bundled_zh_cn_prompts_are_present() {
        let script = EnrollmentScript::load_bundled_zh_cn().expect("bundled prompts should load");
        assert_eq!(script.prompts.len(), super::REQUIRED_PROMPT_COUNT);
        assert!(script.prompts.iter().all(|prompt| !prompt.trim().is_empty()));
    }

    #[test]
    fn clear_from_prompt_drops_later_recordings() {
        let mut manifest = TrainingRecordingManifest::new(3);
        manifest.register(test_clip(RecordingTakeKind::AmbientSilence, "ambient.wav"));
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 0 },
            "prompt_01.wav",
        ));
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 1 },
            "prompt_02.wav",
        ));
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 2 },
            "prompt_03.wav",
        ));
        manifest.register(test_clip(RecordingTakeKind::FreeSpeech, "free.wav"));

        let removed = manifest.clear_from_prompt(1);

        assert_eq!(removed.len(), 3);
        assert!(manifest.ambient_silence.is_some());
        assert!(manifest.fixed_prompts[0].is_some());
        assert!(manifest.fixed_prompts[1].is_none());
        assert!(manifest.fixed_prompts[2].is_none());
        assert!(manifest.free_speech.is_none());
    }

    fn test_clip(kind: RecordingTakeKind, path: &str) -> RecordedClip {
        RecordedClip {
            kind,
            label: kind.label(),
            relative_path: path.to_owned(),
            duration_seconds: 1.0,
            sample_rate_hz: 48_000,
            sample_count: 48_000,
            peak_linear: 0.5,
        }
    }
}
