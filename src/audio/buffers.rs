pub const PASSTHROUGH_LATENCY_MS: u32 = 60;
pub const PASSTHROUGH_RING_BUFFER_MS: u32 = 400;

pub fn latency_samples(sample_rate_hz: u32) -> usize {
    ((sample_rate_hz as usize * PASSTHROUGH_LATENCY_MS as usize) / 1_000).max(256)
}

pub fn ring_capacity_samples(sample_rate_hz: u32) -> usize {
    ((sample_rate_hz as usize * PASSTHROUGH_RING_BUFFER_MS as usize) / 1_000).max(2_048)
}
