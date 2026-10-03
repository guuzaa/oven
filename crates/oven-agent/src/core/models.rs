//! The models an app can route a request to.

use oven_llm::ModelInfo;

/// What an app can run on. `list_models` renders it and `task` validates a
/// requested model against it, so the two cannot disagree about what exists.
pub trait ModelCatalog: Send + Sync {
    fn models(&self) -> Vec<ModelInfo>;
}
