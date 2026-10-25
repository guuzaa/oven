//! A client that asks the app something and reads the answer off the event
//! stream.
//!
//! The terminal draws every event it receives; a client that only wants the
//! answer reads the same stream and takes what it needs from it. The two
//! share the protocol, not the code: this one answers a question, the
//! terminal answers a screen.

use std::sync::Arc;

use oven_proto::{
    AgentEvent, AgentId, AppEvent, AppEventKind, LoopLimitDecision, ShellEvent, StreamEvent,
    TurnEvent, UserResponse,
};
use tokio::sync::mpsc;

use crate::App;
use crate::core::error::AppError;

/// A client that asks the app one question at a time.
///
/// It subscribes to the app's events when it is built, so it reads the
/// events of the question it is asking and nothing else.
pub struct Headless {
    app: App,
    events: mpsc::UnboundedReceiver<Arc<AppEvent>>,
}

impl Headless {
    pub fn new(app: App) -> Self {
        let events = app.subscribe();
        Self { app, events }
    }

    /// Asks the app `text` and waits for the answer.
    pub async fn ask(&mut self, text: &str) -> Result<String, AppError> {
        self.drain();
        let main = self.app.state().levels.agent_id;
        let input = self.app.classify(text);
        self.app.submit(input).await?;
        self.collect(main).await
    }

    /// Drops the events of the questions already answered, so the answer
    /// this one waits for is the only thing the stream holds.
    fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    }

    /// Reads the answer off the stream.
    async fn collect(&mut self, main: AgentId) -> Result<String, AppError> {
        let mut text = String::new();
        let mut in_turn = false;
        loop {
            let Some(event) = self.events.recv().await else {
                return Err(AppError::ChannelClosed);
            };
            match &event.kind {
                AppEventKind::Agent(env) if env.agent_id == main => match &env.event {
                    AgentEvent::Turn(TurnEvent::Started) => {
                        in_turn = true;
                        text.clear();
                    }
                    AgentEvent::Stream(StreamEvent::TextDelta { text: delta }) => {
                        text.push_str(delta);
                    }
                    AgentEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Cancelled { .. }) => {
                        self.app.wait_until_idle().await;
                        return Ok(text);
                    }
                    AgentEvent::Turn(TurnEvent::Failed { error, .. }) => {
                        self.app.wait_until_idle().await;
                        return Err(AppError::Runtime(error.message.clone()));
                    }
                    AgentEvent::Turn(TurnEvent::LoopLimitReached { request_id, .. }) => {
                        self.app.respond(
                            *request_id,
                            UserResponse::LoopLimit(LoopLimitDecision::Exit),
                        );
                    }
                    _ => {}
                },
                AppEventKind::Shell(ev) => match ev {
                    ShellEvent::Started { .. } => {
                        in_turn = true;
                    }
                    ShellEvent::Finished { output, .. } => {
                        self.app.wait_until_idle().await;
                        return Ok(output.clone());
                    }
                    ShellEvent::Failed { error, output, .. } => {
                        self.app.wait_until_idle().await;
                        return Err(AppError::Runtime(if output.is_empty() {
                            error.clone()
                        } else {
                            output.clone()
                        }));
                    }
                },
                AppEventKind::Notification { text: note } if !in_turn => return Ok(note.clone()),
                AppEventKind::Error { message } => return Err(AppError::Runtime(message.clone())),
                AppEventKind::Exited if !in_turn => {
                    return Ok(if text.is_empty() {
                        "goodbye".into()
                    } else {
                        text
                    });
                }
                _ => {}
            }
        }
    }
}
