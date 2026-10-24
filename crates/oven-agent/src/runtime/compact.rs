//! History compaction: replace the conversation with an LLM-written summary
//! so a long session can continue in a fresh, small context.

use std::sync::Arc;

use futures::StreamExt;
use oven_llm::{
    CallOptions, Client, Message, Provider, ReasoningEffort, Request, Response, StreamCollector,
    ToolChoice,
};
use tokio_util::sync::CancellationToken;

use crate::runtime::agent::Agent;
use oven_proto::AgentError;

pub const NOTHING_TO_COMPACT: &str = "nothing to compact";
const EMPTY_SUMMARY: &str = "provider returned an empty summary";

const COMPACT_PROMPT: &str = "Summarize this conversation so it can be \
continued in a fresh session. Write a compact briefing that preserves:\n\
- the user's overall goal and any explicit requirements or preferences\n\
- what has been done so far (files created or modified, commands run, key results)\n\
- important technical decisions and their reasons\n\
- unfinished work and concrete next steps\n\
Reply with the summary only.";

const SUMMARY_PREAMBLE: &str =
    "This session continues a compacted conversation. Summary of the previous context:";

/// Token counts around a compaction: the prompt side of the summarization
/// request (the context that was compacted) and the summary's size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactStats {
    pub before_tokens: u32,
    pub after_tokens: u32,
}

impl Agent {
    /// Ask the provider to summarize the conversation, then replace the
    /// entire history with a single user message carrying the summary.
    /// Todos are kept and re-shown on the next request.
    ///
    /// `cancel` aborts the summary request, including mid-stream. The history
    /// is left untouched when the request fails or is cancelled.
    pub async fn compact(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<CompactStats, AgentError> {
        if self.history.is_empty() {
            return Err(AgentError::from(NOTHING_TO_COMPACT));
        }
        if cancel.is_cancelled() {
            return Err(AgentError::cancelled());
        }
        let mut req = self.build_request();
        req.messages.push(Message::user_text(COMPACT_PROMPT));
        req.tools = Vec::new();
        req.tool_choice = ToolChoice::None;
        // A summary does not need the thinking the turn had.
        let mut options = self.build_options();
        options.reasoning_effort = Some(ReasoningEffort::Off);

        let response = request_summary(self.router(), req, options, cancel).await?;
        if cancel.is_cancelled() {
            return Err(AgentError::cancelled());
        }
        let summary = response.text();
        if summary.trim().is_empty() {
            return Err(AgentError::from(EMPTY_SUMMARY));
        }
        self.acknowledge_request_notes();
        let usage = response.usage.unwrap_or_default();

        self.history.clear();
        self.history.push(Message::user_text(format!(
            "{SUMMARY_PREAMBLE}\n\n{summary}"
        )));
        self.todo_notice = !self.todos().is_empty();
        let stats = CompactStats {
            before_tokens: usage.input_tokens,
            after_tokens: usage.output_tokens,
        };
        tracing::info!(
            before_tokens = stats.before_tokens,
            after_tokens = stats.after_tokens,
            "history compacted"
        );
        Ok(stats)
    }
}

/// The summary call. Dropping the future aborts it: a streaming model drops
/// the body mid-token, and a non-streaming model drops the in-flight request.
async fn request_summary(
    router: Arc<Client>,
    req: Request,
    options: CallOptions,
    cancel: &CancellationToken,
) -> Result<Response, AgentError> {
    let cancel_work = cancel.clone();
    let work = async move {
        let streams = router
            .resolve_model(&req.model)
            .is_none_or(|info| info.capabilities.supports_streaming);
        if !streams {
            return router
                .complete(&req, &options)
                .await
                .map_err(AgentError::from);
        }
        let mut stream = router.stream(&req, &options).await?;
        let mut collector = StreamCollector::new();
        while let Some(event) = stream.next().await {
            if cancel_work.is_cancelled() {
                return Err(AgentError::cancelled());
            }
            collector.push(&event?);
        }
        if cancel_work.is_cancelled() {
            return Err(AgentError::cancelled());
        }
        collector.finish().map_err(AgentError::from)
    };

    tokio::pin!(work);
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(AgentError::cancelled()),
        result = &mut work => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::Stream;
    use futures::stream::BoxStream;
    use oven_llm::{
        ContentBlock, Delta, ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request,
        Response, Role, Router, StopReason, StreamEvent, Usage,
    };
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    struct SummaryProvider {
        seen: Arc<Mutex<Vec<Request>>>,
    }

    #[async_trait]
    impl Provider for SummaryProvider {
        async fn complete(
            &self,
            req: &Request,
            _options: &CallOptions,
        ) -> Result<Response, ProviderError> {
            self.seen.lock().unwrap().push(req.clone());
            Ok(Response {
                id: "r1".into(),
                model: "mock".into(),
                role: Role::Assistant,
                content: vec![ContentBlock::text("the summary")],
                stop_reason: Some(StopReason::EndTurn),
                usage: Some(Usage {
                    input_tokens: 90_000,
                    output_tokens: 500,
                    cache_read_tokens: 10_000,
                    reasoning_tokens: 0,
                }),
            })
        }

        async fn stream(
            &self,
            _req: &Request,
            _options: &CallOptions,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
            ModelInfo::non_streaming(&self.provider_name(), id)
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("mock".into())
        }
    }

    fn text_of(m: &Message) -> String {
        m.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn agent_with_summary_provider() -> (Agent, Arc<Mutex<Vec<Request>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut router = Router::new();
        router.register(Box::new(SummaryProvider { seen: seen.clone() }));
        (Agent::new(router, Vec::new()), seen)
    }

    #[tokio::test]
    async fn compact_replaces_history_with_summary_message() {
        let (mut agent, seen) = agent_with_summary_provider();
        agent.push_history(Message::user_text("do the thing"));
        agent.push_history(Message::assistant_text("done"));

        let stats = agent.compact(&CancellationToken::new()).await.unwrap();
        assert_eq!(stats.before_tokens, 90_000, "cache reads are input tokens");
        assert_eq!(stats.after_tokens, 500);

        let history: Vec<_> = agent.history().collect();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::User);
        let text = text_of(history[0]);
        assert!(text.starts_with(SUMMARY_PREAMBLE));
        assert!(text.contains("the summary"));

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert!(req.tools.is_empty());
        assert_eq!(req.tool_choice, ToolChoice::None);
        assert_eq!(
            text_of(req.messages.last().unwrap()),
            COMPACT_PROMPT,
            "compaction instruction should be the final message"
        );
        assert_eq!(req.messages.len(), 3);
    }

    #[tokio::test]
    async fn compact_on_empty_history_errors_without_calling_provider() {
        let (mut agent, seen) = agent_with_summary_provider();
        let err = agent.compact(&CancellationToken::new()).await.unwrap_err();
        assert_eq!(err.message, NOTHING_TO_COMPACT);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_compact_keeps_history() {
        struct FailingProvider;

        #[async_trait]
        impl Provider for FailingProvider {
            async fn complete(
                &self,
                _req: &Request,
                _options: &CallOptions,
            ) -> Result<Response, ProviderError> {
                Err(ProviderError::Api {
                    status: 500,
                    body: "boom".into(),
                })
            }
            async fn stream(
                &self,
                _req: &Request,
                _options: &CallOptions,
            ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>
            {
                Err(ProviderError::Api {
                    status: 500,
                    body: "boom".into(),
                })
            }
            fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
                ModelInfo::non_streaming(&self.provider_name(), id)
            }
            fn provider_name(&self) -> ProviderName {
                ProviderName::Custom("mock".into())
            }
        }

        let mut router = Router::new();
        router.register(Box::new(FailingProvider));
        let mut agent = Agent::new(router, Vec::new());
        agent.push_history(Message::user_text("hi"));

        assert!(agent.compact(&CancellationToken::new()).await.is_err());
        assert_eq!(agent.history().len(), 1);
    }

    #[tokio::test]
    async fn compact_on_a_cancelled_token_does_not_call_the_provider() {
        let (mut agent, seen) = agent_with_summary_provider();
        agent.push_history(Message::user_text("hi"));
        let cancel = CancellationToken::new();
        cancel.cancel();

        let err = agent.compact(&cancel).await.unwrap_err();

        assert!(err.is_cancelled());
        assert!(seen.lock().unwrap().is_empty());
        assert_eq!(agent.history().len(), 1);
    }

    #[tokio::test]
    async fn cancel_before_the_summary_returns_leaves_history() {
        let (entered_tx, entered_rx) = oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let mut router = Router::new();
        router.register(Box::new(BlockingComplete {
            entered: Mutex::new(Some(entered_tx)),
            dropped: Arc::clone(&dropped),
        }));
        let mut agent = Agent::new(router, Vec::new());
        agent.push_history(Message::user_text("hi"));
        let cancel = CancellationToken::new();
        let flag = cancel.clone();
        tokio::spawn(async move {
            entered_rx.await.ok();
            flag.cancel();
        });

        let err = tokio::time::timeout(Duration::from_secs(2), agent.compact(&cancel))
            .await
            .expect("cancel should abort the summary before it returns")
            .unwrap_err();

        assert!(err.is_cancelled());
        assert!(
            dropped.load(Ordering::SeqCst),
            "the request future was dropped"
        );
        assert_eq!(text_of(agent.history().next().unwrap()), "hi");
    }

    #[tokio::test]
    async fn cancel_during_summary_stream_leaves_history() {
        const PARTIAL: &str = "partial summary token";
        let (entered_tx, entered_rx) = oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let mut router = Router::new();
        router.register(Box::new(HangingStream {
            events: summary_prefix(PARTIAL),
            entered: Mutex::new(Some(entered_tx)),
            dropped: Arc::clone(&dropped),
        }));
        let mut agent = Agent::new(router, Vec::new());
        agent.push_history(Message::user_text("hi"));
        let cancel = CancellationToken::new();
        let flag = cancel.clone();
        tokio::spawn(async move {
            entered_rx.await.ok();
            flag.cancel();
        });

        let err = tokio::time::timeout(Duration::from_secs(2), agent.compact(&cancel))
            .await
            .expect("cancel should abort the summary mid-stream")
            .unwrap_err();

        assert!(err.is_cancelled());
        assert!(dropped.load(Ordering::SeqCst), "the stream was dropped");
        let history: Vec<_> = agent.history().collect();
        assert_eq!(history.len(), 1);
        assert_eq!(text_of(history[0]), "hi");
        assert!(!text_of(history[0]).contains(PARTIAL));
    }

    #[tokio::test]
    async fn streamed_summary_replaces_history() {
        let mut router = Router::new();
        router.register(Box::new(FinishedStream {
            events: summary_events("the summary"),
        }));
        let mut agent = Agent::new(router, Vec::new());
        agent.push_history(Message::user_text("do the thing"));

        let stats = agent.compact(&CancellationToken::new()).await.unwrap();

        assert_eq!(stats.before_tokens, 90_000);
        assert_eq!(stats.after_tokens, 500);
        let history: Vec<_> = agent.history().collect();
        assert_eq!(history.len(), 1);
        assert!(text_of(history[0]).contains("the summary"));
    }

    fn summary_usage() -> Usage {
        Usage {
            input_tokens: 90_000,
            output_tokens: 500,
            cache_read_tokens: 10_000,
            reasoning_tokens: 0,
        }
    }

    fn summary_prefix(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::MessageStart {
                id: "r1".into(),
                model: "mock".into(),
            },
            StreamEvent::ContentBlockStart {
                index: 0,
                block: ContentBlock::text(""),
            },
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta::TextDelta { text: text.into() },
            },
        ]
    }

    fn summary_events(text: &str) -> Vec<StreamEvent> {
        let mut events = summary_prefix(text);
        events.push(StreamEvent::ContentBlockStop { index: 0 });
        events.push(StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(summary_usage()),
        });
        events.push(StreamEvent::MessageStop);
        events
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct BlockingComplete {
        entered: Mutex<Option<oneshot::Sender<()>>>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Provider for BlockingComplete {
        async fn complete(
            &self,
            _req: &Request,
            _options: &CallOptions,
        ) -> Result<Response, ProviderError> {
            let _flag = DropFlag(Arc::clone(&self.dropped));
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            std::future::pending().await
        }

        async fn stream(
            &self,
            _req: &Request,
            _options: &CallOptions,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
            ModelInfo::non_streaming(&self.provider_name(), id)
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("block".into())
        }
    }

    struct HangingStream {
        events: Vec<StreamEvent>,
        entered: Mutex<Option<oneshot::Sender<()>>>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Provider for HangingStream {
        async fn complete(
            &self,
            _req: &Request,
            _options: &CallOptions,
        ) -> Result<Response, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "complete disabled".into(),
            })
        }

        async fn stream(
            &self,
            _req: &Request,
            _options: &CallOptions,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            let events = self.events.clone();
            let entered = self.entered.lock().unwrap().take();
            let dropped = Arc::clone(&self.dropped);
            Ok(Box::pin(HangAfter {
                events: events.into(),
                entered,
                flag: Some(DropFlag(dropped)),
            }))
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("hang".into())
        }
    }

    struct HangAfter {
        events: std::collections::VecDeque<StreamEvent>,
        entered: Option<oneshot::Sender<()>>,
        flag: Option<DropFlag>,
    }

    impl Stream for HangAfter {
        type Item = Result<StreamEvent, ProviderError>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            if let Some(event) = this.events.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
            if let Some(entered) = this.entered.take() {
                let _ = entered.send(());
            }
            Poll::Pending
        }
    }

    impl Drop for HangAfter {
        fn drop(&mut self) {
            self.flag.take();
        }
    }

    struct FinishedStream {
        events: Vec<StreamEvent>,
    }

    #[async_trait]
    impl Provider for FinishedStream {
        async fn complete(
            &self,
            _req: &Request,
            _options: &CallOptions,
        ) -> Result<Response, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "complete disabled".into(),
            })
        }

        async fn stream(
            &self,
            _req: &Request,
            _options: &CallOptions,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            let events = self.events.clone();
            Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("stream".into())
        }
    }
}
