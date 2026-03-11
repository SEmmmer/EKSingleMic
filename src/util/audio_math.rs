pub fn dbfs_from_linear(sample: f32) -> f32 {
    let magnitude = sample.abs().max(f32::EPSILON);
    20.0 * magnitude.log10()
}

pub fn rms_linear(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let mean_square = samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
    mean_square.sqrt()
}

pub fn soft_limit(sample: f32) -> f32 {
    sample.tanh()
}

pub fn lerp(start: f32, end: f32, amount: f32) -> f32 {
    start + (end - start) * amount
}
