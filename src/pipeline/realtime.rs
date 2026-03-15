use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use cpal::{
    FromSample, Sample, SampleFormat, SampleRate, SizedSample, Stream, StreamConfig,
    SupportedStreamConfig,
    traits::{DeviceTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{
    audio::{
        buffers::{latency_samples, ring_capacity_samples},
        devices::{find_input_device, find_output_device},
    },
    config::settings::InferenceMode,
    pipeline::{
        BasicFilterChunkMetrics, BasicFilterChunkOutcome, BasicFilterEngine,
        frames::linear_resample,
    },
    profile::storage::SpeakerProfile,
    util::{audio_math::dbfs_from_linear, time::MODEL_SAMPLE_RATE},
};

const BASIC_FILTER_WORKER_SLEEP_MS: u64 = 4;
const BASIC_FILTER_CHUNK_MS: u32 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStage {
    Stopped,
    RunningPassthrough,
    RunningBasicFilter,
}

impl RuntimeStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::RunningPassthrough => "Running (Passthrough)",
            Self::RunningBasicFilter => "Running (Basic Filter)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeFormatSummary {
    pub input_device_name: String,
    pub output_device_name: String,
    pub sample_rate_hz: u32,
    pub input_channels: u16,
    pub output_channels: u16,
    pub input_sample_format: String,
    pub output_sample_format: String,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeMetricsSnapshot {
    pub input_peak_dbfs: f32,
    pub output_peak_dbfs: f32,
    pub dropped_input_frames: u64,
    pub missing_output_frames: u64,
    pub successful_output_frames: u64,
    pub processed_output_chunks: u64,
    pub current_similarity: f32,
    pub current_frame_gain: f32,
    pub last_chunk_active_frames: u64,
    pub last_chunk_analyzed_frames: u64,
}

impl Default for RuntimeMetricsSnapshot {
    fn default() -> Self {
        Self {
            input_peak_dbfs: dbfs_from_linear(0.0),
            output_peak_dbfs: dbfs_from_linear(0.0),
            dropped_input_frames: 0,
            missing_output_frames: 0,
            successful_output_frames: 0,
            processed_output_chunks: 0,
            current_similarity: 0.0,
            current_frame_gain: 0.0,
            last_chunk_active_frames: 0,
            last_chunk_analyzed_frames: 0,
        }
    }
}

pub enum RealtimeRuntime {
    Passthrough(PassthroughRuntime),
    BasicFilter(BasicFilterRuntime),
}

impl RealtimeRuntime {
    pub fn start(
        mode: InferenceMode,
        selected_input_device: Option<&str>,
        selected_output_device: Option<&str>,
        profile: Option<&SpeakerProfile>,
    ) -> Result<Self> {
        match mode {
            InferenceMode::Passthrough => Ok(Self::Passthrough(PassthroughRuntime::start(
                selected_input_device,
                selected_output_device,
            )?)),
            InferenceMode::BasicFilter => Ok(Self::BasicFilter(BasicFilterRuntime::start(
                selected_input_device,
                selected_output_device,
                profile.context("Basic Filter 启动前需要先加载默认 speaker profile")?,
            )?)),
            InferenceMode::StrongIsolation => Err(anyhow!(
                "Strong Isolation 仍是预留模式，当前尚未接入实时链路"
            )),
        }
    }

    pub fn metrics_snapshot(&self) -> RuntimeMetricsSnapshot {
        match self {
            Self::Passthrough(runtime) => runtime.metrics_snapshot(),
            Self::BasicFilter(runtime) => runtime.metrics_snapshot(),
        }
    }

    pub fn format_summary(&self) -> &RuntimeFormatSummary {
        match self {
            Self::Passthrough(runtime) => runtime.format_summary(),
            Self::BasicFilter(runtime) => runtime.format_summary(),
        }
    }

    pub fn stage(&self) -> RuntimeStage {
        match self {
            Self::Passthrough(_) => RuntimeStage::RunningPassthrough,
            Self::BasicFilter(_) => RuntimeStage::RunningBasicFilter,
        }
    }

    pub fn is_output_ready(&self) -> bool {
        match self {
            Self::Passthrough(runtime) => runtime.is_output_ready(),
            Self::BasicFilter(runtime) => runtime.is_output_ready(),
        }
    }
}

pub struct PassthroughRuntime {
    _input_stream: Stream,
    _output_stream: Stream,
    metrics: Arc<RuntimeMetrics>,
    format: RuntimeFormatSummary,
    priming_output_frames: u64,
}

impl PassthroughRuntime {
    pub fn start(
        selected_input_device: Option<&str>,
        selected_output_device: Option<&str>,
    ) -> Result<Self> {
        let runtime_io = prepare_runtime_io(selected_input_device, selected_output_device)?;
        let latency = latency_samples(runtime_io.input_config.sample_rate.0);
        let capacity =
            ring_capacity_samples(runtime_io.input_config.sample_rate.0).max(latency * 4);
        let (mut producer, consumer) = RingBuffer::<f32>::new(capacity);

        for _ in 0..latency {
            let _ = producer.push(0.0);
        }

        let metrics = Arc::new(RuntimeMetrics::default());
        let input_stream = build_input_stream(
            &runtime_io.input_device,
            runtime_io.input_supported.sample_format(),
            &runtime_io.input_config,
            runtime_io.input_channels,
            producer,
            Arc::clone(&metrics),
        )?;
        let output_stream = build_output_stream(
            &runtime_io.output_device,
            runtime_io.output_supported.sample_format(),
            &runtime_io.output_config,
            runtime_io.output_channels,
            consumer,
            Arc::clone(&metrics),
        )?;

        input_stream
            .play()
            .context("failed to start input stream playback")?;
        output_stream
            .play()
            .context("failed to start output stream playback")?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            metrics,
            format: runtime_io.format_summary,
            priming_output_frames: latency as u64,
        })
    }

    pub fn metrics_snapshot(&self) -> RuntimeMetricsSnapshot {
        self.metrics.snapshot()
    }

    pub fn format_summary(&self) -> &RuntimeFormatSummary {
        &self.format
    }

    pub fn is_output_ready(&self) -> bool {
        self.metrics.snapshot().successful_output_frames > self.priming_output_frames
    }
}

pub struct BasicFilterRuntime {
    _input_stream: Stream,
    _output_stream: Stream,
    worker_stop: Arc<AtomicBool>,
    worker_handle: Option<JoinHandle<()>>,
    metrics: Arc<RuntimeMetrics>,
    format: RuntimeFormatSummary,
    priming_output_frames: u64,
}

impl BasicFilterRuntime {
    pub fn start(
        selected_input_device: Option<&str>,
        selected_output_device: Option<&str>,
        profile: &SpeakerProfile,
    ) -> Result<Self> {
        let runtime_io = prepare_runtime_io(selected_input_device, selected_output_device)?;
        let sample_rate_hz = runtime_io.input_config.sample_rate.0;
        let latency = latency_samples(sample_rate_hz);
        let capacity = ring_capacity_samples(sample_rate_hz).max(latency * 6);

        let (input_producer, input_consumer) = RingBuffer::<f32>::new(capacity);
        let (mut output_producer, output_consumer) = RingBuffer::<f32>::new(capacity);
        for _ in 0..latency {
            let _ = output_producer.push(0.0);
        }

        let metrics = Arc::new(RuntimeMetrics::default());
        let input_stream = build_input_stream(
            &runtime_io.input_device,
            runtime_io.input_supported.sample_format(),
            &runtime_io.input_config,
            runtime_io.input_channels,
            input_producer,
            Arc::clone(&metrics),
        )?;
        let output_stream = build_output_stream(
            &runtime_io.output_device,
            runtime_io.output_supported.sample_format(),
            &runtime_io.output_config,
            runtime_io.output_channels,
            output_consumer,
            Arc::clone(&metrics),
        )?;

        let worker_stop = Arc::new(AtomicBool::new(false));
        let worker_handle = Some(spawn_basic_filter_worker(
            sample_rate_hz,
            input_consumer,
            output_producer,
            Arc::clone(&metrics),
            Arc::clone(&worker_stop),
            profile.clone(),
        )?);

        input_stream
            .play()
            .context("failed to start input stream playback")?;
        output_stream
            .play()
            .context("failed to start output stream playback")?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            worker_stop,
            worker_handle,
            metrics,
            format: runtime_io.format_summary,
            priming_output_frames: latency as u64,
        })
    }

    pub fn metrics_snapshot(&self) -> RuntimeMetricsSnapshot {
        self.metrics.snapshot()
    }

    pub fn format_summary(&self) -> &RuntimeFormatSummary {
        &self.format
    }

    pub fn is_output_ready(&self) -> bool {
        let snapshot = self.metrics.snapshot();
        snapshot.processed_output_chunks > 0
            && snapshot.successful_output_frames > self.priming_output_frames
    }
}

impl Drop for BasicFilterRuntime {
    fn drop(&mut self) {
        self.worker_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Default)]
struct RuntimeMetrics {
    input_peak_linear_bits: AtomicU32,
    output_peak_linear_bits: AtomicU32,
    dropped_input_frames: AtomicU64,
    missing_output_frames: AtomicU64,
    successful_output_frames: AtomicU64,
    processed_output_chunks: AtomicU64,
    current_similarity_bits: AtomicU32,
    current_frame_gain_bits: AtomicU32,
    last_chunk_active_frames: AtomicU64,
    last_chunk_analyzed_frames: AtomicU64,
}

impl RuntimeMetrics {
    fn store_input_peak(&self, value: f32) {
        self.input_peak_linear_bits
            .store(value.to_bits(), Ordering::Relaxed);
    }

    fn store_output_peak(&self, value: f32) {
        self.output_peak_linear_bits
            .store(value.to_bits(), Ordering::Relaxed);
    }

    fn store_filter_metrics(&self, metrics: BasicFilterChunkMetrics) {
        self.current_similarity_bits
            .store(metrics.latest_similarity.to_bits(), Ordering::Relaxed);
        self.current_frame_gain_bits
            .store(metrics.latest_frame_gain.to_bits(), Ordering::Relaxed);
        self.processed_output_chunks.fetch_add(1, Ordering::Relaxed);
        self.last_chunk_active_frames
            .store(metrics.active_frame_count as u64, Ordering::Relaxed);
        self.last_chunk_analyzed_frames
            .store(metrics.analyzed_frame_count as u64, Ordering::Relaxed);
    }

    fn snapshot(&self) -> RuntimeMetricsSnapshot {
        RuntimeMetricsSnapshot {
            input_peak_dbfs: dbfs_from_linear(f32::from_bits(
                self.input_peak_linear_bits.load(Ordering::Relaxed),
            )),
            output_peak_dbfs: dbfs_from_linear(f32::from_bits(
                self.output_peak_linear_bits.load(Ordering::Relaxed),
            )),
            dropped_input_frames: self.dropped_input_frames.load(Ordering::Relaxed),
            missing_output_frames: self.missing_output_frames.load(Ordering::Relaxed),
            successful_output_frames: self.successful_output_frames.load(Ordering::Relaxed),
            processed_output_chunks: self.processed_output_chunks.load(Ordering::Relaxed),
            current_similarity: f32::from_bits(
                self.current_similarity_bits.load(Ordering::Relaxed),
            ),
            current_frame_gain: f32::from_bits(
                self.current_frame_gain_bits.load(Ordering::Relaxed),
            ),
            last_chunk_active_frames: self.last_chunk_active_frames.load(Ordering::Relaxed),
            last_chunk_analyzed_frames: self.last_chunk_analyzed_frames.load(Ordering::Relaxed),
        }
    }
}

struct RuntimeIo {
    input_device: cpal::Device,
    output_device: cpal::Device,
    input_supported: SupportedStreamConfig,
    output_supported: SupportedStreamConfig,
    input_config: StreamConfig,
    output_config: StreamConfig,
    input_channels: usize,
    output_channels: usize,
    format_summary: RuntimeFormatSummary,
}

fn prepare_runtime_io(
    selected_input_device: Option<&str>,
    selected_output_device: Option<&str>,
) -> Result<RuntimeIo> {
    let input_device = find_input_device(selected_input_device)?;
    let output_device = find_output_device(selected_output_device)?;

    let input_name = input_device
        .name()
        .unwrap_or_else(|_| "Unknown input device".to_owned());
    let output_name = output_device
        .name()
        .unwrap_or_else(|_| "Unknown output device".to_owned());

    let input_supported = input_device
        .default_input_config()
        .context("failed to query default input config")?;
    let output_supported = select_output_config(&output_device, input_supported.sample_rate())?;
    let input_config = input_supported.config();
    let output_config = output_supported.config();

    Ok(RuntimeIo {
        input_device,
        output_device,
        input_channels: input_config.channels as usize,
        output_channels: output_config.channels as usize,
        format_summary: RuntimeFormatSummary {
            input_device_name: input_name,
            output_device_name: output_name,
            sample_rate_hz: input_config.sample_rate.0,
            input_channels: input_config.channels,
            output_channels: output_config.channels,
            input_sample_format: input_supported.sample_format().to_string(),
            output_sample_format: output_supported.sample_format().to_string(),
        },
        input_supported,
        output_supported,
        input_config,
        output_config,
    })
}

fn spawn_basic_filter_worker(
    sample_rate_hz: u32,
    mut input_consumer: Consumer<f32>,
    mut output_producer: Producer<f32>,
    metrics: Arc<RuntimeMetrics>,
    worker_stop: Arc<AtomicBool>,
    profile: SpeakerProfile,
) -> Result<JoinHandle<()>> {
    let chunk_samples =
        ((sample_rate_hz as usize * BASIC_FILTER_CHUNK_MS as usize) / 1_000).max(512);

    thread::Builder::new()
        .name("ek-basic-filter-worker".to_owned())
        .spawn(move || {
            let mut engine = match BasicFilterEngine::from_profile(&profile) {
                Ok(engine) => engine,
                Err(error) => {
                    tracing::error!(
                        error = %format!("{error:#}"),
                        "basic filter worker failed to initialize profile state"
                    );
                    return;
                }
            };
            let mut pending_input = Vec::with_capacity(chunk_samples * 2);

            while !worker_stop.load(Ordering::Relaxed) {
                while let Ok(sample) = input_consumer.pop() {
                    pending_input.push(sample);
                }

                if pending_input.len() < chunk_samples {
                    thread::sleep(Duration::from_millis(BASIC_FILTER_WORKER_SLEEP_MS));
                    continue;
                }

                let chunk = pending_input.drain(..chunk_samples).collect::<Vec<_>>();
                let model_chunk = linear_resample(&chunk, sample_rate_hz, MODEL_SAMPLE_RATE);
                let processed = match engine.process_model_rate_samples(&model_chunk) {
                    Ok(processed) => processed,
                    Err(error) => {
                        tracing::error!(error = %format!("{error:#}"), "basic filter worker failed to process chunk");
                        let silence = vec![0.0; model_chunk.len()];
                        metrics.store_filter_metrics(BasicFilterChunkMetrics::default());
                        BasicFilterChunkOutcome {
                            output_samples: silence,
                            metrics: BasicFilterChunkMetrics::default(),
                        }
                    }
                };

                metrics.store_filter_metrics(processed.metrics);
                let output_chunk =
                    linear_resample(&processed.output_samples, MODEL_SAMPLE_RATE, sample_rate_hz);
                for sample in output_chunk {
                    if output_producer.push(sample).is_err() {
                        break;
                    }
                }
            }
        })
        .context("failed to spawn basic filter worker thread")
}

fn select_output_config(
    output_device: &cpal::Device,
    desired_sample_rate: SampleRate,
) -> Result<SupportedStreamConfig> {
    output_device
        .supported_output_configs()
        .context("failed to enumerate supported output configs")?
        .find_map(|config_range| config_range.try_with_sample_rate(desired_sample_rate))
        .ok_or_else(|| {
            anyhow!(
                "selected output device does not support {} Hz; current realtime pipeline still requires equal input/output sample rates",
                desired_sample_rate.0
            )
        })
}

fn build_input_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    input_channels: usize,
    producer: Producer<f32>,
    metrics: Arc<RuntimeMetrics>,
) -> Result<Stream> {
    match sample_format {
        SampleFormat::I8 => {
            build_input_stream_t::<i8>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::I16 => {
            build_input_stream_t::<i16>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::I24 => {
            build_input_stream_t::<cpal::I24>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::I32 => {
            build_input_stream_t::<i32>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::I64 => {
            build_input_stream_t::<i64>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::U8 => {
            build_input_stream_t::<u8>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::U16 => {
            build_input_stream_t::<u16>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::U32 => {
            build_input_stream_t::<u32>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::U64 => {
            build_input_stream_t::<u64>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::F32 => {
            build_input_stream_t::<f32>(device, config, input_channels, producer, metrics)
        }
        SampleFormat::F64 => {
            build_input_stream_t::<f64>(device, config, input_channels, producer, metrics)
        }
        unsupported => Err(anyhow!(
            "unsupported input sample format for realtime runtime: {unsupported}"
        )),
    }
}

fn build_output_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    output_channels: usize,
    consumer: Consumer<f32>,
    metrics: Arc<RuntimeMetrics>,
) -> Result<Stream> {
    match sample_format {
        SampleFormat::I8 => {
            build_output_stream_t::<i8>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::I16 => {
            build_output_stream_t::<i16>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::I24 => {
            build_output_stream_t::<cpal::I24>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::I32 => {
            build_output_stream_t::<i32>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::I64 => {
            build_output_stream_t::<i64>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::U8 => {
            build_output_stream_t::<u8>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::U16 => {
            build_output_stream_t::<u16>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::U32 => {
            build_output_stream_t::<u32>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::U64 => {
            build_output_stream_t::<u64>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::F32 => {
            build_output_stream_t::<f32>(device, config, output_channels, consumer, metrics)
        }
        SampleFormat::F64 => {
            build_output_stream_t::<f64>(device, config, output_channels, consumer, metrics)
        }
        unsupported => Err(anyhow!(
            "unsupported output sample format for realtime runtime: {unsupported}"
        )),
    }
}

fn build_input_stream_t<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    input_channels: usize,
    mut producer: Producer<f32>,
    metrics: Arc<RuntimeMetrics>,
) -> Result<Stream>
where
    T: Sample + SizedSample,
    f32: FromSample<T>,
{
    let err_fn = |error| tracing::error!(error = %error, "input stream error");

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

                metrics.store_input_peak(peak);
            },
            err_fn,
            None,
        )
        .context("failed to build input stream")
}

fn build_output_stream_t<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    output_channels: usize,
    mut consumer: Consumer<f32>,
    metrics: Arc<RuntimeMetrics>,
) -> Result<Stream>
where
    T: Sample + SizedSample + FromSample<f32>,
{
    let err_fn = |error| tracing::error!(error = %error, "output stream error");

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut peak = 0.0_f32;

                for frame in data.chunks_mut(output_channels.max(1)) {
                    let mono = match consumer.pop() {
                        Ok(sample) => {
                            metrics
                                .successful_output_frames
                                .fetch_add(1, Ordering::Relaxed);
                            sample
                        }
                        Err(_) => {
                            metrics
                                .missing_output_frames
                                .fetch_add(1, Ordering::Relaxed);
                            0.0
                        }
                    };

                    peak = peak.max(mono.abs());

                    for sample in frame {
                        *sample = T::from_sample(mono);
                    }
                }

                metrics.store_output_peak(peak);
            },
            err_fn,
            None,
        )
        .context("failed to build output stream")
}
