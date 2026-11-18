use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time;

/// The point the process counts time from. It never moves: every run is
/// counted against it, so the runs of one process do not share a clock.
static EPOCH: OnceLock<time::Instant> = OnceLock::new();
/// How far into the epoch the run that is running now started, in
/// nanoseconds.
static BASE: AtomicU64 = AtomicU64::new(0);

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
/// run it happened". Pinning again starts a new run's clock from zero: two
/// runs of one process each count from their own start.
pub fn pin_base() {
    BASE.store(elapsed_nanos(), Ordering::Relaxed);
}

/// Milliseconds since the base was pinned, on a monotonic clock.
///
/// Wall-clock time moves backwards when the clock is corrected, and a gap
/// between two events then stops meaning what it says. A monotonic clock
/// does not move, so the distance between two moments stays true — which is
/// what a timeline of a run is made of.
pub fn monotonic_ms() -> u64 {
    let base = BASE.load(Ordering::Relaxed);
    as_ms(time::Duration::from_nanos(
        elapsed_nanos().saturating_sub(base),
    ))
}

/// Nanoseconds since the epoch the process counts time from.
fn elapsed_nanos() -> u64 {
    let epoch = EPOCH.get_or_init(time::Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_of_a_run_counts_from_the_start_of_that_run() {
        pin_base();
        let first = monotonic_ms();
        std::thread::sleep(time::Duration::from_millis(10));

        assert!(
            monotonic_ms() >= first + 10,
            "the clock of the run does not stand still"
        );

        pin_base();
        assert!(
            monotonic_ms() < first + 10,
            "the next run counts from its own start"
        );
    }
}
