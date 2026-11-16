//! Tidy that starts itself: when a turn ends and the inbox holds more notes
//! than `AUTO_TIDY_NOTES`, a background task tidies with the model the turn
//! used. The conversation never waits for it.

use std::sync::Arc;

use oven_agent::Aside;
use oven_mem::MemoryStore;
use oven_proto::AppEventKind;
use tokio::task::JoinHandle;

use super::Runtime;
use crate::core::event::EventBus;
use crate::memory::tidy::{notes_are_due, run};

pub(crate) const TIDY_RUNNING: &str = "tidy is already running";
pub(crate) const RETRY_LATER: &str = "the notes stay for a later run";
const AUTO_PREFIX: &str = "(automatic) ";

/// The background tidy, and whether it has already failed in this run.
#[derive(Default)]
pub(crate) struct AutoTidy {
    task: Option<JoinHandle<bool>>,
    failed: bool,
}

impl AutoTidy {
    pub(crate) fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    /// Collects a task that finished: a failure stops further attempts for
    /// the rest of this run, so a provider that is down is not asked again
    /// every turn.
    async fn settle(&mut self) {
        let Some(task) = self.task.take_if(|task| task.is_finished()) else {
            return;
        };
        self.failed |= matches!(task.await, Ok(false));
    }

    /// Waits for a running task to end. Its model call ends first when the
    /// runtime is shutting down, so what waits here is only applying the plan.
    pub(crate) async fn finish(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Runtime {
    /// Starts a tidy when one is due and none has failed or is running.
    pub(crate) async fn maybe_auto_tidy(&mut self) {
        self.auto_tidy.settle().await;
        if self.auto_tidy.failed
            || self.auto_tidy.is_running()
            || self.shared.shutdown.is_cancelled()
        {
            return;
        }
        let Some(store) = self.memory.clone() else {
            return;
        };
        if !notes_are_due(&store).await {
            return;
        }
        tracing::info!("memory tidy started: the inbox is past its threshold");
        let task = tidy_in_background(
            store,
            self.agent.aside(),
            self.shared.shutdown.child_token(),
            self.shared.events.clone(),
        );
        self.auto_tidy.task = Some(tokio::spawn(task));
    }
}

/// Tidies, tells the user how it went, and says whether it worked.
async fn tidy_in_background(
    store: Arc<MemoryStore>,
    aside: Aside,
    cancel: oven_agent::CancellationToken,
    events: EventBus,
) -> bool {
    let (text, ok) = match run(&store, &aside, &cancel).await {
        Ok(report) => (format!("{AUTO_PREFIX}{report}"), true),
        Err(reason) if cancel.is_cancelled() => {
            tracing::info!(reason, "memory tidy cancelled at shutdown");
            return false;
        }
        Err(reason) => (format!("{AUTO_PREFIX}{reason}; {RETRY_LATER}"), false),
    };
    events.emit(AppEventKind::Notification { text });
    ok
}
