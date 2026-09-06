//! History compaction: replace the conversation with an LLM-written summary
//! so a long session can continue in a fresh, small context.

use oven_llm::{Message, Provider, ThinkingMode, ToolChoice};

use crate::agent::Agent;
use crate::error::AgentError;

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
    /// Todos are kept. The history is untouched when the request fails.
    pub async fn compact(&mut self) -> Result<CompactStats, AgentError> {
        if self.history.is_empty() {
            return Err(AgentError::from(NOTHING_TO_COMPACT));
        }
        let mut req = self.build_request();
        req.messages.push(Message::user_text(COMPACT_PROMPT));
        req.tools = Vec::new();
        req.tool_choice = ToolChoice::None;
        req.thinking = Some(ThinkingMode::Disabled);
        req.reasoning_effort = None;

        let router = self.router();
        let response = Provider::complete(&*router, &req).await?;
        let summary = response.text();
        if summary.trim().is_empty() {
            return Err(AgentError::from(EMPTY_SUMMARY));
        }
        let usage = response.usage.unwrap_or_default();

        self.history.clear();
        self.history.push(Message::user_text(format!(
            "{SUMMARY_PREAMBLE}\n\n{summary}"
        )));
        Ok(CompactStats {
            before_tokens: usage.input_tokens.saturating_add(usage.cache_read_tokens),
            after_tokens: usage.output_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use oven_llm::{
        ContentBlock, ModelId, ModelInfo, ProviderError, ProviderName, Request, Response, Role,
        Router, StopReason, StreamEvent, Usage,
    };

    struct SummaryProvider {
        seen: Arc<Mutex<Vec<Request>>>,
    }

    #[async_trait]
    impl Provider for SummaryProvider {
        async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
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
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
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

        let stats = agent.compact().await.unwrap();
        assert_eq!(stats.before_tokens, 100_000);
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
        let err = agent.compact().await.unwrap_err();
        assert_eq!(err.message, NOTHING_TO_COMPACT);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_compact_keeps_history() {
        struct FailingProvider;

        #[async_trait]
        impl Provider for FailingProvider {
            async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
                Err(ProviderError::Api {
                    status: 500,
                    body: "boom".into(),
                })
            }
            async fn stream(
                &self,
                _req: &Request,
            ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>
            {
                Err(ProviderError::Api {
                    status: 500,
                    body: "boom".into(),
                })
            }
            fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
                None
            }
            fn provider_name(&self) -> ProviderName {
                ProviderName::Custom("mock".into())
            }
        }

        let mut router = Router::new();
        router.register(Box::new(FailingProvider));
        let mut agent = Agent::new(router, Vec::new());
        agent.push_history(Message::user_text("hi"));

        assert!(agent.compact().await.is_err());
        assert_eq!(agent.history().len(), 1);
    }
}
