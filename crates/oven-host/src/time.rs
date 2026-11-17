use std::sync::OnceLock;
use std::time;

/// The point every monotonic millisecond is counted from.
static BASE: OnceLock<time::Instant> = OnceLock::new();

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

/// Pins the point monotonic milliseconds are counted from.
///
/// The app pins it when it opens a run, so a moment is "how far into the
/// run it happened". Without a pin the clock pins itself the first time it
/// is read.
pub fn pin_base() {
    BASE.get_or_init(time::Instant::now);
}

/// Milliseconds since the base was pinned, on a monotonic clock.
///
/// Wall-clock time moves backwards when the clock is corrected, and a gap
/// between two events then stops meaning what it says. A monotonic clock
/// does not move, so the distance between two moments stays true — which is
/// what a timeline of a run is made of.
pub fn monotonic_ms() -> u64 {
    as_ms(BASE.get_or_init(time::Instant::now).elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_is_pinned_once() {
        pin_base();
        let first = monotonic_ms();
        std::thread::sleep(time::Duration::from_millis(10));
        pin_base();

        assert!(
            monotonic_ms() >= first + 10,
            "pinning again does not move the base"
        );
    }
}
