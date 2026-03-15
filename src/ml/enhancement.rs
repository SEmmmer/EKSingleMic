use crate::{pipeline::frames::AudioFrame, util::audio_math::soft_limit};

#[derive(Debug, Clone, Copy)]
pub struct EnhancementEngine {
    attack_alpha: f32,
    release_alpha: f32,
}

impl Default for EnhancementEngine {
    fn default() -> Self {
        Self {
            attack_alpha: 0.28,
            release_alpha: 0.22,
        }
    }
}

impl EnhancementEngine {
    pub fn smooth_gains(&self, desired_gains: &[f32]) -> Vec<f32> {
        self.smooth_gains_from(desired_gains, None)
    }

    pub fn smooth_gains_from(&self, desired_gains: &[f32], initial_gain: Option<f32>) -> Vec<f32> {
        if desired_gains.is_empty() {
            return Vec::new();
        }

        let mut smoothed = Vec::with_capacity(desired_gains.len());
        let mut current = initial_gain.unwrap_or(desired_gains[0]);
        for &desired in desired_gains {
            let alpha = if desired < current {
                self.attack_alpha
            } else {
                self.release_alpha
            };
            current = current + (desired - current) * alpha;
            smoothed.push(current);
        }

        smoothed
    }

    pub fn apply_frame_gains(
        &self,
        samples: &[f32],
        frames: &[AudioFrame],
        frame_gains: &[f32],
    ) -> Vec<f32> {
        if samples.is_empty() || frames.is_empty() || frame_gains.is_empty() {
            return Vec::new();
        }

        let mut weighted = vec![0.0_f32; samples.len()];
        let mut weights = vec![0.0_f32; samples.len()];

        for (frame, gain) in frames.iter().zip(frame_gains.iter().copied()) {
            for index in frame.start_sample..frame.end_sample.min(samples.len()) {
                weighted[index] += samples[index] * gain;
                weights[index] += 1.0;
            }
        }

        weighted
            .into_iter()
            .zip(weights)
            .zip(samples.iter().copied())
            .map(|((weighted, weight), original)| {
                let gained = if weight > 0.0 {
                    weighted / weight
                } else {
                    original
                };
                soft_limit(gained)
            })
            .collect()
    }
}
