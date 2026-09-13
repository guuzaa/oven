use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{mpsc, oneshot};

use crate::identity::ToolCallId;
use crate::tools::ToolView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApprovalRequestId(pub u64);

impl ApprovalRequestId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

pub struct ToolApproval {
    pub request_id: ApprovalRequestId,
    pub call_id: ToolCallId,
    pub name: String,
    pub view: ToolView,
    pub responder: oneshot::Sender<ApprovalDecision>,
}

pub type ApprovalSender = mpsc::UnboundedSender<ToolApproval>;
