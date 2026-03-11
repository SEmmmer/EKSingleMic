use crate::{
    pipeline::frames::{AudioFrame, frame_signal},
    util::audio_math::{dbfs_from_linear, rms_linear},
};

const FRAME_WINDOW_SECONDS: f32 = 0.025;
const FRAME_HOP_SECONDS: f32 = 0.010;
const HANGOVER_FRAMES: usize = 3;

#[derive(Debug, Clone)]
pub struct VadFrameDecision {
    pub frame: AudioFrame,
    pub is_active: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct VadEngine {
    activity_threshold_dbfs: f32,
    hangover_frames: usize,
}

impl VadEngine {
    pub fn new(activity_threshold_dbfs: f32) -> Self {
        Self {
            activity_threshold_dbfs,
            hangover_frames: HANGOVER_FRAMES,
        }
    }

    pub fn analyze(&self, samples: &[f32], sample_rate_hz: u32) -> Vec<VadFrameDecision> {
        let frames = frame_signal(samples, sample_rate_hz, FRAME_WINDOW_SECONDS, FRAME_HOP_SECONDS);
        let mut decisions = Vec::with_capacity(frames.len());
        let mut hangover = 0_usize;

        for frame in frames {
            let rms_dbfs = dbfs_from_linear(rms_linear(&samples[frame.start_sample..frame.end_sample]));
            let is_active = if rms_dbfs >= self.activity_threshold_dbfs {
                hangover = self.hangover_frames;
                true
            } else if hangover > 0 {
                hangover -= 1;
                true
            } else {
                false
            };

            decisions.push(VadFrameDecision { frame, is_active });
        }

        decisions
    }

    pub fn activity_threshold_dbfs(&self) -> f32 {
        self.activity_threshold_dbfs
    }
}

