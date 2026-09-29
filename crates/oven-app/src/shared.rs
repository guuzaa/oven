mod live;

pub(crate) use live::{GOODBYE, queued_notice, save_provider_overlay};

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use oven_agent::{
    Agent, AgentEvent, AgentId, AgentMode, CancellationToken, PendingRequest, RequestSink,
    RouterHandle, Selection, ToolEvent, TurnEvent, TurnId, UserRequest, UserRequestId,
    UserResponse,
};
use tokio::sync::watch;

use crate::config::AppConfig;
use crate::event::{AppEventKind, EventBus};
use crate::state::{AppPhase, AppState};
use crate::subagent::Subagents;

/// The turn that is running now: how to stop it, and the one request it is
/// waiting on the user for.
struct ActiveTurn {
    turn_id: TurnId,
    cancel: CancellationToken,
    awaiting: Option<PendingRequest>,
}

/// What the runtime and the `App` handle both reach, so control that never
/// needs the conversation driver — cancelling, switching mode, answering a
/// request, stopping subagents — is applied by the caller instead of queued
/// behind the turn it wants to affect.
pub(crate) struct Shared {
    pub(crate) state: watch::Sender<AppState>,
    pub(crate) events: EventBus,
    pub(crate) subagents: Arc<Subagents>,
    /// Asked to stop: the runtime ends the turn it is driving, then exits.
    pub(crate) shutdown: CancellationToken,
    selection: Selection,
    /// Independent of `&mut Agent`, so `/model` can be validated and
    /// applied while a turn holds the agent's exclusive borrow.
    router: RouterHandle,
    config: Mutex<AppConfig>,
    user_config_path: Option<PathBuf>,
    subagent_revision: Mutex<u64>,
    turn: Mutex<Option<ActiveTurn>>,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn new(
        state: watch::Sender<AppState>,
        events: EventBus,
        subagents: Arc<Subagents>,
        agent: &Agent,
        config: AppConfig,
        user_config_path: Option<PathBuf>,
    ) -> Self {
        Self {
            state,
            events,
            subagents,
            shutdown: CancellationToken::new(),
            selection: agent.selection(),
            router: agent.router_handle(),
            config: Mutex::new(config),
            user_config_path,
            subagent_revision: Mutex::new(0),
            turn: Mutex::new(None),
        }
    }

    pub(crate) fn user_config_path(&self) -> Option<&Path> {
        self.user_config_path.as_deref()
    }

    fn active_turn(&self) -> MutexGuard<'_, Option<ActiveTurn>> {
        self.turn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_phase(&self, phase: AppPhase) {
        self.state.send_modify(|state| state.phase = phase);
    }

    fn move_phase(&self, from: AppPhase, to: AppPhase) {
        self.state.send_if_modified(|state| {
            let moves = state.phase == from;
            if moves {
                state.phase = to;
            }
            moves
        });
    }

    pub(crate) fn begin_turn(&self, turn_id: TurnId, cancel: CancellationToken) {
        *self.active_turn() = Some(ActiveTurn {
            turn_id,
            cancel,
            awaiting: None,
        });
        self.set_phase(AppPhase::Running { turn_id });
    }

    pub(crate) fn end_turn(&self) {
        let pending = self.active_turn().take().and_then(|turn| turn.awaiting);
        if let Some(request) = pending {
            self.emit_resolved(request.request_id);
        }
    }

    pub(crate) fn cancel(&self, turn_id: TurnId) {
        let turn = self.active_turn();
        let Some(active) = turn.as_ref().filter(|active| active.turn_id == turn_id) else {
            return;
        };
        self.set_phase(AppPhase::Cancelling { turn_id });
        active.cancel.cancel();
    }

    pub(crate) fn respond(&self, request_id: UserRequestId, response: UserResponse) {
        let mut turn = self.active_turn();
        let Some(active) = turn.as_mut() else {
            return;
        };
        let Some(request) = active
            .awaiting
            .take_if(|request| request.request_id == request_id)
        else {
            return;
        };
        let turn_id = active.turn_id;
        match request.respond(response) {
            Ok(()) => {
                self.move_phase(
                    AppPhase::Awaiting { turn_id },
                    AppPhase::Running { turn_id },
                );
                self.emit_resolved(request_id);
            }
            Err(request) => active.awaiting = Some(request),
        }
    }

    fn emit_resolved(&self, request_id: UserRequestId) {
        self.events
            .emit(AppEventKind::RequestResolved { request_id });
    }

    pub(crate) fn set_mode(&self, mode: AgentMode) {
        self.selection.set_mode(mode);
        self.state.send_modify(|state| state.mode = mode);
    }

    pub(crate) fn stop_subagent(&self, id: AgentId) {
        if !self.subagents.cancel(id) {
            self.events
                .emit_error(format!("no subagent {id:?} to stop"));
        }
        self.sync_subagents();
    }

    pub(crate) fn stop_subagents(&self) {
        let stopped = self.subagents.active();
        self.subagents.cancel_all();
        self.events.emit(AppEventKind::Notification {
            text: format!("cancelled {stopped} subagents"),
        });
        self.sync_subagents();
    }

    /// Mirrors the subagent registry into published state. The registry is
    /// the truth; the revision is what it read last, so a signal the
    /// registry did not change is dropped without copying the list — a
    /// subagent reports on every tool call it starts.
    pub(crate) fn sync_subagents(&self) {
        let mut revision = self
            .subagent_revision
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let current = self.subagents.revision();
        if current == *revision {
            return;
        }
        *revision = current;
        let snapshot = Arc::new(self.subagents.snapshot());
        self.state.send_modify(|state| state.subagents = snapshot);
    }
}

impl RequestSink for Shared {
    fn submit(&self, turn_id: TurnId, request: PendingRequest) -> bool {
        let event = requested_event(&request);
        let mut turn = self.active_turn();
        let Some(active) = turn.as_mut().filter(|active| active.turn_id == turn_id) else {
            return false;
        };
        let replaced = active
            .awaiting
            .replace(request)
            .map(|pending| pending.request_id);
        self.move_phase(
            AppPhase::Running { turn_id },
            AppPhase::Awaiting { turn_id },
        );
        if let Some(request_id) = replaced {
            self.emit_resolved(request_id);
        }
        let agent_id = self.state.borrow().agent_id;
        self.events.emit_agent(agent_id, turn_id, event);
        true
    }
}

/// Projects a user request onto the event a frontend draws. The agent sends
/// the request and waits; it does not emit the prompt itself.
fn requested_event(request: &PendingRequest) -> AgentEvent {
    let request_id = request.request_id;
    match &request.request {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::PoisonError;

    use crate::event::{AppEvent, AppEventKind};
    use crate::state::SessionState;
    use oven_agent::{Agent, AnswerResponse, ApprovalDecision, LoopLimitDecision};
    use oven_llm::Router;
    use tokio::sync::{mpsc, oneshot};

    fn shared() -> Shared {
        let agent = Agent::new(Router::new(), Vec::new());
        let state = AppState::from_agent(
            &agent,
            Default::default(),
            Vec::new(),
            SessionState::default(),
        );
        let (state, _) = watch::channel(state);
        Shared::new(
            state,
            EventBus::new(),
            Subagents::bare(agent.id(), agent.router_handle()),
            &agent,
            AppConfig::default(),
            None,
        )
    }

    fn phase(shared: &Shared) -> AppPhase {
        shared.state.borrow().phase.clone()
    }

    fn subscribe(shared: &Shared) -> mpsc::UnboundedReceiver<AppEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        shared
            .events
            .subscribers()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tx);
        rx
    }

    fn resolved_ids(rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> Vec<UserRequestId> {
        let mut ids = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AppEventKind::RequestResolved { request_id } = event.kind {
                ids.push(request_id);
            }
        }
        ids
    }

    fn loop_limit() -> (PendingRequest, oneshot::Receiver<LoopLimitDecision>) {
        let (responder, reply) = oneshot::channel();
        let request = PendingRequest {
            request_id: UserRequestId::next(),
            request: UserRequest::LoopLimit {
                max_iters: 1,
                responder,
            },
        };
        (request, reply)
    }

    #[test]
    fn cancel_of_another_turn_leaves_the_running_one_alone() {
        let shared = shared();
        let running = TurnId::next();
        let cancel = CancellationToken::new();
        shared.begin_turn(running, cancel.clone());

        shared.cancel(TurnId::next());

        assert!(!cancel.is_cancelled());
        assert_eq!(phase(&shared), AppPhase::Running { turn_id: running });

        shared.cancel(running);

        assert!(cancel.is_cancelled());
        assert_eq!(phase(&shared), AppPhase::Cancelling { turn_id: running });
    }

    #[test]
    fn cancel_after_the_turn_ended_does_nothing() {
        let shared = shared();
        let turn_id = TurnId::next();
        let cancel = CancellationToken::new();
        shared.begin_turn(turn_id, cancel.clone());
        shared.end_turn();

        shared.cancel(turn_id);

        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn a_request_moves_the_phase_to_awaiting_until_it_is_answered() {
        let shared = shared();
        let turn_id = TurnId::next();
        shared.begin_turn(turn_id, CancellationToken::new());
        let (request, mut reply) = loop_limit();
        let request_id = request.request_id;

        assert!(shared.submit(turn_id, request));
        assert_eq!(phase(&shared), AppPhase::Awaiting { turn_id });

        let mut events = subscribe(&shared);
        shared.respond(request_id, UserResponse::LoopLimit(LoopLimitDecision::Exit));

        assert_eq!(phase(&shared), AppPhase::Running { turn_id });
        assert_eq!(reply.try_recv().unwrap(), LoopLimitDecision::Exit);
        assert_eq!(resolved_ids(&mut events), vec![request_id]);

        shared.end_turn();
        assert!(resolved_ids(&mut events).is_empty());
    }

    #[test]
    fn a_request_for_a_turn_that_is_not_running_is_refused() {
        let shared = shared();
        shared.begin_turn(TurnId::next(), CancellationToken::new());

        assert!(!shared.submit(TurnId::next(), loop_limit().0));
    }

    #[test]
    fn a_reply_of_the_wrong_kind_keeps_the_request_open() {
        let shared = shared();
        let turn_id = TurnId::next();
        shared.begin_turn(turn_id, CancellationToken::new());
        let (request, mut reply) = loop_limit();
        let request_id = request.request_id;
        shared.submit(turn_id, request);

        let mut events = subscribe(&shared);
        shared.respond(
            request_id,
            UserResponse::Approval(ApprovalDecision::Approved),
        );
        shared.respond(request_id, UserResponse::Answer(AnswerResponse::Declined));

        assert_eq!(phase(&shared), AppPhase::Awaiting { turn_id });
        assert!(reply.try_recv().is_err());
        assert!(resolved_ids(&mut events).is_empty());

        shared.respond(
            request_id,
            UserResponse::LoopLimit(LoopLimitDecision::Continue),
        );

        assert_eq!(reply.try_recv().unwrap(), LoopLimitDecision::Continue);
    }

    #[test]
    fn a_reply_to_another_request_is_ignored() {
        let shared = shared();
        let turn_id = TurnId::next();
        shared.begin_turn(turn_id, CancellationToken::new());
        let (request, _reply) = loop_limit();
        shared.submit(turn_id, request);

        shared.respond(
            UserRequestId::next(),
            UserResponse::LoopLimit(LoopLimitDecision::Exit),
        );

        assert_eq!(phase(&shared), AppPhase::Awaiting { turn_id });
    }

    #[test]
    fn respond_with_no_turn_running_does_nothing() {
        let shared = shared();

        shared.respond(
            UserRequestId::next(),
            UserResponse::LoopLimit(LoopLimitDecision::Exit),
        );

        assert_eq!(phase(&shared), AppPhase::Idle);
    }

    #[test]
    fn ending_a_turn_resolves_the_request_it_was_still_holding() {
        let shared = shared();
        let turn_id = TurnId::next();
        shared.begin_turn(turn_id, CancellationToken::new());
        let (request, _reply) = loop_limit();
        let request_id = request.request_id;
        shared.submit(turn_id, request);
        let mut events = subscribe(&shared);

        shared.end_turn();

        assert_eq!(resolved_ids(&mut events), vec![request_id]);
    }

    #[test]
    fn a_later_request_resolves_the_one_it_replaces() {
        let shared = shared();
        let turn_id = TurnId::next();
        shared.begin_turn(turn_id, CancellationToken::new());
        let (first, _reply) = loop_limit();
        let first_id = first.request_id;
        shared.submit(turn_id, first);
        let mut events = subscribe(&shared);
        let (second, _reply) = loop_limit();
        let second_id = second.request_id;

        assert!(shared.submit(turn_id, second));

        assert_eq!(resolved_ids(&mut events), vec![first_id]);
        assert_ne!(first_id, second_id);
    }
}
