use crate::config::ProviderConfig;
use oven_agent::{AgentEvent, AgentMode};
use oven_llm::{Message, Usage};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Id for one long-lived oven-app instance inside a TUI process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AppId(pub u64);

impl AppId {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Events emitted by an app task (agent events plus app lifecycle).
#[derive(Debug, Clone)]
pub enum AppEvent {
    Agent {
        app_id: AppId,
        event: AgentEvent,
    },
    ModelsUpdated {
        app_id: AppId,
        models: Vec<(String, String)>,
    },
    Idle {
        app_id: AppId,
    },
    Error {
        app_id: AppId,
        message: String,
    },
    /// One exchange was rewound by the TUI: `text` is the removed user
    /// message (joined text blocks), `messages` is the truncated history,
    /// and `usage` is the cumulative token usage after the rollback.
    Rewound {
        app_id: AppId,
        text: Option<String>,
        messages: Vec<Message>,
        usage: Usage,
    },
    /// `/exit` asked the process to quit.
    Exit {
        app_id: AppId,
    },
    /// `/setup` applied a new provider config. `api_key` is never included.
    ProviderUpdated {
        app_id: AppId,
        provider: ProviderConfig,
    },
    /// Slash-command informational notify. Shown below the status bar, not
    /// appended to the transcript.
    Notify {
        app_id: AppId,
        text: String,
    },
    ModeChanged {
        app_id: AppId,
        mode: AgentMode,
    },
}

/// Event fan-out for one runtime. Each subscriber gets its own lossless
/// unbounded channel, so a slow UI never silently drops streaming chunks
/// the way a broadcast receiver would when it lags.
pub(crate) type Subscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<AppEvent>>>>;
