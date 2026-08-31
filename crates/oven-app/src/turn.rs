use oven_agent::{
    AgentEvent, AgentEventEnvelope, CancellationToken, ChannelEventSink, TurnContext, TurnId,
};
use tokio::sync::mpsc;

use crate::command::AppCommand;
use crate::event::{AppEventKind, Subscribers};
use crate::runtime::{
    Control, Runtime, emit, emit_error, emit_state, persist_todo_snapshot, publish,
    record_recent_path, should_persist_todos,
};
use crate::shell;
use crate::slash::CommandOutcome;
use crate::state::{AppPhase, AppState, StateChange};

impl Runtime {
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

        match self.slash.parse_and_run(&mut self.agent, &input) {
            Ok(CommandOutcome::Passthrough) => {}
            Ok(outcome) => {
                self.apply_slash(outcome).await;
                return Control::Continue;
            }
            Err(e) => {
                self.emit_error(e.to_string());
                return Control::Continue;
            }
        }

        let turn_id = TurnId::next();
        self.state.phase = AppPhase::Running { turn_id };
        self.publish();

        let cancel = CancellationToken::new();
        let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();
        let mut sink = ChannelEventSink::new(agent_tx, self.agent.id(), turn_id);
        let ctx = TurnContext::new(turn_id, cancel.clone(), self.agent.mode());

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
                                publish(&self.state_tx, &self.state);
                                cancel.cancel();
                                let _ = turn.await;
                                return Control::Shutdown;
                            }
                            Some(AppCommand::Cancel { turn_id: id }) if id == turn_id => {
                                cancel_turn(&mut self.state, &self.state_tx, turn_id, &cancel);
                            }
                            Some(AppCommand::Cancel { .. }) => {}
                            Some(AppCommand::SetMode { mode }) => {
                                ctx.set_mode(mode);
                                self.state.mode = mode;
                                publish(&self.state_tx, &self.state);
                                emit_state(
                                    &mut self.seq,
                                    &mut self.state_rev,
                                    &self.subscribers,
                                    StateChange::ModeChanged { mode },
                                );
                            }
                            Some(cmd) => self.pending.push_back(cmd),
                        }
                    }
                    ev = agent_rx.recv() => {
                        match ev {
                            Some(event) => forward_agent_event(
                                event,
                                &mut self.seq,
                                &mut self.state_rev,
                                &self.subscribers,
                                &mut self.state,
                                &self.state_tx,
                            ),
                            None => break turn.await,
                        }
                    }
                    res = &mut turn => break res,
                }
            }
        };

        if let Some(store) = &self.session {
            self.agent.ensure_session_meta(store.root.clone());
        }

        while let Ok(event) = agent_rx.try_recv() {
            forward_agent_event(
                event,
                &mut self.seq,
                &mut self.state_rev,
                &self.subscribers,
                &mut self.state,
                &self.state_tx,
            );
        }

        if result.is_ok() {
            self.persist_turn();
        }

        self.snapshot_agent();
        self.state.phase = AppPhase::Idle;
        self.publish();
        Control::Continue
    }

    pub(crate) fn persist_turn(&mut self) {
        let Some(store) = &self.session else {
            return;
        };
        let rev = self.agent.history_revision();
        let after = self.agent.history_records();
        if rev != self.persisted_rev {
            self.persisted_prefix = 0;
            self.persisted_rev = rev;
        } else if after.len() > self.persisted_prefix {
            if let Err(e) = store
                .current()
                .append_records(&after[self.persisted_prefix..])
            {
                emit_error(&mut self.seq, &self.subscribers, e.to_string());
            } else {
                store.mark_content(true);
                self.persisted_prefix = after.len();
                if let Err(e) = record_recent_path(store) {
                    emit_error(&mut self.seq, &self.subscribers, e.to_string());
                }
            }
        }
        if should_persist_todos(self.agent.todos(), self.agent.todo_written_this_turn())
            && let Err(e) = persist_todo_snapshot(store, self.agent.todos())
        {
            emit_error(&mut self.seq, &self.subscribers, e.to_string());
        }
        self.sync_session_id();
        self.publish();
    }
}

pub(crate) fn cancel_turn(
    state: &mut AppState,
    state_tx: &tokio::sync::watch::Sender<AppState>,
    turn_id: TurnId,
    cancel: &CancellationToken,
) {
    state.phase = AppPhase::Cancelling { turn_id };
    publish(state_tx, state);
    cancel.cancel();
}

fn forward_agent_event(
    event: AgentEventEnvelope,
    seq: &mut u64,
    state_rev: &mut u64,
    subs: &Subscribers,
    state: &mut AppState,
    state_tx: &tokio::sync::watch::Sender<AppState>,
) {
    if let AgentEvent::TodosChanged { todos } = &event.event {
        state.todos = todos.clone();
        publish(state_tx, state);
        emit_state(
            seq,
            state_rev,
            subs,
            StateChange::TodosChanged {
                todos: todos.clone(),
            },
        );
    }
    emit(seq, subs, AppEventKind::Agent(event));
}
