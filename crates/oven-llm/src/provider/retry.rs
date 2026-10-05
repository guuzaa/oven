//! Optional per-attempt timeout and bounded retries for [`crate::Router`].
//!
//! Off unless the caller opts in. Transport failures, rate limits, and HTTP
//! 408/429/5xx are retried. A timeout is reported as 408 so the same rule
//! covers it. Streaming retries the connection start only; `list_models` is
//! never passed through here.

use std::time::Duration;

use crate::provider::ProviderError;

const TIMEOUT_STATUS: u16 = 408;
const TOO_MANY_REQUESTS: u16 = 429;
const SERVER_ERROR: u16 = 500;
const DEFAULT_BACKOFF: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub(crate) struct RetryPolicy {
    pub timeout: Option<Duration>,
    pub max_retries: u32,
    pub base_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            timeout: None,
            max_retries: 0,
            base_backoff: DEFAULT_BACKOFF,
        }
    }
}

pub(crate) fn is_retryable(err: &ProviderError) -> bool {
    match err {
        ProviderError::Transport(_) | ProviderError::RateLimit { .. } => true,
        ProviderError::Api { status, .. } => {
            *status >= SERVER_ERROR || *status == TIMEOUT_STATUS || *status == TOO_MANY_REQUESTS
        }
        _ => false,
    }
}

pub(crate) fn status_of(err: &ProviderError) -> Option<u16> {
    match err {
        ProviderError::Api { status, .. } => Some(*status),
        _ => None,
    }
}

pub(crate) fn backoff_for(policy: &RetryPolicy, attempt: u32, err: &ProviderError) -> Duration {
    if let ProviderError::RateLimit {
        retry_after_ms: Some(ms),
    } = err
    {
        return Duration::from_millis(*ms);
    }
    policy.base_backoff * 2u32.pow(attempt.saturating_sub(1))
}

pub(crate) fn timeout_error(limit: Duration) -> ProviderError {
    ProviderError::Api {
        status: TIMEOUT_STATUS,
        body: format!("request timed out after {}s", limit.as_secs()),
    }
}

pub(crate) fn log_retry(attempt: u32, backoff: Duration, err: &ProviderError) {
    tracing::warn!(
        attempt,
        backoff_ms = backoff.as_millis() as u64,
        status = status_of(err),
        "retrying provider request"
    );
}

pub(crate) fn exhausted(err: Option<ProviderError>) -> ProviderError {
    err.unwrap_or_else(|| ProviderError::Api {
        status: SERVER_ERROR,
        body: "retry exhausted".into(),
    })
}
