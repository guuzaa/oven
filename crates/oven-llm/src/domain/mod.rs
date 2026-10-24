//! `domain` 层：与 provider 无关的消息 / 请求 / 响应 / 流式事件模型。

pub mod message;
pub mod options;
pub mod reasoning;
pub mod request;
pub mod response;
pub mod stream;
pub mod tool;

pub use message::{ContentBlock, ImageSource, Message, Role};
pub use options::{CallOptions, SamplingParams};
pub use reasoning::{LADDER, ParseEffortError, ReasoningEffort};
pub use request::{BuilderError, ModelId, Request, RequestBuilder, canonical_vendor};
pub use response::{Response, StopReason, Usage};
pub use stream::{Delta, StreamCollector, StreamCollectorError, StreamEvent, StreamPhase};
pub use tool::{Tool, ToolChoice};
