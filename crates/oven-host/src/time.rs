use std::time;

/// Current wall-clock time as Unix milliseconds.
pub fn now_ms() -> u64 {
    time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .map_or(0, as_ms)
}

/// Whole milliseconds of a duration, saturating instead of truncating.
pub fn as_ms(duration: time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
