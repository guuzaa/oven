use oven_agent::{CancellationToken, TurnContext, TurnId};
use oven_host::run_shell_command;
use oven_llm::Message;

use crate::commands::CommandContext;
use crate::core::event::{AppEventKind, BusSink, ShellEvent};
use crate::core::state::AppPhase;
use crate::platform::shell;

use super::Runtime;

const EMPTY_SHELL: &str = "empty shell command";

impl Runtime {
    pub(crate) async fn start_turn(&mut self, input: String) {
        let turn_id = TurnId::next();
        tracing::info!(
            turn_id = turn_id.0,
            mode = self.agent.mode().label(),
            model = %self.agent.model(),
            "turn started"
        );
        let cancel = CancellationToken::new();
        self.shared.begin_turn(turn_id, cancel.clone());
        let mut sink = BusSink::new(self.shared.events.clone(), self.agent.id(), turn_id);
        let ctx = TurnContext::new(turn_id, cancel.clone(), self.agent.selection())
            .with_policy(self.policy)
            .with_requests(self.shared.clone());

        let result = {
            let turn = self.agent.run(input, &ctx, &mut sink);
            tokio::pin!(turn);

            loop {
                tokio::select! {
                    biased;
                    () = self.shared.shutdown.cancelled() => {
                        self.shared.set_phase(AppPhase::ShuttingDown);
                        self.shared.subagents.shutdown();
                        cancel.cancel();
                        break turn.await;
                    }
                    Some(()) = self.wake_rx.recv() => self.shared.sync_subagents(),
                    res = &mut turn => break res,
                }
            }
        };
        self.shared.end_turn();

        if let Some(store) = &self.session {
            self.agent.ensure_session_meta(store.root.clone());
        }

        self.shared.sync_subagents();

        if result.is_ok() {
            self.persist_turn();
        }

        self.sync_state();
        if result.is_ok() && self.should_auto_compact() {
            self.compact_history().await;
        }
        self.shared.set_phase(AppPhase::Idle);
    }

    pub(crate) async fn run_slash(&mut self, name: &str, args: &str) {
        let cx = &mut CommandContext::with_agent(&mut self.agent, &self.shared.subagents);
        match self.slash.run(cx, name, args) {
            Ok(outcome) => {
                tracing::info!(name, "slash command");
                self.apply_slash(outcome).await;
            }
            Err(e) => self.emit_error(e.to_string()),
        }
    }

    pub(crate) async fn run_shell(&mut self, command: String) {
        let turn_id = TurnId::next();
        tracing::info!(turn_id = turn_id.0, "shell started");
        let cancel = CancellationToken::new();
        self.shared.begin_turn(turn_id, cancel.clone());
        self.emit(AppEventKind::Shell(ShellEvent::Started {
            command: command.clone(),
        }));

        let root = self.root.clone();
        let run = run_shell_command(&command, &root, shell::HOST_SHELL_TIMEOUT, Some(&cancel));
        tokio::pin!(run);

        let result = loop {
            tokio::select! {
                biased;
                () = self.shared.shutdown.cancelled() => {
                    self.shared.set_phase(AppPhase::ShuttingDown);
                    cancel.cancel();
                    break run.await;
                }
                // A shell command holds the driver as much as a turn does,
                // so a subagent finishing still updates the registry here.
                Some(()) = self.wake_rx.recv() => self.shared.sync_subagents(),
                res = &mut run => break res,
            }
        };
        self.shared.end_turn();

        if let Some(store) = &self.session {
            self.agent.ensure_session_meta(store.root.clone());
        }

        let shell = shell::commit_shell(&command, result);
        match &shell.error {
            None => {
                let exit_code = shell.exit_code.unwrap_or(0);
                tracing::info!(exit_code, "shell finished");
                self.emit(AppEventKind::Shell(ShellEvent::Finished {
                    command: command.clone(),
                    output: shell.output.clone(),
                    exit_code,
                }));
            }
            Some(error) => {
                tracing::warn!(error = %error, "shell failed");
                self.emit(AppEventKind::Shell(ShellEvent::Failed {
                    command: command.clone(),
                    error: error.clone(),
                    output: shell.output.clone(),
                }));
            }
        }

        self.agent
            .push_history(Message::user_text(shell.to_string()));
        self.persist_turn();
        self.sync_state();
        self.shared.set_phase(AppPhase::Idle);
    }

    pub(crate) fn reject_empty_shell(&self) {
        self.emit(AppEventKind::Notification {
            text: EMPTY_SHELL.into(),
        });
    }
}
