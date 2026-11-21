//! How a call is retried, and what it cost to get its answer.
//!
//! The rules live here, but the retry itself belongs to the caller: a
//! caller that wants to know what the retries cost asks for the report
//! alongside the answer. Transport failures, rate limits, and HTTP
//! 408/429/5xx are retried. A timeout is reported as 408 so the same rule
//! covers it. Streaming retries the connection start only. `list_models` is
//! not retried.

use std::future::Future;
use std::time::{Duration, Instant};

use crate::provider::{ProviderError, Result};

const TIMEOUT_STATUS: u16 = 408;
const TOO_MANY_REQUESTS: u16 = 429;
const SERVER_ERROR: u16 = 500;
const DEFAULT_BACKOFF: Duration = Duration::from_millis(500);

/// 指数退避的上限档位：`1 << 16`。base 500ms 时约 9 小时，足够覆盖任何合理的
/// 重试次数，同时保证 `2^shift` 与 `Duration` 乘法都不会溢出。
const MAX_BACKOFF_SHIFT: u32 = 16;

/// How a call is retried: how many times, how long to wait between
/// attempts, and how long one attempt may take.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    timeout: Option<Duration>,
    max_retries: u32,
    base_backoff: Duration,
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

impl RetryPolicy {
    /// No retries, no timeout.
    pub fn new() -> Self {
        Self::default()
    }

    /// How long one attempt may take. A timeout is reported as HTTP 408.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// How many attempts follow the first one.
    pub fn with_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// The wait before the first retry. Every later wait doubles it.
    pub fn with_base_backoff(mut self, base_backoff: Duration) -> Self {
        self.base_backoff = base_backoff;
        self
    }

    /// How long one attempt may take.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

/// One wait between two attempts of a retried call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryWait {
    /// Which retry this was: 1 is the first time the call was asked again.
    pub retry: u32,
    /// How long the call waited, in milliseconds.
    pub waited_ms: u64,
    /// How far into the call the wait ended, in milliseconds, counted from
    /// the start of the call. It is what places the wait on a timeline.
    pub at_ms: u64,
    /// The status the attempt that failed reported, when it had one.
    pub status: Option<u16>,
}

/// What a call did to get its answer: the waits between its attempts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallReport {
    /// The waits between attempts, in order. Empty when the call was not
    /// retried.
    pub waits: Vec<RetryWait>,
}

fn is_retryable(err: &ProviderError) -> bool {
    match err {
        ProviderError::Transport(_) | ProviderError::RateLimit { .. } => true,
        ProviderError::Api { status, .. } => {
            *status >= SERVER_ERROR || *status == TIMEOUT_STATUS || *status == TOO_MANY_REQUESTS
        }
        _ => false,
    }
}

fn status_of(err: &ProviderError) -> Option<u16> {
    match err {
        ProviderError::Api { status, .. } => Some(*status),
        _ => None,
    }
}

/// 第 `attempt` 次重试前的等待时间。`attempt` 从 1 开始计数（`0` 不在调用
/// 路径上）：1 → 1 倍，2 → 2 倍，3 → 4 倍……档位超过
/// [`MAX_BACKOFF_SHIFT`] 后不再增长，并用 `saturating_mul` 兜底，因此
/// `with_retries(u32::MAX)` 也不会 panic 或变成无限等待。
fn backoff_for(policy: &RetryPolicy, attempt: u32, err: &ProviderError) -> Duration {
    if let ProviderError::RateLimit {
        retry_after_ms: Some(ms),
        ..
    } = err
    {
        return Duration::from_millis(*ms);
    }
    let shift = attempt.saturating_sub(1).min(MAX_BACKOFF_SHIFT);
    policy.base_backoff.saturating_mul(1 << shift)
}

fn timeout_error(limit: Duration) -> ProviderError {
    ProviderError::Api {
        status: TIMEOUT_STATUS,
        body: format!("request timed out after {}s", limit.as_secs()),
    }
}

fn log_retry(attempt: u32, backoff: Duration, err: &ProviderError) {
    tracing::warn!(
        attempt,
        backoff_ms = backoff.as_millis() as u64,
        status = status_of(err),
        "retrying provider request"
    );
}

async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

fn exhausted(err: Option<ProviderError>) -> ProviderError {
    err.unwrap_or_else(|| ProviderError::Api {
        status: SERVER_ERROR,
        body: "retry exhausted".into(),
    })
}

/// Runs `op`, retrying what is worth retrying.
///
/// Returns what the call produced and what it took to get it: the report
/// says how many attempts were made and how long the call waited between
/// them. A timeout is the caller's business — it applies one per attempt,
/// because a stream is not timed out the way a whole answer is.
pub async fn with_retry<T, F, Fut>(policy: &RetryPolicy, mut op: F) -> (Result<T>, CallReport)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let started = Instant::now();
    let mut report = CallReport::default();
    let mut last_err = None;
    for attempt in 0..=policy.max_retries {
        if let Some(err) = last_err.as_ref() {
            let backoff = backoff_for(policy, attempt, err);
            log_retry(attempt, backoff, err);
            sleep(backoff).await;
            report.waits.push(RetryWait {
                retry: attempt,
                waited_ms: backoff.as_millis() as u64,
                at_ms: started.elapsed().as_millis() as u64,
                status: status_of(err),
            });
        }
        match op().await {
            Ok(value) => return (Ok(value), report),
            Err(err) => {
                if attempt < policy.max_retries && is_retryable(&err) {
                    last_err = Some(err);
                } else {
                    return (Err(err), report);
                }
            }
        }
    }
    (Err(exhausted(last_err)), report)
}

/// Runs `op` under `limit`, reporting the timeout as HTTP 408.
pub async fn with_timeout<F, T>(limit: Duration, op: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    match tokio::time::timeout(limit, op).await {
        Ok(result) => result,
        Err(_) => Err(timeout_error(limit)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn retries_until_the_call_answers() {
        let calls = Arc::new(Mutex::new(0u32));
        let policy = RetryPolicy::new()
            .with_retries(3)
            .with_base_backoff(Duration::from_millis(1));
        let (outcome, report) = with_retry(&policy, || async {
            let mut seen = calls.lock().unwrap();
            *seen += 1;
            if *seen <= 2 {
                return Err(ProviderError::Api {
                    status: 500,
                    body: "flaky".into(),
                });
            }
            Ok(())
        })
        .await;

        assert!(outcome.is_ok());
        assert_eq!(*calls.lock().unwrap(), 3);
        assert_eq!(report.waits.len(), 2, "two attempts failed first");
    }

    #[tokio::test]
    async fn does_not_retry_an_error_that_is_not_worth_retrying() {
        let calls = Arc::new(Mutex::new(0u32));
        let policy = RetryPolicy::new()
            .with_retries(5)
            .with_base_backoff(Duration::from_millis(1));
        let (outcome, report) = with_retry(&policy, || async {
            let mut seen = calls.lock().unwrap();
            *seen += 1;
            Err::<(), _>(ProviderError::Auth("bad key".into()))
        })
        .await;

        assert!(matches!(outcome, Err(ProviderError::Auth(_))));
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(report.waits.is_empty());
    }

    #[tokio::test]
    async fn a_timeout_is_a_retryable_408() {
        let policy = RetryPolicy::new().with_retries(0);
        let (outcome, _) = with_retry(&policy, || async {
            with_timeout(Duration::from_millis(10), async {
                sleep(Duration::from_millis(50)).await;
                Ok::<(), ProviderError>(())
            })
            .await
        })
        .await;

        assert!(
            matches!(outcome, Err(ProviderError::Api { status: 408, .. })),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn default_does_not_retry() {
        let calls = Arc::new(Mutex::new(0u32));
        let policy = RetryPolicy::new().with_base_backoff(Duration::from_millis(1));
        let (outcome, report) = with_retry(&policy, || async {
            let mut seen = calls.lock().unwrap();
            *seen += 1;
            Err::<(), _>(ProviderError::Api {
                status: 500,
                body: "flaky".into(),
            })
        })
        .await;

        assert!(outcome.is_err());
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(report.waits.is_empty());
    }

    #[test]
    fn backoff_doubles_per_attempt_and_caps_without_overflow() {
        let policy = RetryPolicy {
            timeout: None,
            max_retries: 2,
            base_backoff: DEFAULT_BACKOFF,
        };
        let err = ProviderError::Api {
            status: 500,
            body: "boom".into(),
        };

        assert_eq!(backoff_for(&policy, 1, &err), DEFAULT_BACKOFF);
        assert_eq!(backoff_for(&policy, 2, &err), DEFAULT_BACKOFF * 2);
        assert_eq!(backoff_for(&policy, 3, &err), DEFAULT_BACKOFF * 4);

        let capped = backoff_for(&policy, MAX_BACKOFF_SHIFT + 1, &err);
        assert_eq!(backoff_for(&policy, MAX_BACKOFF_SHIFT + 2, &err), capped);
        assert_eq!(backoff_for(&policy, u32::MAX, &err), capped);
    }

    #[tokio::test]
    async fn the_report_says_what_the_retries_cost() {
        let policy = RetryPolicy::new()
            .with_retries(2)
            .with_base_backoff(Duration::from_millis(5));
        let (outcome, report) = with_retry(&policy, || async {
            Err::<(), _>(ProviderError::Api {
                status: 429,
                body: "slow down".into(),
            })
        })
        .await;

        assert!(outcome.is_err());
        assert_eq!(report.waits.len(), 2);
        assert_eq!(report.waits[0].retry, 1);
        assert_eq!(report.waits[0].waited_ms, 5, "the first wait");
        assert_eq!(report.waits[1].waited_ms, 10, "the second wait doubles");
        assert_eq!(report.waits[0].status, Some(429));
        assert!(
            report.waits[1].at_ms >= report.waits[0].at_ms,
            "the waits happen in order"
        );
    }
}
