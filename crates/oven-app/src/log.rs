use oven_host::RotatingFile;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::dirs;

const LOG_FILE_NAME: &str = "oven.log";
const OVEN_LOG_ENV: &str = "OVEN_LOG";
const RUST_LOG_ENV: &str = "RUST_LOG";
const DEFAULT_LOG_FILTER: &str = "info";
const LOG_HOME_MISSING: &str = "warning: logging: home directory not found";
const LOG_OPEN_FAILED: &str = "warning: logging:";

pub fn init() {
    let Some(dir) = dirs::logs_dir() else {
        eprintln!("{LOG_HOME_MISSING}");
        return;
    };
    let path = dir.join(LOG_FILE_NAME);
    let file = match RotatingFile::open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("{LOG_OPEN_FAILED} {error}");
            return;
        }
    };
    let _ = tracing_subscriber::registry()
        .with(log_filter())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_writer(move || file.clone()),
        )
        .try_init();
}

fn log_filter() -> EnvFilter {
    let spec = std::env::var(OVEN_LOG_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var(RUST_LOG_ENV).ok().filter(|s| !s.is_empty()));
    match spec {
        Some(spec) => {
            EnvFilter::try_new(spec).unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER))
        }
        None => EnvFilter::new(DEFAULT_LOG_FILTER),
    }
}
