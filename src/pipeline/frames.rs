use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};

use crate::util::{audio_math::lerp, time::MODEL_SAMPLE_RATE};

#[derive(Debug, Clone)]
pub struct AudioClip {
    pub sample_rate_hz: u32,
    pub samples: Vec<f32>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AudioFrame {
    pub start_sample: usize,
    pub end_sample: usize,
}

impl AudioClip {
    pub fn read_wav_mono(path: &Path) -> Result<Self> {
        let mut reader = hound::WavReader::open(path)
            .with_context(|| format!("failed to create offline WAV reader: {}", path.display()))?;
        let spec = reader.spec();
        ensure!(
            spec.channels > 0,
            "offline WAV must have at least one channel: {}",
            path.display()
        );

        let interleaved = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .samples::<f32>()
                .map(|sample| {
                    sample
                        .map(|value| value.clamp(-1.0, 1.0))
                        .map_err(|error| anyhow!("failed to decode offline WAV sample: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            hound::SampleFormat::Int => {
                ensure!(
                    spec.bits_per_sample > 0 && spec.bits_per_sample <= 32,
                    "unsupported offline WAV bits per sample {} at {}",
                    spec.bits_per_sample,
                    path.display()
                );
                let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|sample| {
                        sample
                            .map(|value| (value as f32 / scale).clamp(-1.0, 1.0))
                            .map_err(|error| {
                                anyhow!("failed to decode offline WAV sample: {error}")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        };

        let channel_count = spec.channels as usize;
        let mut mono = Vec::with_capacity(interleaved.len() / channel_count.max(1));
        for frame in interleaved.chunks(channel_count.max(1)) {
            let sum = frame.iter().sum::<f32>();
            mono.push(sum / frame.len().max(1) as f32);
        }

        Ok(Self {
            sample_rate_hz: spec.sample_rate,
            samples: mono,
        })
    }

    pub fn write_wav_mono(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create output WAV directory: {}",
                    parent.display()
                )
            })?;
        }

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: self.sample_rate_hz,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec)
            .with_context(|| format!("failed to create output WAV writer: {}", path.display()))?;

        for &sample in &self.samples {
            writer
                .write_sample(sample.clamp(-1.0, 1.0))
                .with_context(|| {
                    format!("failed to write output WAV sample: {}", path.display())
                })?;
        }

        writer
            .finalize()
            .with_context(|| format!("failed to finalize output WAV: {}", path.display()))?;
        Ok(())
    }

    pub fn resample_to_model_rate(&self) -> Self {
        if self.sample_rate_hz == MODEL_SAMPLE_RATE {
            return self.clone();
        }

        Self {
            sample_rate_hz: MODEL_SAMPLE_RATE,
            samples: linear_resample(&self.samples, self.sample_rate_hz, MODEL_SAMPLE_RATE),
        }
    }
}

pub fn frame_signal(
    samples: &[f32],
    sample_rate_hz: u32,
    window_seconds: f32,
    hop_seconds: f32,
) -> Vec<AudioFrame> {
    if samples.is_empty() || sample_rate_hz == 0 {
        return Vec::new();
    }

    let frame_size = ((sample_rate_hz as f32) * window_seconds).round().max(1.0) as usize;
    let hop_size = ((sample_rate_hz as f32) * hop_seconds).round().max(1.0) as usize;

    if samples.len() <= frame_size {
        return vec![AudioFrame {
            start_sample: 0,
            end_sample: samples.len(),
        }];
    }

    let mut frames = Vec::new();
    let mut start = 0_usize;
    while start + frame_size <= samples.len() {
        frames.push(AudioFrame {
            start_sample: start,
            end_sample: start + frame_size,
        });
        start += hop_size;
    }

    if let Some(last) = frames.last()
        && last.end_sample < samples.len()
    {
        frames.push(AudioFrame {
            start_sample: samples.len() - frame_size.min(samples.len()),
            end_sample: samples.len(),
        });
    }

    frames
}

pub fn linear_resample(samples: &[f32], source_rate_hz: u32, target_rate_hz: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate_hz == 0 || target_rate_hz == 0 {
        return Vec::new();
    }

    if source_rate_hz == target_rate_hz {
        return samples.to_vec();
    }

    let output_len = ((samples.len() as f64) * target_rate_hz as f64 / source_rate_hz as f64)
        .round()
        .max(1.0) as usize;
    let step = source_rate_hz as f64 / target_rate_hz as f64;
    let mut output = Vec::with_capacity(output_len);

    for index in 0..output_len {
        let position = index as f64 * step;
        let lower_index = position.floor() as usize;
        let upper_index = (lower_index + 1).min(samples.len() - 1);
        let fraction = (position - lower_index as f64) as f32;
        output.push(lerp(samples[lower_index], samples[upper_index], fraction));
    }

    output
}
