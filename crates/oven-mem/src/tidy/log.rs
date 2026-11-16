//! The OKF `log.md` of a layer: date-grouped entries, newest first.

use std::io;
use std::path::Path;

use chrono::Utc;
use tokio::fs;

const LOG_FILE: &str = "log.md";
const TITLE: &str = "# Directory Update Log";
const DATE_PREFIX: &str = "## ";
const DATE_FORMAT: &str = "%Y-%m-%d";

/// Adds `lines` under today's heading at the top of `dir`'s log.
pub(super) async fn append_log(dir: &Path, lines: &[String]) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let path = dir.join(LOG_FILE);
    let existing = match fs::read_to_string(&path).await {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let date = Utc::now().format(DATE_FORMAT).to_string();
    oven_host::write_atomic(&path, with_entries(&existing, &date, lines)).await
}

fn with_entries(existing: &str, date: &str, lines: &[String]) -> String {
    let heading = format!("{DATE_PREFIX}{date}");
    let entries: String = lines.iter().map(|line| format!("{line}\n")).collect();
    let body = existing
        .strip_prefix(TITLE)
        .unwrap_or(existing)
        .trim_start_matches('\n');
    let rest = match body.strip_prefix(&heading) {
        Some(today) => today.trim_start_matches('\n').to_owned(),
        None if body.is_empty() => String::new(),
        None => format!("\n{body}"),
    };
    let mut out = format!("{TITLE}\n\n{heading}\n{entries}{rest}");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::with_entries;

    #[test]
    fn a_new_log_has_a_title_and_one_dated_section() {
        assert_eq!(
            with_entries("", "2026-11-16", &["* a".into()]),
            "# Directory Update Log\n\n## 2026-11-16\n* a\n"
        );
    }

    #[test]
    fn today_goes_on_top_of_today_and_a_new_day_on_top_of_all() {
        let first = with_entries("", "2026-11-15", &["* old".into()]);
        let second = with_entries(&first, "2026-11-16", &["* a".into()]);
        assert_eq!(
            second,
            "# Directory Update Log\n\n## 2026-11-16\n* a\n\n## 2026-11-15\n* old\n"
        );
        let third = with_entries(&second, "2026-11-16", &["* b".into()]);
        assert_eq!(
            third,
            "# Directory Update Log\n\n## 2026-11-16\n* b\n* a\n\n## 2026-11-15\n* old\n"
        );
    }
}
