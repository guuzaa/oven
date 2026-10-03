//! `list_models`: which models this app can run, for the `model` argument of
//! `task`.

use std::fmt::Write;
use std::sync::Arc;

use async_trait::async_trait;
use oven_llm::ModelInfo;
use serde_json::{Value, json};

use crate::capabilities::tools::Tool;
use crate::core::error::AgentError;
use crate::core::models::ModelCatalog;
use crate::core::turn::TurnContext;

const NO_MODELS: &str = "no models are available; configure a provider with /setup";
const UNKNOWN: &str = "unknown";

pub struct ListModelsTool {
    catalog: Arc<dyn ModelCatalog>,
}

impl ListModelsTool {
    pub const NAME: &str = "list_models";

    pub fn new(catalog: Arc<dyn ModelCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl Tool for ListModelsTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "List the models this app can run on, for `task`'s `model` argument. Each line is a \
         model id, its context window and its maximum output."
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }

    async fn run(&self, _args: &Value, _cx: &TurnContext) -> Result<String, AgentError> {
        let models = self.catalog.models();
        if models.is_empty() {
            return Ok(NO_MODELS.to_string());
        }
        Ok(render(&models))
    }
}

fn render(models: &[ModelInfo]) -> String {
    let mut out = String::new();
    for model in models {
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = write!(
            out,
            "{} — context {}, max output {}",
            model.slug(),
            limit(model.context_window),
            limit(model.max_output_tokens)
        );
    }
    out
}

fn limit(tokens: u32) -> String {
    match tokens {
        0 => UNKNOWN.to_string(),
        n => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_llm::{ModelCapabilities, ProviderName};

    struct Catalog(Vec<ModelInfo>);

    impl ModelCatalog for Catalog {
        fn models(&self) -> Vec<ModelInfo> {
            self.0.clone()
        }
    }

    fn model(id: &str, context_window: u32, max_output_tokens: u32) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            provider: ProviderName::DeepSeek,
            context_window,
            max_output_tokens,
            capabilities: ModelCapabilities::default(),
            pricing: None,
            protocols: Vec::new(),
        }
    }

    fn tool(models: Vec<ModelInfo>) -> ListModelsTool {
        ListModelsTool::new(Arc::new(Catalog(models)))
    }

    #[tokio::test]
    async fn lists_each_model_with_its_limits() {
        let out = tool(vec![
            model("deepseek-v4-flash", 1_000_000, 384_000),
            model("model", 0, 0),
        ])
        .run(&json!({}), &TurnContext::for_test())
        .await
        .unwrap();
        assert!(
            out.contains("deepseek/deepseek-v4-flash — context 1000000, max output 384000"),
            "{out}"
        );
        assert!(
            out.contains("deepseek/model — context unknown, max output unknown"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn says_so_when_nothing_is_available() {
        let out = tool(Vec::new())
            .run(&json!({}), &TurnContext::for_test())
            .await
            .unwrap();
        assert_eq!(out, NO_MODELS);
    }

    #[test]
    fn the_schema_takes_no_arguments() {
        let schema = tool(Vec::new()).schema();
        assert!(schema["properties"].as_object().unwrap().is_empty());
    }
}
