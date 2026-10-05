//! Per-provider timeout and retries. Off unless the caller opts in when the
//! provider is built. Transport failures, rate limits, and HTTP 408/429/5xx
//! are retried. A timeout is reported as 408 so the same rule covers it.
//! Streaming retries the connection start only. `list_models` is not retried.

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::domain::{ModelId, Request, Response, StreamEvent};
use crate::provider::model::ModelInfo;
use crate::provider::{Provider, ProviderError, ProviderKind, ProviderName};

const TIMEOUT_STATUS: u16 = 408;
const TOO_MANY_REQUESTS: u16 = 429;
const SERVER_ERROR: u16 = 500;
const DEFAULT_BACKOFF: Duration = Duration::from_millis(500);

/// 指数退避的上限档位：`1 << 16`。base 500ms 时约 9 小时，足够覆盖任何合理的
/// 重试次数，同时保证 `2^shift` 与 `Duration` 乘法都不会溢出。
const MAX_BACKOFF_SHIFT: u32 = 16;

#[derive(Debug, Clone)]
struct RetryPolicy {
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

async fn run<T, F, Fut>(policy: &RetryPolicy, mut op: F) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let mut last_err = None;
    for attempt in 0..=policy.max_retries {
        if let Some(err) = last_err.as_ref() {
            let backoff = backoff_for(policy, attempt, err);
            log_retry(attempt, backoff, err);
            sleep(backoff).await;
        }
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) if attempt < policy.max_retries && is_retryable(&err) => last_err = Some(err),
            Err(err) => return Err(err),
        }
    }
    Err(exhausted(last_err))
}

async fn sleep(duration: Duration) {
    futures_timer::Delay::new(duration).await;
}

async fn with_timeout<Fut, T>(limit: Duration, fut: Fut) -> Result<T, ()>
where
    Fut: Future<Output = T>,
{
    futures_lite::pin!(fut);
    futures_lite::future::or(
        async {
            let value = fut.await;
            Ok(value)
        },
        async {
            sleep(limit).await;
            Err(())
        },
    )
    .await
}

fn exhausted(err: Option<ProviderError>) -> ProviderError {
    err.unwrap_or_else(|| ProviderError::Api {
        status: SERVER_ERROR,
        body: "retry exhausted".into(),
    })
}

/// Wraps a provider with an optional timeout and a bounded retry.
///
/// [`new`](Self::new) does not retry and does not time out. Opt in with
/// [`with_timeout`](Self::with_timeout), [`with_retries`](Self::with_retries),
/// and [`with_base_backoff`](Self::with_base_backoff) before the provider is
/// registered. `list_models` is never retried.
pub struct RetryingProvider {
    inner: Box<dyn Provider>,
    policy: RetryPolicy,
}

impl RetryingProvider {
    pub fn new(inner: impl Provider + 'static) -> Self {
        Self {
            inner: Box::new(inner),
            policy: RetryPolicy::default(),
        }
    }

    /// Timeout for one `complete` attempt. Elapsed time is HTTP 408, so it
    /// is retried when retries are on. `stream` is not timed out.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.policy.timeout = Some(timeout);
        self
    }

    /// Extra attempts after the first failure. `0` (the default) does not retry.
    pub fn with_retries(mut self, max_retries: u32) -> Self {
        self.policy.max_retries = max_retries;
        self
    }

    /// Base for exponential backoff. Default 500ms. A rate-limit `retry_after`
    /// replaces it for that attempt.
    pub fn with_base_backoff(mut self, base_backoff: Duration) -> Self {
        self.policy.base_backoff = base_backoff;
        self
    }

    async fn attempt_complete(&self, req: &Request) -> Result<Response, ProviderError> {
        match self.policy.timeout {
            Some(limit) => match with_timeout(limit, self.inner.complete(req)).await {
                Ok(result) => result,
                Err(()) => Err(timeout_error(limit)),
            },
            None => self.inner.complete(req).await,
        }
    }
}

#[async_trait]
impl Provider for RetryingProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        run(&self.policy, || self.attempt_complete(req)).await
    }

    async fn stream(
        &self,
        req: &Request,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        run(&self.policy, || self.inner.stream(req)).await
    }

    fn known_models(&self) -> Vec<ModelInfo> {
        self.inner.known_models()
    }

    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        self.inner.resolve_model(id)
    }

    fn protocol(&self) -> Option<ProviderKind> {
        self.inner.protocol()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.inner.list_models().await
    }

    fn provider_name(&self) -> ProviderName {
        self.inner.provider_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use crate::domain::message::Role;

    struct Flaky {
        fails_before_success: u32,
        calls: Arc<Mutex<u32>>,
        stream_calls: Arc<Mutex<u32>>,
        list_calls: Arc<Mutex<u32>>,
    }

    fn request() -> Request {
        Request::builder().model("mock").build().unwrap()
    }

    #[async_trait]
    impl Provider for Flaky {
        async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if *calls <= self.fails_before_success {
                return Err(ProviderError::Api {
                    status: 500,
                    body: "flaky".into(),
                });
            }
            Ok(Response {
                id: "1".into(),
                model: req.model.as_str().to_owned(),
                role: Role::Assistant,
                content: Vec::new(),
                stop_reason: None,
                usage: None,
            })
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            let mut calls = self.stream_calls.lock().unwrap();
            *calls += 1;
            if *calls <= self.fails_before_success {
                return Err(ProviderError::Api {
                    status: 500,
                    body: "flaky stream".into(),
                });
            }
            Ok(Box::pin(futures::stream::iter(vec![Ok(
                StreamEvent::MessageStop,
            )])))
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
            *self.list_calls.lock().unwrap() += 1;
            Err(ProviderError::Api {
                status: 500,
                body: "no list".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("flaky".into())
        }
    }

    #[tokio::test]
    async fn retries_complete_until_success() {
        let calls = Arc::new(Mutex::new(0u32));
        let provider = RetryingProvider::new(Flaky {
            fails_before_success: 2,
            calls: Arc::clone(&calls),
            stream_calls: Arc::new(Mutex::new(0)),
            list_calls: Arc::new(Mutex::new(0)),
        })
        .with_retries(3)
        .with_base_backoff(Duration::from_millis(1));
        provider.complete(&request()).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn does_not_retry_auth_errors() {
        struct AuthFail;
        #[async_trait]
        impl Provider for AuthFail {
            async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
                Err(ProviderError::Auth("bad key".into()))
            }
            async fn stream(
                &self,
                _req: &Request,
            ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>
            {
                Err(ProviderError::Auth("bad key".into()))
            }
            fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
                None
            }
            fn provider_name(&self) -> ProviderName {
                ProviderName::Custom("auth".into())
            }
        }

        let provider = RetryingProvider::new(AuthFail)
            .with_retries(5)
            .with_base_backoff(Duration::from_millis(1));
        let err = provider.complete(&request()).await.unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
    }

    #[tokio::test]
    async fn retries_stream_start_not_list_models() {
        let stream_calls = Arc::new(Mutex::new(0u32));
        let list_calls = Arc::new(Mutex::new(0u32));
        let provider = RetryingProvider::new(Flaky {
            fails_before_success: 1,
            calls: Arc::new(Mutex::new(0)),
            stream_calls: Arc::clone(&stream_calls),
            list_calls: Arc::clone(&list_calls),
        })
        .with_retries(3)
        .with_base_backoff(Duration::from_millis(1));
        let stream = provider.stream(&request()).await.unwrap();
        let events: Vec<_> = futures::StreamExt::collect(stream).await;
        assert_eq!(events.len(), 1);
        assert_eq!(*stream_calls.lock().unwrap(), 2);

        let err = provider.list_models().await.unwrap_err();
        assert!(matches!(err, ProviderError::Api { status: 500, .. }));
        assert_eq!(*list_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn complete_timeout_is_a_retryable_408() {
        struct Slow;
        #[async_trait]
        impl Provider for Slow {
            async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
                sleep(Duration::from_millis(50)).await;
                Err(ProviderError::Api {
                    status: 500,
                    body: "late".into(),
                })
            }
            async fn stream(
                &self,
                _req: &Request,
            ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>
            {
                unimplemented!()
            }
            fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
                None
            }
            fn provider_name(&self) -> ProviderName {
                ProviderName::Custom("slow".into())
            }
        }

        let provider = RetryingProvider::new(Slow)
            .with_timeout(Duration::from_millis(10))
            .with_retries(0);
        let err = provider.complete(&request()).await.unwrap_err();
        assert!(
            matches!(err, ProviderError::Api { status: 408, .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn default_does_not_retry() {
        let calls = Arc::new(Mutex::new(0u32));
        let provider = RetryingProvider::new(Flaky {
            fails_before_success: 1,
            calls: Arc::clone(&calls),
            stream_calls: Arc::new(Mutex::new(0)),
            list_calls: Arc::new(Mutex::new(0)),
        });
        let err = provider.complete(&request()).await.unwrap_err();
        assert!(matches!(err, ProviderError::Api { status: 500, .. }));
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn backoff_doubles_per_attempt_and_caps_without_overflow() {
        let policy = RetryPolicy {
            timeout: None,
            max_retries: 0,
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
}
