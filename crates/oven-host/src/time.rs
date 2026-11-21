use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time;

/// The point the process counts time from. It never moves: the
/// nanoseconds it hands out stay comparable across every run of one
/// process.
static EPOCH: OnceLock<time::Instant> = OnceLock::new();
/// How far into the epoch the run that is running now started, in
/// nanoseconds. One base for the whole process: [`pin_base`] moves it for
/// every clock at once.
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
/// run it happened". The base is one per process: pinning it again starts
/// every clock over, so one process runs one run at a time.
pub fn pin_base() {
    BASE.store(elapsed_nanos(), Ordering::Relaxed);
}

/// Milliseconds between the base a run counts from and a moment, both in
/// the nanoseconds [`elapsed_nanos`] hands out. A moment before the base
/// reads as zero.
fn ms_between(base_nanos: u64, moment_nanos: u64) -> u64 {
    as_ms(time::Duration::from_nanos(
        moment_nanos.saturating_sub(base_nanos),
    ))
}

/// Milliseconds since the base was pinned, on a monotonic clock.
///
/// Wall-clock time moves backwards when the clock is corrected, and a gap
/// between two events then stops meaning what it says. A monotonic clock
/// does not move, so the distance between two moments stays true — which is
/// what a timeline of a run is made of.
pub fn monotonic_ms() -> u64 {
    ms_between(BASE.load(Ordering::Relaxed), elapsed_nanos())
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
    fn a_moment_is_counted_from_the_base_of_its_run() {
        assert_eq!(ms_between(1_000_000_000, 1_500_000_000), 500);
    }

    #[test]
    fn a_moment_before_the_base_reads_as_zero() {
        assert_eq!(ms_between(1_500_000_000, 1_000_000_000), 0);
    }

    #[test]
    fn the_clock_of_a_run_never_goes_backwards() {
        pin_base();
        let first = monotonic_ms();
        assert!(
            monotonic_ms() >= first,
            "the clock of the run does not go back"
        );
    }
}
