//! Subagent supervision: one registry, one concurrency cap, one task each.
//!
//! A subagent runs its own turn in its own task, so it never enters the
//! runtime's phase: it is spawned, watched and cancelled through here. The
//! registry is the single place that knows what subagents exist and how they
//! are doing — published state mirrors it, `/agents` reads it, and
//! `task_output` reads it — so the three can never disagree.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Instant;

use async_trait::async_trait;
use oven_agent::{
    Agent, AgentError, AgentEvent, AgentId, CancellationToken, EventSink, NodeHandle, NodeInfo,
    NodeOutcome, NodeReport, NodeStatus, RoleSpec, RouterHandle, RunPolicy, SpawnRequest,
    SubagentSpawner, Tool, ToolEvent, TurnContext, TurnEvent, TurnId,
};
use oven_host::{as_ms, now_ms};
use oven_llm::Usage;
use tokio::sync::{Semaphore, mpsc, oneshot};

use crate::core::event::{BusSink, EventBus};
use tokio::task::JoinHandle;

/// Finished subagents kept for `/agents` and `task_output` before the oldest
/// are dropped. Running ones are never pruned.
const MAX_FINISHED: usize = 8;

/// A role a subagent can run as: the tools it may use and what it is told.
pub(crate) struct Role {
    pub(crate) spec: RoleSpec,
    tools: Vec<Arc<dyn Tool>>,
    system: String,
}

impl Role {
    pub(crate) fn new(spec: RoleSpec, tools: Vec<Arc<dyn Tool>>, system: String) -> Self {
        Self {
            spec,
            tools,
            system,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.spec.name
    }
}

pub(crate) struct SubagentParts {
    pub(crate) parent: AgentId,
    pub(crate) router: RouterHandle,
    pub(crate) roles: Vec<Role>,
    pub(crate) max_concurrent: usize,
    pub(crate) max_iters: usize,
    pub(crate) events: EventBus,
    pub(crate) wake: mpsc::UnboundedSender<()>,
}

pub(crate) struct Subagents {
    /// Its own weak handle, so a spawned task can own the supervisor without
    /// the spawner having to be handed one.
    me: Weak<Subagents>,
    registry: Mutex<Registry>,
    /// Permits bound how many subagents run at once; a spawn without one
    /// waits in `Pending` rather than being refused.
    slots: Semaphore,
    parent: AgentId,
    router: RouterHandle,
    roles: Vec<Role>,
    max_iters: usize,
    events: EventBus,
    wake: mpsc::UnboundedSender<()>,
    /// Cancelled when the app shuts down, which stops every subagent.
    root: CancellationToken,
}

impl Subagents {
    pub(crate) fn new(parts: SubagentParts) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            registry: Mutex::new(Registry::default()),
            slots: Semaphore::new(parts.max_concurrent.max(1)),
            parent: parts.parent,
            router: parts.router,
            roles: parts.roles,
            max_iters: parts.max_iters,
            events: parts.events,
            wake: parts.wake,
            root: CancellationToken::new(),
        })
    }

    pub(crate) fn role_specs(&self) -> Vec<RoleSpec> {
        self.roles.iter().map(|role| role.spec.clone()).collect()
    }

    /// A snapshot of every subagent, in spawn order.
    pub(crate) fn snapshot(&self) -> Vec<NodeInfo> {
        self.lock()
            .entries
            .iter()
            .map(|entry| entry.info.clone())
            .collect()
    }

    /// How many times the registry has changed. A reader that only wants to
    /// know whether it is out of date reads this instead of the list, so a
    /// signal that carries no change costs nothing to check.
    pub(crate) fn revision(&self) -> u64 {
        self.lock().revision
    }

    pub(crate) fn active(&self) -> usize {
        self.lock()
            .entries
            .iter()
            .filter(|entry| entry.info.status.is_active())
            .count()
    }

    /// Cancels one subagent, reporting whether there was one to cancel so a
    /// command can say so instead of silently doing nothing.
    pub(crate) fn cancel(&self, id: AgentId) -> bool {
        let cancel = self
            .lock()
            .entries
            .iter()
            .find(|entry| entry.info.id == id)
            .map(|entry| entry.cancel.clone());
        match cancel {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }

    pub(crate) fn cancel_all(&self) {
        for token in self.cancel_tokens() {
            token.cancel();
        }
    }

    pub(crate) fn forget(&self, id: AgentId) -> bool {
        let removed = self.lock().remove(id);
        if removed {
            self.notify();
        }
        removed
    }

    /// Drops every subagent, stopping the running ones first. `/clear` starts
    /// a new session, and work delegated by the old one has no place in it.
    pub(crate) fn clear(&self) {
        {
            let mut registry = self.lock();
            for entry in &registry.entries {
                entry.cancel.cancel();
            }
            registry.entries.clear();
            registry.revision += 1;
        }
        self.notify();
    }

    /// The app is going away: nothing may outlive it.
    pub(crate) fn shutdown(&self) {
        self.root.cancel();
        self.cancel_all();
    }

    /// Resolves an address the user typed: a name (`explore#2`) or the
    /// 1-based position in the listing.
    pub(crate) fn resolve(&self, key: &str) -> Result<AgentId, String> {
        let registry = self.lock();
        let index = registry.resolve(key)?;
        Ok(registry.entries[index].info.id)
    }

    #[cfg(test)]
    /// A supervisor with no roles, for tests that need one but spawn nothing.
    pub(crate) fn bare(parent: AgentId, router: RouterHandle) -> Arc<Self> {
        let (wake, _) = mpsc::unbounded_channel();
        Self::new(SubagentParts {
            parent,
            router,
            roles: Vec::new(),
            max_concurrent: 1,
            max_iters: 1,
            events: EventBus::new(),
            wake,
        })
    }

    fn cancel_tokens(&self) -> Vec<CancellationToken> {
        self.lock()
            .entries
            .iter()
            .map(|entry| entry.cancel.clone())
            .collect()
    }

    fn update(&self, id: AgentId, f: impl FnOnce(&mut Entry)) {
        let mut registry = self.lock();
        if let Some(entry) = registry.entries.iter_mut().find(|e| e.info.id == id) {
            f(entry);
            registry.revision += 1;
            drop(registry);
            self.notify();
        }
    }

    /// Records the terminal state and prunes the oldest finished subagents.
    fn finish(&self, id: AgentId, status: NodeStatus, report: String, usage: Usage) {
        {
            let mut registry = self.lock();
            let Some(entry) = registry.entries.iter_mut().find(|e| e.info.id == id) else {
                return;
            };
            entry.info.status = status;
            entry.info.usage = usage;
            entry.info.finished_at = Some(now_ms());
            entry.report = Some(report);
            registry.revision += 1;
            registry.prune();
        }
        self.notify();
    }

    fn notify(&self) {
        let _ = self.wake.send(());
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A token for one subagent: cancelled when it is stopped by name, when
    /// the turn that spawned it is cancelled, or when the app shuts down.
    fn scoped_token(&self, parent: &CancellationToken) -> (CancellationToken, JoinHandle<()>) {
        let token = CancellationToken::new();
        let linked = token.clone();
        let root = self.root.clone();
        let parent = parent.clone();
        let link = tokio::spawn(async move {
            tokio::select! {
                biased;
                () = root.cancelled() => {}
                () = parent.cancelled() => {}
            }
            linked.cancel();
        });
        (token, link)
    }
}

#[async_trait]
impl SubagentSpawner for Subagents {
    async fn spawn(&self, request: SpawnRequest) -> Result<NodeHandle, AgentError> {
        let role = self
            .roles
            .iter()
            .find(|role| role.name() == request.role)
            .ok_or_else(|| AgentError::from(format!("unknown role '{}'", request.role)))?;
        // Nothing is registered or started until the task that will own the
        // subagent exists, so a spawn during shutdown leaves no orphan behind.
        let owner = self
            .me
            .upgrade()
            .ok_or_else(|| AgentError::from("subagents are shutting down"))?;
        let (cancel, link) = self.scoped_token(&request.cancellation);
        let id = AgentId::next();
        let turn_id = TurnId::next();
        let name = {
            let mut registry = self.lock();
            let name = registry.next_name(&request.role);
            registry.entries.push(Entry {
                info: NodeInfo {
                    id,
                    name: name.clone(),
                    role: request.role.clone(),
                    label: request.label.clone(),
                    background: request.background,
                    parent: self.parent,
                    status: NodeStatus::Pending,
                    usage: Usage::default(),
                    tool_calls: 0,
                    steps: 0,
                    started_at: now_ms(),
                    finished_at: None,
                },
                cancel: cancel.clone(),
                report: None,
            });
            name
        };
        self.notify();

        let mut agent = Agent::with_router(self.router.clone(), role.tools.clone())
            .with_id(id)
            .with_system(role.system.clone())
            .with_model(request.model.clone());
        agent.set_reasoning_effort(request.reasoning_effort);
        let ctx = TurnContext::new(turn_id, cancel, agent.selection())
            .with_policy(RunPolicy::default().with_max_iters(self.max_iters));

        let (done, outcome) = oneshot::channel();
        tokio::spawn(run_subagent(
            owner,
            Child {
                id,
                name: name.clone(),
                agent,
                ctx,
                prompt: request.prompt,
                link,
            },
            done,
        ));
        Ok(NodeHandle::new(id, name, outcome))
    }

    fn reports(&self, name: Option<&str>) -> Result<Vec<NodeReport>, AgentError> {
        let registry = self.lock();
        let report = |entry: &Entry| NodeReport {
            info: entry.info.clone(),
            report: entry.report.clone(),
        };
        match name {
            None => Ok(registry.entries.iter().map(report).collect()),
            Some(name) => {
                let index = registry.resolve(name).map_err(AgentError::from)?;
                Ok(vec![report(&registry.entries[index])])
            }
        }
    }
}

#[derive(Default)]
struct Registry {
    counters: BTreeMap<String, u32>,
    entries: Vec<Entry>,
    /// Bumped on every change, including a status or counter update.
    revision: u64,
}

impl Registry {
    fn next_name(&mut self, role: &str) -> String {
        let counter = self.counters.entry(role.to_string()).or_default();
        *counter += 1;
        self.revision += 1;
        format!("{role}#{counter}")
    }

    /// The position of the subagent addressed by `key`: its name, or the
    /// 1-based position in the listing.
    fn resolve(&self, key: &str) -> Result<usize, String> {
        if let Some(index) = self.entries.iter().position(|e| e.info.name == key) {
            return Ok(index);
        }
        match key.parse::<usize>().ok().and_then(|i| i.checked_sub(1)) {
            Some(index) if index < self.entries.len() => Ok(index),
            _ => Err(format!("unknown subagent '{key}'; {}", self.listing())),
        }
    }

    fn listing(&self) -> String {
        match self.entries.is_empty() {
            true => "no subagents have run in this session".to_string(),
            false => format!(
                "known: {}",
                self.entries
                    .iter()
                    .map(|entry| entry.info.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn remove(&mut self, id: AgentId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.info.id != id);
        let removed = self.entries.len() != before;
        self.revision += u64::from(removed);
        removed
    }

    /// Drops the oldest finished subagents, keeping recent ones for `/agents`.
    fn prune(&mut self) {
        let finished = self
            .entries
            .iter()
            .filter(|entry| !entry.info.status.is_active())
            .count();
        for _ in 0..finished.saturating_sub(MAX_FINISHED) {
            let Some(index) = self
                .entries
                .iter()
                .position(|entry| !entry.info.status.is_active())
            else {
                break;
            };
            self.entries.remove(index);
        }
    }
}

struct Entry {
    info: NodeInfo,
    cancel: CancellationToken,
    report: Option<String>,
}

struct Child {
    id: AgentId,
    name: String,
    agent: Agent,
    ctx: TurnContext,
    prompt: String,
    link: JoinHandle<()>,
}

async fn run_subagent(owner: Arc<Subagents>, mut child: Child, done: oneshot::Sender<NodeOutcome>) {
    let turn_id = child.ctx.turn_id;
    // Waiting for a slot is cancellable too: a subagent stopped while it is
    // still queued must stop rather than hold its place in line forever.
    let permit = tokio::select! {
        biased;
        () = child.ctx.cancellation.cancelled() => None,
        permit = owner.slots.acquire() => permit.ok(),
    };
    let (result, duration_ms) = match permit {
        None => (Err(AgentError::cancelled()), 0),
        Some(_permit) => {
            owner.update(child.id, |entry| {
                entry.info.status = NodeStatus::Running { turn_id };
            });
            let mut sink = CountingSink::new(
                BusSink::new(owner.events.clone(), child.id, turn_id),
                Arc::clone(&owner),
                child.id,
            );
            let started = Instant::now();
            let result = child.agent.run(child.prompt, &child.ctx, &mut sink).await;
            (result, as_ms(started.elapsed()))
        }
    };
    let usage = child.agent.last_turn_usage();
    let (status, report) = match result {
        Ok(output) => (NodeStatus::Completed, output.text()),
        Err(error) if error.is_cancelled() => (NodeStatus::Cancelled, String::new()),
        Err(error) => (
            NodeStatus::Failed {
                error: error.message.clone(),
            },
            String::new(),
        ),
    };
    let (tool_calls, steps) = owner
        .lock()
        .entries
        .iter()
        .find(|entry| entry.info.id == child.id)
        .map_or((0, 0), |entry| (entry.info.tool_calls, entry.info.steps));
    tracing::info!(
        subagent = %child.name,
        status = status.label(),
        duration_ms,
        tool_calls,
        steps,
        "subagent finished"
    );
    owner.finish(child.id, status.clone(), report.clone(), usage);
    child.link.abort();
    let _ = done.send(NodeOutcome {
        status,
        report,
        tool_calls,
        steps,
        usage,
        duration_ms,
    });
}

/// Counts what a subagent is doing as its events stream past, so the registry
/// stays the one place that knows.
struct CountingSink {
    inner: BusSink,
    owner: Arc<Subagents>,
    id: AgentId,
}

impl CountingSink {
    fn new(inner: BusSink, owner: Arc<Subagents>, id: AgentId) -> Self {
        Self { inner, owner, id }
    }
}

impl EventSink for CountingSink {
    fn emit(&mut self, event: AgentEvent) {
        match &event {
            // Started is also emitted for a refusal, which never ran. The
            // count is invocations: a success or a failure from the tool.
            AgentEvent::Tool(ToolEvent::Finished { result, .. }) if result.executed() => {
                self.owner.update(self.id, |entry| {
                    entry.info.tool_calls += 1;
                });
            }
            AgentEvent::Turn(TurnEvent::StepStarted { index }) => {
                let index = *index as u32;
                self.owner.update(self.id, |entry| entry.info.steps = index);
            }
            _ => {}
        }
        self.inner.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_agent::{Agent, ToolCallId, ToolResult, ToolView};
    use oven_llm::Router;

    fn bare() -> Arc<Subagents> {
        let agent = Agent::new(Router::new(), Vec::new());
        Subagents::bare(agent.id(), agent.router_handle())
    }

    #[test]
    fn the_revision_counts_only_what_it_mirrors() {
        let subagents = bare();
        assert_eq!(subagents.revision(), 0);

        assert!(!subagents.forget(AgentId::next()), "nothing to drop");
        assert!(!subagents.cancel(AgentId::next()), "nothing to stop");
        assert_eq!(
            subagents.revision(),
            0,
            "a lookup that found nothing changed nothing a reader would mirror"
        );

        subagents.clear();
        assert_eq!(
            subagents.revision(),
            1,
            "dropping every subagent is a change an empty registry still has"
        );
    }

    #[test]
    fn tool_calls_count_invocations_not_refusals() {
        let subagents = bare();
        let id = AgentId::next();
        subagents.lock().entries.push(Entry {
            info: NodeInfo {
                id,
                name: "explore#1".into(),
                role: "explore".into(),
                label: String::new(),
                background: false,
                parent: AgentId::next(),
                status: NodeStatus::Pending,
                usage: Usage::default(),
                tool_calls: 0,
                steps: 0,
                started_at: 0,
                finished_at: None,
            },
            cancel: CancellationToken::new(),
            report: None,
        });
        let mut sink = CountingSink::new(
            BusSink::new(EventBus::new(), id, TurnId::next()),
            Arc::clone(&subagents),
            id,
        );
        let started = |call_id| {
            AgentEvent::Tool(ToolEvent::Started {
                call_id,
                name: "write".into(),
                view: ToolView::named("write"),
            })
        };
        let finished = |call_id, result| {
            AgentEvent::Tool(ToolEvent::Finished {
                call_id,
                result,
                detail: None,
            })
        };

        let refused = ToolCallId::next();
        sink.emit(started(refused));
        sink.emit(finished(
            refused,
            ToolResult::Rejected {
                reason: "unknown tool: write".into(),
            },
        ));
        sink.emit(finished(
            ToolCallId::next(),
            ToolResult::Rejected {
                reason: "tool 'file_write' is unavailable in Ask mode".into(),
            },
        ));
        sink.emit(finished(ToolCallId::next(), ToolResult::Cancelled));
        assert_eq!(subagents.snapshot()[0].tool_calls, 0);

        sink.emit(finished(
            ToolCallId::next(),
            ToolResult::Success {
                output: "ok".into(),
            },
        ));
        sink.emit(finished(
            ToolCallId::next(),
            ToolResult::Failed {
                error: "boom".into(),
                output: Some("boom".into()),
            },
        ));
        assert_eq!(subagents.snapshot()[0].tool_calls, 2);
    }
}
