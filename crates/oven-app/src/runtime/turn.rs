use std::collections::VecDeque;
use std::fmt::Write;
use std::path::Path;
use std::sync::PoisonError;

use oven_agent::{
    AgentEvent, AgentId, CancellationToken, PendingRequest, RouterHandle, ToolEvent, TurnContext,
    TurnEvent, TurnId, UserRequest,
};
use oven_host::run_shell_command;
use oven_llm::{Message, ModelId};
use tokio::sync::{mpsc, watch};

use crate::command::{AppCommand, ControlCommand};
use crate::config::AppConfig;
use crate::event::{AppEventKind, BusSink, EventBus, ShellEvent, SubagentEvent};
use crate::shell;
use crate::slash::{CommandContext, CommandOutcome, Model, ModelDirective, SlashRegistry};
use crate::state::{AppPhase, AppState, StateChange, context_window_of};
use crate::subagent::Subagents;

use super::{
    Control, Runtime, format_model_switched, publish_context_window, report_unsent,
    resolve_model_switch, save_provider_overlay, stop_subagent, stop_subagents, sync_subagents,
};

const EMPTY_SHELL: &str = "empty shell command";
const QUEUED_NOTICE_SUFFIX: &str = "queued: will apply once the current reply finishes";
const REWIND_QUEUED_NOTICE: &str = "rewind queued: will apply once the current reply finishes";

impl Runtime {
    #[allow(
        clippy::too_many_lines,
        reason = "the turn loop must borrow agent fields individually while the turn future runs"
    )]
    pub(crate) async fn start_turn(
        &mut self,
        input: String,
        cmd_rx: &mut mpsc::UnboundedReceiver<AppCommand>,
    ) -> Control {
        if let Some(shell) = shell::ShellInput::parse(&input) {
            return match shell.command() {
                Some(command) => self.run_shell(command.to_string(), cmd_rx).await,
                None => self.reject_empty_shell(),
            };
        }

        match self.slash.parse_and_run(
            &mut CommandContext::with_agent(&mut self.agent, &self.subagents),
            &input,
        ) {
            Ok(CommandOutcome::Passthrough) => {}
            Ok(outcome) => {
                if let Some(name) = self.slash.recognized_name(&input) {
                    tracing::info!(name, "slash command");
                }
                self.apply_slash(outcome).await;
                return Control::Continue;
            }
            Err(e) => {
                self.emit_error(e.to_string());
                return Control::Continue;
            }
        }

        let turn_id = TurnId::next();
        tracing::info!(
            turn_id = turn_id.0,
            mode = self.agent.mode().label(),
            model = %self.agent.model(),
            "turn started"
        );
        self.state.phase = AppPhase::Running { turn_id };
        self.publish();

        let cancel = CancellationToken::new();
        let (requests, mut request_rx) = mpsc::unbounded_channel::<PendingRequest>();
        let mut pending = None;
        let mut sink = BusSink::new(self.events.clone(), self.agent.id(), turn_id);
        let ctx = TurnContext::new(
            turn_id,
            cancel.clone(),
            self.agent.mode(),
            self.agent.model().clone(),
            self.agent.reasoning_effort(),
        )
        .with_policy(self.policy)
        .with_requests(requests);

        let agent_id = self.agent.id();
        let result = {
            let turn = self.agent.run(input, &ctx, &mut sink);
            tokio::pin!(turn);

            loop {
                tokio::select! {
                    biased;
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            None | Some(AppCommand::Shutdown) => {
                                self.state.phase = AppPhase::ShuttingDown;
                                self.subagents.shutdown();
                                report_unsent(&self.pending, &self.unsent);
                                let _ = self.state_tx.send(self.state.clone());
                                cancel.cancel();
                                let _ = turn.await;
                                return Control::Shutdown;
                            }
                            Some(AppCommand::Control(ControlCommand::Cancel { turn_id: id }))
                                if id == turn_id =>
                            {
                                cancel_turn(&mut self.state, &self.state_tx, turn_id, &cancel);
                            }
                            Some(AppCommand::Control(ControlCommand::Cancel { .. })) => {}
                            Some(AppCommand::Control(ControlCommand::StopSubagent { id })) => {
                                stop_subagent(&self.subagents, &self.events, id);
                                sync_subagents(
                                    &self.subagents,
                                    &mut self.subagent_revision,
                                    &mut self.state,
                                    &self.events,
                                    &self.state_tx,
                                );
                            }
                            Some(AppCommand::Control(ControlCommand::StopSubagents)) => {
                                stop_subagents(&self.subagents, &self.events);
                                sync_subagents(
                                    &self.subagents,
                                    &mut self.subagent_revision,
                                    &mut self.state,
                                    &self.events,
                                    &self.state_tx,
                                );
                            }
                            Some(AppCommand::Control(ControlCommand::Respond { request_id, response })) => {
                                if pending
                                    .as_ref()
                                    .is_some_and(|request: &PendingRequest| request.request_id == request_id)
                                    && let Some(request) = pending.take()
                                {
                                    match request.respond(response) {
                                        Ok(()) => {
                                            self.state.phase = AppPhase::Running { turn_id };
                                            let _ = self.state_tx.send(self.state.clone());
                                        }
                                        Err(request) => pending = Some(request),
                                    }
                                }
                            }
                            Some(AppCommand::Control(ControlCommand::SetMode { mode })) => {
                                ctx.set_mode(mode);
                                self.state.mode = mode;
                                let _ = self.state_tx.send(self.state.clone());
                                self.events.emit_state(StateChange::ModeChanged { mode });
                            }

                            Some(cmd) => {
                                let model_args = match &cmd {
                                    AppCommand::Prompt(text) => {
                                        self.slash.args_of(Model::NAME, text)
                                    }
                                    _ => None,
                                };
                                match model_args {
                                    Some(args) => apply_model_during_turn(
                                        args,
                                        &ctx,
                                        &self.router,
                                        &mut self.config,
                                        &mut self.state,
                                        &self.state_tx,
                                        self.user_config_path.as_deref(),
                                        &self.events,
                                    ),
                                    None if shared_command(&cmd, &self.slash, &self.subagents, &self.events) => {}
                                    None => defer_command(
                                        cmd,
                                        &self.slash,
                                        &self.events,
                                        &mut self.pending,
                                    ),
                                }
                            }
                        }
                    }
                    request = request_rx.recv() => {
                        if let Some(request) = request {
                            announce(agent_id, turn_id, &request, &self.events);
                            self.state.phase = AppPhase::Awaiting { turn_id };
                            let _ = self.state_tx.send(self.state.clone());
                            pending = Some(request);
                        }
                    }
                    Some(()) = self.wake_rx.recv() => sync_subagents(
                        &self.subagents,
                        &mut self.subagent_revision,
                        &mut self.state,
                        &self.events,
                        &self.state_tx,
                    ),
                    res = &mut turn => break res,
                }
            }
        };

        if let Some(store) = &self.session {
            self.agent.ensure_session_meta(store.root.clone());
        }

        self.sync_subagents();

        if result.is_ok() {
            self.persist_turn();
        }

        self.sync_state();
        self.emit_context_changed();
        if result.is_ok() && self.should_auto_compact() {
            self.compact_history().await;
        }
        self.state.phase = AppPhase::Idle;
        self.publish();
        Control::Continue
    }

    pub(crate) async fn run_shell(
        &mut self,
        command: String,
        cmd_rx: &mut mpsc::UnboundedReceiver<AppCommand>,
    ) -> Control {
        let turn_id = TurnId::next();
        tracing::info!(turn_id = turn_id.0, "shell started");
        self.state.phase = AppPhase::Running { turn_id };
        self.publish();
        self.emit(AppEventKind::Shell(ShellEvent::Started {
            command: command.clone(),
        }));

        let cancel = CancellationToken::new();
        let root = self.root.clone();
        let run = run_shell_command(&command, &root, shell::HOST_SHELL_TIMEOUT, Some(&cancel));
        tokio::pin!(run);

        let result = loop {
            tokio::select! {
                biased;
                cmd = cmd_rx.recv() => {
                    match cmd {
                        None | Some(AppCommand::Shutdown) => {
                            self.state.phase = AppPhase::ShuttingDown;
                            self.publish();
                            cancel.cancel();
                            let _ = run.await;
                            return Control::Shutdown;
                        }
                        Some(AppCommand::Control(ControlCommand::Cancel { turn_id: id }))
                            if id == turn_id =>
                        {
                            cancel_turn(&mut self.state, &self.state_tx, turn_id, &cancel);
                        }
                        Some(AppCommand::Control(ControlCommand::Cancel { .. })) => {}
                        Some(AppCommand::Control(ControlCommand::SetMode { mode })) => {
                            self.set_mode(mode);
                        }
                        Some(cmd) => {
                            if !shared_command(&cmd, &self.slash, &self.subagents, &self.events) {
                                defer_command(cmd, &self.slash, &self.events, &mut self.pending);
                            }
                        }
                    }
                }
                // A shell command holds the driver as much as a turn does,
                // so a subagent finishing still updates the registry here.
                Some(()) = self.wake_rx.recv() => self.sync_subagents(),
                res = &mut run => break res,
            }
        };

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
        if !matches!(self.state.phase, AppPhase::ShuttingDown) {
            self.state.phase = AppPhase::Idle;
            self.publish();
        }
        Control::Continue
    }

    pub(crate) fn reject_empty_shell(&mut self) -> Control {
        self.emit(AppEventKind::Notification {
            text: EMPTY_SHELL.into(),
        });
        Control::Continue
    }
}

/// Projects a user request onto the event a frontend draws. The agent sends
/// the request and waits; it does not emit the prompt itself.
fn announce(agent_id: AgentId, turn_id: TurnId, request: &PendingRequest, events: &EventBus) {
    let request_id = request.request_id;
    let event = match &request.request {
        UserRequest::ApproveTool {
            call_id,
            name,
            view,
            ..
        } => AgentEvent::Tool(ToolEvent::ApprovalRequested {
            request_id,
            call_id: *call_id,
            name: name.clone(),
            view: view.clone(),
        }),
        UserRequest::LoopLimit { max_iters, .. } => AgentEvent::Turn(TurnEvent::LoopLimitReached {
            request_id,
            max_iters: *max_iters,
        }),
        UserRequest::Question { question, .. } => AgentEvent::Tool(ToolEvent::QuestionAsked {
            request_id,
            question: question.clone(),
        }),
    };
    events.emit_agent(agent_id, turn_id, event);
}

fn cancel_turn(
    state: &mut AppState,
    state_tx: &watch::Sender<AppState>,
    turn_id: TurnId,
    cancel: &CancellationToken,
) {
    state.phase = AppPhase::Cancelling { turn_id };
    let _ = state_tx.send(state.clone());
    cancel.cancel();
}

/// Applies a command that never asks for the driver, returning whether it
/// did. Anything the command wants the driver for — `/clear`, `/setup`,
/// `/compact` — reports itself busy and is deferred instead, so a turn in
/// flight keeps the conversation to itself while a view command like
/// `/agents` still works when the user needs it most.
fn shared_command(
    cmd: &AppCommand,
    slash: &SlashRegistry,
    subagents: &Subagents,
    events: &EventBus,
) -> bool {
    let AppCommand::Prompt(text) = cmd else {
        return false;
    };
    let Some(name) = slash.recognized_name(text) else {
        return false;
    };
    match slash.parse_and_run_shared(subagents, text) {
        Ok(Some(outcome)) => {
            apply_mid_turn(name, outcome, events, subagents);
            true
        }
        Ok(None) => false,
        Err(error) => {
            events.emit_error(error.to_string());
            true
        }
    }
}

/// Applies a command's outcome while a turn holds the driver. Only outcomes a
/// command can reach without the driver arrive here; the fallback keeps a
/// command that forgot its `cx.agent()` guard from doing more than waiting.
fn apply_mid_turn(name: &str, outcome: CommandOutcome, events: &EventBus, subagents: &Subagents) {
    match outcome {
        CommandOutcome::Reply(text) => events.emit(AppEventKind::Notification { text }),
        CommandOutcome::FocusSubagent { id } => {
            events.emit(AppEventKind::Subagent(SubagentEvent::Focus { id }));
        }
        CommandOutcome::Exit => {
            subagents.shutdown();
            events.emit(AppEventKind::Notification {
                text: "goodbye".into(),
            });
            events.emit(AppEventKind::Exited);
        }
        _ => events.emit_error(format!("/{name} has to wait for the current reply")),
    }
}

/// Defers a command that arrived while a turn holds `&mut Agent` exclusively.
/// Anything the user can recognize as a command (a slash command, or a
/// rewind) gets an immediate acknowledgement so the UI never looks stuck
/// while the deferred command waits in `pending`.
fn defer_command(
    cmd: AppCommand,
    slash: &SlashRegistry,
    events: &EventBus,
    pending: &mut VecDeque<AppCommand>,
) {
    if let Some(notice) = deferred_notice(&cmd, slash) {
        events.emit(AppEventKind::Notification { text: notice });
    }
    pending.push_back(cmd);
}

fn deferred_notice(cmd: &AppCommand, slash: &SlashRegistry) -> Option<String> {
    match cmd {
        AppCommand::Control(ControlCommand::Rewind) => Some(REWIND_QUEUED_NOTICE.to_string()),
        AppCommand::Prompt(text) => slash
            .recognized_name(text)
            .map(|name| format!("/{name} {QUEUED_NOTICE_SUFFIX}")),
        AppCommand::Control(
            ControlCommand::Cancel { .. }
            | ControlCommand::SetMode { .. }
            | ControlCommand::Respond { .. }
            | ControlCommand::StopSubagent { .. }
            | ControlCommand::StopSubagents,
        )
        | AppCommand::Shutdown => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_model_during_turn(
    args: &str,
    ctx: &TurnContext,
    router: &RouterHandle,
    config: &mut AppConfig,
    state: &mut AppState,
    state_tx: &watch::Sender<AppState>,
    user_config_path: Option<&Path>,
    events: &EventBus,
) {
    let snapshot = router
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let current_effort = ctx.model().1;
    match Model::resolve(&snapshot, current_effort, args) {
        Ok(ModelDirective::Query) => {
            let (model, effort) = ctx.model();
            events.emit(AppEventKind::Notification {
                text: Model::describe(model.as_str(), effort),
            });
        }
        Ok(ModelDirective::Switch {
            model,
            reasoning_effort,
        }) => {
            let outcome = resolve_model_switch(&snapshot, config, model, reasoning_effort);
            ctx.set_model(
                ModelId::from(outcome.model.as_str()),
                outcome.reasoning_effort,
            );
            state.model.clone_from(&outcome.model);
            state.reasoning_effort = outcome.reasoning_effort;
            let _ = state_tx.send(state.clone());
            events.emit_state(StateChange::ModelChanged {
                model: outcome.model.clone(),
                reasoning_effort: outcome.reasoning_effort,
            });
            publish_context_window(
                state,
                state_tx,
                context_window_of(&snapshot, &outcome.model),
                events,
            );
            let saved = save_provider_overlay(user_config_path, &outcome.overlay, events);
            let mut text = format_model_switched(&outcome.model, outcome.reasoning_effort);
            if let Some(path) = saved {
                let _ = write!(text, "\nsaved to {}", path.display());
            }
            events.emit(AppEventKind::Notification { text });
        }
        Err(e) => events.emit_error(e.to_string()),
    }
}
