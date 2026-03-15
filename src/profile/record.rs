use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use cpal::{
    FromSample, Sample, SampleFormat, SampleRate, SizedSample, Stream, StreamConfig,
    SupportedStreamConfig,
    traits::{DeviceTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{
    audio::{
        buffers::ring_capacity_samples,
        devices::{find_input_device, find_output_device},
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

    pub fn short_label(self) -> &'static str {
        match self {
            Self::AmbientSilence => "环境静音",
            Self::FixedPrompt { .. } => "固定短句",
            Self::FreeSpeech => "自由发挥",
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

    pub fn recorded_clip_count(&self) -> usize {
        usize::from(self.ambient_silence.is_some())
            + self.recorded_prompt_count()
            + usize::from(self.free_speech.is_some())
    }

    pub fn is_complete(&self) -> bool {
        self.ambient_silence.is_some()
            && self.free_speech.is_some()
            && self.recorded_prompt_count() == self.fixed_prompts.len()
    }

    pub fn get(&self, kind: RecordingTakeKind) -> Option<&RecordedClip> {
        match kind {
            RecordingTakeKind::AmbientSilence => self.ambient_silence.as_ref(),
            RecordingTakeKind::FixedPrompt { index } => {
                self.fixed_prompts.get(index).and_then(|slot| slot.as_ref())
            }
            RecordingTakeKind::FreeSpeech => self.free_speech.as_ref(),
        }
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

#[derive(Debug, Clone)]
pub struct DetectedTrainingRecordings {
    pub manifest: TrainingRecordingManifest,
    pub missing_paths: Vec<String>,
    pub unexpected_entries: Vec<String>,
    pub invalid_entries: Vec<String>,
}

impl DetectedTrainingRecordings {
    pub fn recognized_count(&self) -> usize {
        source_recordings_from_manifest(&self.manifest).len()
    }

    pub fn is_complete(&self) -> bool {
        self.manifest.is_complete()
    }

    pub fn can_load(&self) -> bool {
        self.recognized_count() > 0
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

pub struct RecordingPreviewSession {
    _output_stream: Stream,
    finished: Arc<AtomicBool>,
}

impl RecordingPreviewSession {
    pub fn start(clip: &RecordedClip) -> Result<Self> {
        let output_device = find_output_device(None)?;
        let output_supported =
            select_preview_output_config(&output_device, SampleRate(clip.sample_rate_hz))?;
        let output_config = output_supported.config();
        let output_channels = output_config.channels as usize;
        let samples = Arc::new(read_preview_samples(Path::new(&clip.relative_path))?);
        let sample_index = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicBool::new(false));

        let output_stream = build_preview_output_stream(
            &output_device,
            output_supported.sample_format(),
            &output_config,
            output_channels,
            samples,
            sample_index,
            Arc::clone(&finished),
        )?;

        output_stream
            .play()
            .context("failed to start recording preview stream")?;

        Ok(Self {
            _output_stream: output_stream,
            finished,
        })
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
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
        let (raw_recording, absolute_path, relative_path, kind, sample_rate_hz) = self
            .stop_capture()
            .context("failed to finalize training recording")?;
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

    fn stop_capture(mut self) -> Result<(RawRecording, PathBuf, String, RecordingTakeKind, u32)> {
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

pub fn scan_default_recordings(prompt_count: usize) -> Result<Option<DetectedTrainingRecordings>> {
    let recordings_dir = default_recordings_dir();
    scan_recordings_dir(&recordings_dir, prompt_count)
}

fn scan_recordings_dir(
    recordings_dir: &Path,
    prompt_count: usize,
) -> Result<Option<DetectedTrainingRecordings>> {
    if !recordings_dir.exists() {
        return Ok(None);
    }

    let expected_entries = expected_recording_entries(recordings_dir, prompt_count);
    let expected_by_file_name = expected_entries
        .iter()
        .map(|(kind, path)| {
            (
                Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default(),
                (*kind, path.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut manifest = TrainingRecordingManifest::new(prompt_count);
    let mut unexpected_entries = Vec::new();
    let mut invalid_entries = Vec::new();

    for entry in fs::read_dir(&recordings_dir).with_context(|| {
        format!(
            "failed to read recordings directory: {}",
            recordings_dir.display()
        )
    })? {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect recordings directory entry: {}",
                recordings_dir.display()
            )
        })?;
        let path = entry.path();
        let relative_path = normalize_relative_path(&path);
        let file_name = entry.file_name().to_string_lossy().to_string();

        let Some((kind, _expected_path)) = expected_by_file_name.get(&file_name).cloned() else {
            unexpected_entries.push(relative_path);
            continue;
        };

        if !path.is_file() {
            unexpected_entries.push(relative_path);
            continue;
        }

        match load_recorded_clip_from_path(kind, &path) {
            Ok(clip) => manifest.register(clip),
            Err(error) => invalid_entries.push(format!("{relative_path}：{error}")),
        }
    }

    let missing_paths = expected_entries
        .into_iter()
        .filter_map(|(kind, path)| manifest.get(kind).is_none().then_some(path))
        .collect::<Vec<_>>();

    if manifest.recorded_prompt_count() == 0
        && manifest.ambient_silence.is_none()
        && manifest.free_speech.is_none()
        && unexpected_entries.is_empty()
        && invalid_entries.is_empty()
    {
        return Ok(None);
    }

    unexpected_entries.sort();
    invalid_entries.sort();

    Ok(Some(DetectedTrainingRecordings {
        manifest,
        missing_paths,
        unexpected_entries,
        invalid_entries,
    }))
}

pub fn clear_default_recordings_dir() -> Result<()> {
    let recordings_dir = default_recordings_dir();
    if !recordings_dir.exists() {
        return Ok(());
    }

    fs::remove_dir_all(&recordings_dir).with_context(|| {
        format!(
            "failed to clear recordings directory: {}",
            recordings_dir.display()
        )
    })
}

pub fn source_recordings_from_manifest(manifest: &TrainingRecordingManifest) -> Vec<String> {
    let mut source_recordings = Vec::with_capacity(manifest.fixed_prompts.len() + 2);

    if let Some(clip) = &manifest.ambient_silence {
        source_recordings.push(clip.relative_path.clone());
    }

    for clip in manifest.fixed_prompts.iter().flatten() {
        source_recordings.push(clip.relative_path.clone());
    }

    if let Some(clip) = &manifest.free_speech {
        source_recordings.push(clip.relative_path.clone());
    }

    source_recordings
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

fn expected_recording_entries(
    recordings_dir: &Path,
    prompt_count: usize,
) -> Vec<(RecordingTakeKind, String)> {
    let mut entries = Vec::with_capacity(prompt_count + 2);
    let ambient = RecordingTakeKind::AmbientSilence;
    entries.push((
        ambient,
        normalize_relative_path(&recordings_dir.join(ambient.file_name())),
    ));

    for index in 0..prompt_count {
        let kind = RecordingTakeKind::FixedPrompt { index };
        entries.push((
            kind,
            normalize_relative_path(&recordings_dir.join(kind.file_name())),
        ));
    }

    let free_speech = RecordingTakeKind::FreeSpeech;
    entries.push((
        free_speech,
        normalize_relative_path(&recordings_dir.join(free_speech.file_name())),
    ));

    entries
}

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn load_recorded_clip_from_path(kind: RecordingTakeKind, path: &Path) -> Result<RecordedClip> {
    let (sample_rate_hz, sample_count, peak_linear) = read_recording_metadata(path)?;
    ensure!(
        sample_rate_hz > 0,
        "recording sample rate must be positive: {}",
        path.display()
    );

    Ok(RecordedClip {
        kind,
        label: kind.label(),
        relative_path: normalize_relative_path(path),
        duration_seconds: sample_count as f32 / sample_rate_hz as f32,
        sample_rate_hz,
        sample_count,
        peak_linear,
    })
}

fn read_recording_metadata(path: &Path) -> Result<(u32, usize, f32)> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to create WAV reader: {}", path.display()))?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1,
        "training recording must be mono WAV, got {} channels at {}",
        spec.channels,
        path.display()
    );

    match spec.sample_format {
        hound::SampleFormat::Float => {
            let mut sample_count = 0_usize;
            let mut peak_linear = 0.0_f32;

            for sample in reader.samples::<f32>() {
                let value = sample
                    .map(|sample| sample.clamp(-1.0, 1.0))
                    .map_err(|error| anyhow!("failed to decode WAV sample: {error}"))?;
                peak_linear = peak_linear.max(value.abs());
                sample_count += 1;
            }

            Ok((spec.sample_rate, sample_count, peak_linear))
        }
        hound::SampleFormat::Int => {
            ensure!(
                spec.bits_per_sample > 0 && spec.bits_per_sample <= 32,
                "unsupported WAV bits per sample {} at {}",
                spec.bits_per_sample,
                path.display()
            );
            let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            let mut sample_count = 0_usize;
            let mut peak_linear = 0.0_f32;

            for sample in reader.samples::<i32>() {
                let value = sample
                    .map(|sample| (sample as f32 / scale).clamp(-1.0, 1.0))
                    .map_err(|error| anyhow!("failed to decode WAV sample: {error}"))?;
                peak_linear = peak_linear.max(value.abs());
                sample_count += 1;
            }

            Ok((spec.sample_rate, sample_count, peak_linear))
        }
    }
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

fn select_preview_output_config(
    output_device: &cpal::Device,
    desired_sample_rate: SampleRate,
) -> Result<SupportedStreamConfig> {
    output_device
        .supported_output_configs()
        .context("failed to enumerate supported preview output configs")?
        .find_map(|config_range| config_range.try_with_sample_rate(desired_sample_rate))
        .ok_or_else(|| {
            anyhow!(
                "selected output device does not support preview sample rate {} Hz",
                desired_sample_rate.0
            )
        })
}

fn build_preview_output_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    output_channels: usize,
    samples: Arc<Vec<f32>>,
    sample_index: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
) -> Result<Stream> {
    match sample_format {
        SampleFormat::I8 => build_preview_output_stream_t::<i8>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::I16 => build_preview_output_stream_t::<i16>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::I24 => build_preview_output_stream_t::<cpal::I24>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::I32 => build_preview_output_stream_t::<i32>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::I64 => build_preview_output_stream_t::<i64>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::U8 => build_preview_output_stream_t::<u8>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::U16 => build_preview_output_stream_t::<u16>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::U32 => build_preview_output_stream_t::<u32>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::U64 => build_preview_output_stream_t::<u64>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::F32 => build_preview_output_stream_t::<f32>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        SampleFormat::F64 => build_preview_output_stream_t::<f64>(
            device,
            config,
            output_channels,
            samples,
            sample_index,
            finished,
        ),
        unsupported => Err(anyhow!(
            "unsupported output sample format for recording preview: {unsupported}"
        )),
    }
}

fn build_preview_output_stream_t<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    output_channels: usize,
    samples: Arc<Vec<f32>>,
    sample_index: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
) -> Result<Stream>
where
    T: Sample + SizedSample + FromSample<f32>,
{
    let err_fn = |error| tracing::error!(error = %error, "recording preview output stream error");

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for frame in data.chunks_mut(output_channels.max(1)) {
                    let index = sample_index.fetch_add(1, Ordering::Relaxed);
                    let mono = if let Some(&sample) = samples.get(index) {
                        sample
                    } else {
                        finished.store(true, Ordering::Release);
                        0.0
                    };

                    for channel in frame {
                        *channel = T::from_sample(mono);
                    }
                }
            },
            err_fn,
            None,
        )
        .context("failed to build recording preview output stream")
}

fn read_preview_samples(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to create preview WAV reader: {}", path.display()))?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1,
        "recording preview only supports mono WAV input, got {} channels at {}",
        spec.channels,
        path.display()
    );

    match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| {
                sample
                    .map(|value| value.clamp(-1.0, 1.0))
                    .map_err(|error| anyhow!("failed to decode preview WAV sample: {error}"))
            })
            .collect(),
        hound::SampleFormat::Int => {
            ensure!(
                spec.bits_per_sample > 0 && spec.bits_per_sample <= 32,
                "unsupported preview WAV bits per sample {} at {}",
                spec.bits_per_sample,
                path.display()
            );
            let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| (value as f32 / scale).clamp(-1.0, 1.0))
                        .map_err(|error| anyhow!("failed to decode preview WAV sample: {error}"))
                })
                .collect()
        }
    }
}

fn write_recording_wav(path: &Path, sample_rate_hz: u32, samples: &[f32]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create recording directory: {}", parent.display())
        })?;
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
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        DEFAULT_PROFILE_ID, EnrollmentScript, RECORDINGS_SUBDIR, RecordedClip, RecordingTakeKind,
        TrainingRecordingManifest, scan_recordings_dir,
    };

    #[test]
    fn bundled_zh_cn_prompts_are_present() {
        let script = EnrollmentScript::load_bundled_zh_cn().expect("bundled prompts should load");
        assert_eq!(script.prompts.len(), super::REQUIRED_PROMPT_COUNT);
        assert!(
            script
                .prompts
                .iter()
                .all(|prompt| !prompt.trim().is_empty())
        );
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

    #[test]
    fn recorded_clip_count_includes_ambient_prompts_and_free_speech() {
        let mut manifest = TrainingRecordingManifest::new(3);
        assert_eq!(manifest.recorded_clip_count(), 0);

        manifest.register(test_clip(RecordingTakeKind::AmbientSilence, "ambient.wav"));
        manifest.register(test_clip(
            RecordingTakeKind::FixedPrompt { index: 1 },
            "prompt_02.wav",
        ));
        manifest.register(test_clip(RecordingTakeKind::FreeSpeech, "free.wav"));

        assert_eq!(manifest.recorded_clip_count(), 3);
    }

    #[test]
    fn scan_default_recordings_detects_complete_expected_set() {
        let root = unique_test_root();
        let recordings_dir = root
            .join("profiles")
            .join(DEFAULT_PROFILE_ID)
            .join(RECORDINGS_SUBDIR);
        fs::create_dir_all(&recordings_dir).expect("recordings dir should exist");
        write_test_wav(&recordings_dir.join("ambient_silence.wav"), 16_000, 16_000);
        write_test_wav(&recordings_dir.join("fixed_prompt_01.wav"), 16_000, 16_000);
        write_test_wav(&recordings_dir.join("free_speech.wav"), 16_000, 48_000);

        let detected = scan_recordings_dir(&recordings_dir, 1)
            .expect("scan should succeed")
            .expect("existing recordings should be detected");
        let _ = fs::remove_dir_all(root);

        assert!(detected.is_complete());
        assert!(detected.unexpected_entries.is_empty());
        assert!(detected.invalid_entries.is_empty());
        assert_eq!(detected.recognized_count(), 3);
        assert!(detected.can_load());
    }

    #[test]
    fn scan_default_recordings_flags_missing_and_unexpected_entries() {
        let root = unique_test_root();
        let recordings_dir = root
            .join("profiles")
            .join(DEFAULT_PROFILE_ID)
            .join(RECORDINGS_SUBDIR);
        fs::create_dir_all(&recordings_dir).expect("recordings dir should exist");
        write_test_wav(&recordings_dir.join("ambient_silence.wav"), 16_000, 16_000);
        fs::write(recordings_dir.join("notes.txt"), "unexpected").expect("misc file should write");

        let detected = scan_recordings_dir(&recordings_dir, 1)
            .expect("scan should succeed")
            .expect("existing recordings should be detected");
        let _ = fs::remove_dir_all(root);

        assert!(!detected.is_complete());
        assert_eq!(detected.missing_paths.len(), 2);
        assert_eq!(detected.unexpected_entries.len(), 1);
        assert_eq!(detected.recognized_count(), 1);
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

    fn unique_test_root() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ek-single-mic-record-scan-{nonce}"))
    }

    fn write_test_wav(path: &Path, sample_rate_hz: u32, sample_count: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sample_rate_hz,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("test wav should create");
        for index in 0..sample_count {
            let sample = if index % 128 == 0 { 0.25 } else { 0.0 };
            writer
                .write_sample(sample)
                .expect("test sample should write");
        }
        writer.finalize().expect("test wav should finalize");
    }
}
