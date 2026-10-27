//! `domain` 层：与 provider 无关的消息 / 请求 / 响应 / 流式事件模型。

pub mod message;
mod model;
pub mod options;
pub mod reasoning;
pub mod request;
pub mod response;
pub mod stream;
pub mod tool;

pub use message::{ContentBlock, ImageSource, Message, Role, ToolArgumentsError, ToolUse};
pub use model::{ModelId, canonical_vendor};
pub use options::{CallOptions, SamplingParams};
pub use reasoning::{LADDER, ParseEffortError, ReasoningEffort};
pub use request::{Request, RequestBuilder};
pub use response::{Response, StopReason, Usage};
pub use stream::{Delta, StreamCollector, StreamCollectorError, StreamEvent, StreamPhase};
pub use tool::{Tool, ToolChoice};
