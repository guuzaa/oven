use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use oven_llm::{
    ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request, Response, Result,
    StreamEvent,
};
use tokio::time::{sleep, timeout};

/// Wraps a [`Provider`] adding a per-request timeout and a bounded number of
/// retries (with exponential backoff) for failed calls.
pub struct RetryingProvider {
    inner: Box<dyn Provider>,
    request_timeout: Option<Duration>,
    max_retries: u32,
    base_backoff: Duration,
}

impl RetryingProvider {
    pub fn new(inner: Box<dyn Provider>) -> Self {
        Self {
            inner,
            request_timeout: None,
            max_retries: 0,
            base_backoff: Duration::from_millis(500),
        }
    }

    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.request_timeout = Some(d);
        self
    }

    pub fn with_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    pub fn with_base_backoff(mut self, d: Duration) -> Self {
        self.base_backoff = d;
        self
    }

    fn backoff_for(&self, attempt: u32, err: &ProviderError) -> Duration {
        if let ProviderError::RateLimit {
            retry_after_ms: Some(ms),
        } = err
        {
            return Duration::from_millis(*ms);
        }
        self.base_backoff * 2u32.pow(attempt.saturating_sub(1))
    }
}

fn is_retryable(err: &ProviderError) -> bool {
    match err {
        ProviderError::Transport(_) | ProviderError::RateLimit { .. } => true,
        ProviderError::Api { status, .. } => *status >= 500 || *status == 408 || *status == 429,
        _ => false,
    }
}

async fn attempt_complete(
    inner: &dyn Provider,
    req: &Request,
    request_timeout: Option<Duration>,
) -> Result<Response> {
    let fut = inner.complete(req);
    match request_timeout {
        Some(d) => timeout(d, fut).await.map_err(|_| ProviderError::Api {
            status: 408,
            body: format!("request timed out after {}s", d.as_secs()),
        })?,
        None => fut.await,
    }
}

#[async_trait]
impl Provider for RetryingProvider {
    async fn complete(&self, req: &Request) -> Result<Response> {
        let mut last_err: Option<ProviderError> = None;
        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                let backoff = self.backoff_for(
                    attempt,
                    last_err
                        .as_ref()
                        .expect("retry attempt must follow a prior error"),
                );
                sleep(backoff).await;
            }
            match attempt_complete(self.inner.as_ref(), req, self.request_timeout).await {
                Ok(r) => return Ok(r),
                Err(e) if attempt < self.max_retries && is_retryable(&e) => {
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| ProviderError::Api {
            status: 500,
            body: "retry exhausted".into(),
        }))
    }

    async fn stream(&self, req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        // Streams are long-lived and don't compose cleanly with a hard
        // per-call timeout (timing out would silently truncate the stream).
        // Only the initial connection attempt is retried.
        let mut last_err: Option<ProviderError> = None;
        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                let backoff = self.backoff_for(
                    attempt,
                    last_err
                        .as_ref()
                        .expect("retry attempt must follow a prior error"),
                );
                sleep(backoff).await;
            }
            match self.inner.stream(req).await {
                Ok(s) => return Ok(s),
                Err(e) if attempt < self.max_retries && is_retryable(&e) => {
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| ProviderError::Api {
            status: 500,
            body: "retry exhausted".into(),
        }))
    }

    fn known_models(&self) -> Vec<ModelInfo> {
        self.inner.known_models()
    }

    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        self.inner.resolve_model(id)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        self.inner.list_models().await
    }

    fn provider_name(&self) -> ProviderName {
        self.inner.provider_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_llm::{ContentBlock, Role, StopReason, Usage};
    use std::sync::{Arc, Mutex};

    struct Counting {
        fails_before_success: u32,
        calls: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl Provider for Counting {
        async fn complete(&self, _req: &Request) -> Result<Response> {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            if *c <= self.fails_before_success {
                Err(ProviderError::Api {
                    status: 500,
                    body: "flaky failure".into(),
                })
            } else {
                Ok(Response {
                    id: "1".into(),
                    model: "mock".into(),
                    role: Role::Assistant,
                    content: vec![ContentBlock::text("ok")],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Some(Usage::default()),
                })
            }
        }

        async fn stream(&self, _req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>> {
            unimplemented!()
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("counting".into())
        }
    }

    fn empty_req() -> Request {
        Request {
            model: ModelId::new("mock"),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn retries_until_success() {
        let calls = Arc::new(Mutex::new(0u32));
        let provider = Counting {
            fails_before_success: 2,
            calls: calls.clone(),
        };
        let wrapping = RetryingProvider::new(Box::new(provider))
            .with_retries(3)
            .with_base_backoff(Duration::from_millis(1));
        let r = wrapping.complete(&empty_req()).await.unwrap();
        assert_eq!(r.text(), "ok");
        assert_eq!(*calls.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn retries_exhaust_errors() {
        let calls = Arc::new(Mutex::new(0u32));
        let provider = Counting {
            fails_before_success: 100,
            calls: calls.clone(),
        };
        let wrapping = RetryingProvider::new(Box::new(provider))
            .with_retries(2)
            .with_base_backoff(Duration::from_millis(1));
        let err = wrapping.complete(&empty_req()).await.unwrap_err();
        assert!(matches!(err, ProviderError::Api { status: 500, .. }));
        assert_eq!(*calls.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn does_not_retry_auth_errors() {
        struct AuthFail;
        #[async_trait]
        impl Provider for AuthFail {
            async fn complete(&self, _req: &Request) -> Result<Response> {
                Err(ProviderError::Auth("bad key".into()))
            }
            async fn stream(
                &self,
                _req: &Request,
            ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
                unimplemented!()
            }
            fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
                None
            }
            fn provider_name(&self) -> ProviderName {
                ProviderName::Custom("auth".into())
            }
        }

        let wrapping = RetryingProvider::new(Box::new(AuthFail))
            .with_retries(5)
            .with_base_backoff(Duration::from_millis(1));
        let err = wrapping.complete(&empty_req()).await.unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
    }
}
