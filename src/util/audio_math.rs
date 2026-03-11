pub fn dbfs_from_linear(sample: f32) -> f32 {
    let magnitude = sample.abs().max(f32::EPSILON);
    20.0 * magnitude.log10()
}
