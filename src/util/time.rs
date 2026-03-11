use anyhow::{Context, Result};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub const MODEL_SAMPLE_RATE: u32 = 16_000;

pub fn current_utc_timestamp_rfc3339() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("failed to format current UTC timestamp")
}
