use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
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
    util::audio_math::dbfs_from_linear,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStage {
    Stopped,
    RunningPassthrough,
}

impl RuntimeStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::RunningPassthrough => "Running (Passthrough)",
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
}

impl Default for RuntimeMetricsSnapshot {
    fn default() -> Self {
        Self {
            input_peak_dbfs: dbfs_from_linear(0.0),
            output_peak_dbfs: dbfs_from_linear(0.0),
            dropped_input_frames: 0,
            missing_output_frames: 0,
        }
    }
}

pub struct PassthroughRuntime {
    _input_stream: Stream,
    _output_stream: Stream,
    metrics: Arc<RuntimeMetrics>,
    format: RuntimeFormatSummary,
}

impl PassthroughRuntime {
    pub fn start(selected_input_device: Option<&str>, selected_output_device: Option<&str>) -> Result<Self> {
        let input_device = find_input_device(selected_input_device)?;
        let output_device = find_output_device(selected_output_device)?;

        let input_name = input_device.name().unwrap_or_else(|_| "Unknown input device".to_owned());
        let output_name = output_device.name().unwrap_or_else(|_| "Unknown output device".to_owned());

        let input_supported = input_device
            .default_input_config()
            .context("failed to query default input config")?;
        let output_supported = select_output_config(&output_device, input_supported.sample_rate())?;

        let input_config = input_supported.config();
        let output_config = output_supported.config();

        let sample_rate_hz = input_config.sample_rate.0;
        let latency = latency_samples(sample_rate_hz);
        let capacity = ring_capacity_samples(sample_rate_hz).max(latency * 4);
        let (mut producer, consumer) = RingBuffer::<f32>::new(capacity);

        for _ in 0..latency {
            let _ = producer.push(0.0);
        }

        let metrics = Arc::new(RuntimeMetrics::default());
        let input_channels = input_config.channels as usize;
        let output_channels = output_config.channels as usize;

        let input_stream = build_input_stream(
            &input_device,
            input_supported.sample_format(),
            &input_config,
            input_channels,
            producer,
            Arc::clone(&metrics),
        )?;
        let output_stream = build_output_stream(
            &output_device,
            output_supported.sample_format(),
            &output_config,
            output_channels,
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
            format: RuntimeFormatSummary {
                input_device_name: input_name,
                output_device_name: output_name,
                sample_rate_hz,
                input_channels: input_config.channels,
                output_channels: output_config.channels,
                input_sample_format: input_supported.sample_format().to_string(),
                output_sample_format: output_supported.sample_format().to_string(),
            },
        })
    }

    pub fn metrics_snapshot(&self) -> RuntimeMetricsSnapshot {
        self.metrics.snapshot()
    }

    pub fn format_summary(&self) -> &RuntimeFormatSummary {
        &self.format
    }
}

#[derive(Default)]
struct RuntimeMetrics {
    input_peak_linear_bits: AtomicU32,
    output_peak_linear_bits: AtomicU32,
    dropped_input_frames: AtomicU64,
    missing_output_frames: AtomicU64,
}

impl RuntimeMetrics {
    fn store_input_peak(&self, value: f32) {
        self.input_peak_linear_bits.store(value.to_bits(), Ordering::Relaxed);
    }

    fn store_output_peak(&self, value: f32) {
        self.output_peak_linear_bits.store(value.to_bits(), Ordering::Relaxed);
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
        }
    }
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
                "selected output device does not support {} Hz; M1 passthrough currently requires equal input/output sample rates",
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
        SampleFormat::I8 => build_input_stream_t::<i8>(device, config, input_channels, producer, metrics),
        SampleFormat::I16 => build_input_stream_t::<i16>(device, config, input_channels, producer, metrics),
        SampleFormat::I24 => build_input_stream_t::<cpal::I24>(device, config, input_channels, producer, metrics),
        SampleFormat::I32 => build_input_stream_t::<i32>(device, config, input_channels, producer, metrics),
        SampleFormat::I64 => build_input_stream_t::<i64>(device, config, input_channels, producer, metrics),
        SampleFormat::U8 => build_input_stream_t::<u8>(device, config, input_channels, producer, metrics),
        SampleFormat::U16 => build_input_stream_t::<u16>(device, config, input_channels, producer, metrics),
        SampleFormat::U32 => build_input_stream_t::<u32>(device, config, input_channels, producer, metrics),
        SampleFormat::U64 => build_input_stream_t::<u64>(device, config, input_channels, producer, metrics),
        SampleFormat::F32 => build_input_stream_t::<f32>(device, config, input_channels, producer, metrics),
        SampleFormat::F64 => build_input_stream_t::<f64>(device, config, input_channels, producer, metrics),
        unsupported => Err(anyhow!("unsupported input sample format for passthrough: {unsupported}")),
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
        SampleFormat::I8 => build_output_stream_t::<i8>(device, config, output_channels, consumer, metrics),
        SampleFormat::I16 => build_output_stream_t::<i16>(device, config, output_channels, consumer, metrics),
        SampleFormat::I24 => build_output_stream_t::<cpal::I24>(device, config, output_channels, consumer, metrics),
        SampleFormat::I32 => build_output_stream_t::<i32>(device, config, output_channels, consumer, metrics),
        SampleFormat::I64 => build_output_stream_t::<i64>(device, config, output_channels, consumer, metrics),
        SampleFormat::U8 => build_output_stream_t::<u8>(device, config, output_channels, consumer, metrics),
        SampleFormat::U16 => build_output_stream_t::<u16>(device, config, output_channels, consumer, metrics),
        SampleFormat::U32 => build_output_stream_t::<u32>(device, config, output_channels, consumer, metrics),
        SampleFormat::U64 => build_output_stream_t::<u64>(device, config, output_channels, consumer, metrics),
        SampleFormat::F32 => build_output_stream_t::<f32>(device, config, output_channels, consumer, metrics),
        SampleFormat::F64 => build_output_stream_t::<f64>(device, config, output_channels, consumer, metrics),
        unsupported => Err(anyhow!("unsupported output sample format for passthrough: {unsupported}")),
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
                        Ok(sample) => sample,
                        Err(_) => {
                            metrics.missing_output_frames.fetch_add(1, Ordering::Relaxed);
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
