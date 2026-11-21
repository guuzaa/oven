//! The values of a trace, as a person reads them.

use std::time::SystemTime;

use oven_app::session::short_session_id;

/// How many characters of a piece of text are worth showing.
const TEXT_WIDTH: usize = 160;
const MILLIS_PER_SECOND: u64 = 1000;
const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * 60;
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
const BYTES_PER_KILOBYTE: u64 = 1024;
const BYTES_PER_MEGABYTE: u64 = 1024 * 1024;

/// How long ago the trace was written.
pub fn age(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "unknown".to_owned();
    };
    let Ok(since) = SystemTime::now().duration_since(modified) else {
        return "just now".to_owned();
    };
    let seconds = since.as_secs();
    match seconds {
        0 => "just now".to_owned(),
        1 => "a second ago".to_owned(),
        n if n < SECONDS_PER_MINUTE => format!("{n} seconds ago"),
        n if n < SECONDS_PER_HOUR => {
            let minutes = n / SECONDS_PER_MINUTE;
            let word = plural(minutes, "minute", "minutes");
            format!("{minutes} {word} ago")
        }
        n if n < SECONDS_PER_DAY => {
            let hours = n / SECONDS_PER_HOUR;
            let word = plural(hours, "hour", "hours");
            format!("{hours} {word} ago")
        }
        n => {
            let days = n / SECONDS_PER_DAY;
            let word = plural(days, "day", "days");
            format!("{days} {word} ago")
        }
    }
}

/// How much a trace holds.
pub fn bytes(size: u64) -> String {
    match size {
        b if b < BYTES_PER_KILOBYTE => format!("{b} B"),
        b if b < BYTES_PER_MEGABYTE => format!("{:.1} KB", b as f64 / BYTES_PER_KILOBYTE as f64),
        b => format!("{:.1} MB", b as f64 / BYTES_PER_MEGABYTE as f64),
    }
}

/// How long something took, in the units a person reads.
pub fn span(millis: u64) -> String {
    if millis < MILLIS_PER_SECOND {
        return format!("{millis}ms");
    }
    let seconds = millis / MILLIS_PER_SECOND;
    if seconds < SECONDS_PER_MINUTE {
        return format!("{:.2}s", millis as f64 / MILLIS_PER_SECOND as f64);
    }
    if seconds < SECONDS_PER_HOUR {
        return format!(
            "{}m {:02}s",
            seconds / SECONDS_PER_MINUTE,
            seconds % SECONDS_PER_MINUTE
        );
    }
    if seconds < SECONDS_PER_DAY {
        return format!(
            "{}h {:02}m",
            seconds / SECONDS_PER_HOUR,
            (seconds % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE
        );
    }
    format!(
        "{}d {:02}h",
        seconds / SECONDS_PER_DAY,
        (seconds % SECONDS_PER_DAY) / SECONDS_PER_HOUR
    )
}

/// The short form of a piece of text: one line, and enough to tell what it
/// was.
pub fn brief(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(TEXT_WIDTH) {
        None => text,
        Some((at, _)) => format!("{}…", &text[..at]),
    }
}

/// How many characters of a tool call's summary are worth showing.
const HEADLINE_WIDTH: usize = 100;

/// The head of a long text, such as a command: its first line, cut at the
/// width, with `…` when there is more.
pub fn headline(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    let first = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let more = lines.next().is_some();
    match first.char_indices().nth(HEADLINE_WIDTH) {
        None if !more => first,
        None => format!("{first} …"),
        Some((at, _)) => format!("{}…", &first[..at]),
    }
}

/// The short form of an id: the last few characters, the way the app names
/// a session in a log line.
pub fn short_id(id: Option<&str>) -> String {
    match id {
        Some(id) => short_session_id(id).to_owned(),
        None => "-".to_owned(),
    }
}

/// The width of the widest of the given strings, or of the header.
pub fn width_of<'a>(values: impl Iterator<Item = &'a str>, header: &str) -> usize {
    values
        .map(|value| value.chars().count())
        .chain(std::iter::once(header.chars().count()))
        .max()
        .unwrap_or_default()
}

/// The word that fits the count: one of them, or more than one.
pub fn plural(count: u64, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_age_of_one_hour_is_one_hour() {
        let hour_ago = SystemTime::now() - std::time::Duration::from_secs(60 * 60);
        assert_eq!(age(Some(hour_ago)), "1 hour ago");
    }

    #[test]
    fn the_time_reads_in_the_units_a_person_reads() {
        assert_eq!(span(0), "0ms");
        assert_eq!(span(940), "940ms");
        assert_eq!(span(1_500), "1.50s");
        assert_eq!(span(65_000), "1m 05s");
        assert_eq!(span(3_700_000), "1h 01m");
        assert_eq!(span(90_000_000), "1d 01h");
    }

    #[test]
    fn a_long_text_is_cut_at_the_width() {
        let text = "x".repeat(TEXT_WIDTH + 10);
        let brief = brief(&text);
        assert_eq!(brief.chars().count(), TEXT_WIDTH + 1);
        assert!(brief.ends_with('…'), "{brief}");
    }

    #[test]
    fn a_headline_is_the_first_line_and_says_when_there_is_more() {
        assert_eq!(headline("git commit\n-m message"), "git commit …");
        assert_eq!(headline("git commit"), "git commit");
        let long = headline(&"x".repeat(HEADLINE_WIDTH + 10));
        assert_eq!(long.chars().count(), HEADLINE_WIDTH + 1);
    }

    #[test]
    fn a_text_with_lines_in_it_stays_one_line() {
        assert_eq!(
            brief("the first line\nthe second line"),
            "the first line the second line"
        );
    }
}
