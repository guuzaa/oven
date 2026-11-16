use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::PathBuf;

use tracing::warn;

use super::log::append_log;
use super::plan::{PutOp, TidyOp, TidyPlan};
use crate::error::{MemoryError, SHARED_READ_ONLY};
use crate::model::{Memory, MemoryId, MemoryKind, MemoryScope, MemoryStatus, Source, Stamp};
use crate::store::MemoryStore;

const REVIEWED_LOCKED: &str = "a person reviewed it, so tidy leaves it alone";
const NOT_GLOBAL: &str = "only a global memory can be demoted";
const ORIGIN_UNKNOWN: &str = "the project it came from is unknown";
const UNKNOWN_SCOPE: &str = "unknown scope";
const UNKNOWN_TYPE: &str = "unknown type";
const MISSING_SLASH: &str = "a ref is <scope>/<id>";
const UNPARSABLE_OP: &str = "an op";
const NOTHING_TO_CHANGE: &str = "tidy: nothing to change";

/// What tidy needs to stamp and route what it writes.
pub struct TidyContext<'a> {
    pub model: &'a str,
    /// The canonical root of the open workspace.
    pub root: &'a str,
    /// For each global memory with a known origin, that project's layer.
    pub origins: &'a BTreeMap<MemoryId, PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Created {
        reference: String,
        description: String,
    },
    Updated {
        reference: String,
    },
    Merged {
        reference: String,
        absorbed: Vec<String>,
    },
    Drafted {
        reference: String,
        kept: Vec<String>,
    },
    Deleted {
        reference: String,
        reason: String,
    },
    Demoted {
        reference: String,
        to: String,
    },
    Skipped {
        op: String,
        reason: String,
    },
}

impl Change {
    fn label(&self) -> &'static str {
        match self {
            Self::Created { .. } => "created",
            Self::Updated { .. } => "updated",
            Self::Merged { .. } => "merged",
            Self::Drafted { .. } => "drafted",
            Self::Deleted { .. } => "deleted",
            Self::Demoted { .. } => "demoted",
            Self::Skipped { .. } => "skipped",
        }
    }

    fn line(&self) -> String {
        match self {
            Self::Created {
                reference,
                description,
            } => format!("created {reference}: {description}"),
            Self::Updated { reference } => format!("updated {reference}"),
            Self::Merged {
                reference,
                absorbed,
            } => format!("merged {} into {reference}", absorbed.join(", ")),
            Self::Drafted { reference, kept } if kept.is_empty() => {
                format!("drafted {reference} for /memory review")
            }
            Self::Drafted { reference, kept } => format!(
                "drafted {reference} for /memory review; kept {} until it is approved",
                kept.join(", ")
            ),
            Self::Deleted { reference, reason } => format!("deleted {reference}: {reason}"),
            Self::Demoted { reference, to } => format!("demoted {reference} to {to}"),
            Self::Skipped { op, reason } => format!("skipped {op}: {reason}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TidyReport {
    pub changes: Vec<Change>,
}

impl TidyReport {
    /// A count per kind of change, then one line per change.
    pub fn render(&self) -> String {
        if self.changes.is_empty() {
            return NOTHING_TO_CHANGE.to_owned();
        }
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for change in &self.changes {
            match counts
                .iter_mut()
                .find(|(label, _)| *label == change.label())
            {
                Some((_, count)) => *count += 1,
                None => counts.push((change.label(), 1)),
            }
        }
        let summary: Vec<String> = counts
            .iter()
            .map(|(label, count)| format!("{count} {label}"))
            .collect();
        let mut out = format!("tidy: {}", summary.join(", "));
        for change in &self.changes {
            let _ = write!(out, "\n- {}", change.line());
        }
        out
    }
}

/// The `log.md` lines each touched layer gets.
type Logs = BTreeMap<PathBuf, Vec<String>>;

/// Applies every op the rules allow, logs what changed in each layer's
/// `log.md`, and reports both what changed and what was refused.
pub async fn apply_plan(store: &MemoryStore, plan: TidyPlan, cx: &TidyContext<'_>) -> TidyReport {
    let mut report = TidyReport::default();
    let mut logs = Logs::new();
    for op in plan.ops {
        let change = match op {
            Err(reason) => Change::Skipped {
                op: UNPARSABLE_OP.to_owned(),
                reason,
            },
            Ok(TidyOp::Put(put)) => {
                let name = format!("put {}/{}", put.scope, put.id);
                apply_put(store, put, cx, &mut logs)
                    .await
                    .unwrap_or_else(|reason| Change::Skipped { op: name, reason })
            }
            Ok(TidyOp::Delete { reference, reason }) => {
                apply_delete(store, &reference, reason, &mut logs)
                    .await
                    .unwrap_or_else(|why| Change::Skipped {
                        op: format!("delete {reference}"),
                        reason: why,
                    })
            }
            Ok(TidyOp::Demote { reference, .. }) => apply_demote(store, &reference, cx, &mut logs)
                .await
                .unwrap_or_else(|why| Change::Skipped {
                    op: format!("demote {reference}"),
                    reason: why,
                }),
        };
        report.changes.push(change);
    }
    for (dir, lines) in logs {
        if let Err(err) = append_log(&dir, &lines).await {
            warn!(path = %dir.display(), error = %err, "memory log write failed");
        }
    }
    report
}

async fn apply_put(
    store: &MemoryStore,
    put: PutOp,
    cx: &TidyContext<'_>,
    logs: &mut Logs,
) -> Result<Change, String> {
    let scope = writable_scope(&put.scope)?;
    let id = MemoryId::new(&put.id).map_err(|err| err.to_string())?;
    let kind = MemoryKind::parse(&put.kind).ok_or(format!("{UNKNOWN_TYPE} '{}'", put.kind))?;
    let existing = match store.read(scope, &id).await {
        Ok(memory) if memory.is_reviewed() => return Err(REVIEWED_LOCKED.to_owned()),
        Ok(memory) => Some(memory),
        Err(MemoryError::NotFound { .. }) => None,
        Err(err) => return Err(err.to_string()),
    };
    let mut absorbed = Vec::new();
    for raw in &put.replaces {
        let target = unreviewed(store, raw).await?;
        if (target.scope, &target.id) != (scope, &id) {
            absorbed.push(target);
        }
    }
    let status = match (scope, &existing) {
        (MemoryScope::Global, Some(old)) => old.status,
        (MemoryScope::Global, None) => MemoryStatus::Draft,
        _ => MemoryStatus::Stable,
    };
    let mut memory = Memory::new(id, scope, kind, &put.description);
    if let Some(old) = &existing {
        memory.sources = old.sources.clone();
        memory.stale_after = old.stale_after.clone();
        memory.extra = old.extra.clone();
    }
    for source in absorbed.iter().flat_map(|old| old.sources.iter()) {
        memory.add_source(source.clone());
    }
    memory.add_source(Source::project(cx.root));
    memory.why = put.why;
    memory.body = put.body;
    memory.anchors = put.anchors;
    memory.status = status;
    memory.generated = Some(Stamp::model(cx.model));
    let replaces: Vec<(MemoryScope, MemoryId)> = match status {
        MemoryStatus::Draft => Vec::new(),
        _ => absorbed
            .iter()
            .map(|old| (old.scope, old.id.clone()))
            .collect(),
    };
    store
        .put_replacing(memory.clone(), &replaces)
        .await
        .map_err(|err| err.to_string())?;

    let reference = memory.reference();
    let absorbed_refs: Vec<String> = absorbed.iter().map(Memory::reference).collect();
    if !replaces.is_empty() {
        for old in &absorbed {
            let reason = format!("merged into {reference}");
            log(store, logs, old.scope, deletion(&old.id, &reason));
        }
    }
    let change = match (status, &existing) {
        (MemoryStatus::Draft, None) => Change::Drafted {
            reference,
            kept: absorbed_refs,
        },
        _ if !replaces.is_empty() => Change::Merged {
            reference,
            absorbed: absorbed_refs,
        },
        (_, Some(_)) => Change::Updated { reference },
        (_, None) => Change::Created {
            reference,
            description: memory.description.clone(),
        },
    };
    let word = match &change {
        Change::Created { .. } => "Creation",
        Change::Drafted { .. } => "Draft",
        _ => "Update",
    };
    log(
        store,
        logs,
        scope,
        entry(word, &memory.id, &memory.description),
    );
    Ok(change)
}

async fn apply_delete(
    store: &MemoryStore,
    raw: &str,
    reason: String,
    logs: &mut Logs,
) -> Result<Change, String> {
    let target = unreviewed(store, raw).await?;
    store
        .forget(target.scope, &target.id)
        .await
        .map_err(|err| err.to_string())?;
    log(store, logs, target.scope, deletion(&target.id, &reason));
    Ok(Change::Deleted {
        reference: target.reference(),
        reason,
    })
}

async fn apply_demote(
    store: &MemoryStore,
    raw: &str,
    cx: &TidyContext<'_>,
    logs: &mut Logs,
) -> Result<Change, String> {
    let target = unreviewed(store, raw).await?;
    if target.scope != MemoryScope::Global {
        return Err(NOT_GLOBAL.to_owned());
    }
    let dir = cx
        .origins
        .get(&target.id)
        .ok_or_else(|| ORIGIN_UNKNOWN.to_owned())?;
    store
        .relocate(target.scope, &target.id, dir)
        .await
        .map_err(|err| err.to_string())?;
    let to = dir.display().to_string();
    log(
        store,
        logs,
        MemoryScope::Global,
        format!("* **Demotion**: {} moved to {to}", target.id),
    );
    logs.entry(dir.clone())
        .or_default()
        .push(entry("Creation", &target.id, "demoted from global"));
    Ok(Change::Demoted {
        reference: target.reference(),
        to,
    })
}

/// The memory a ref names, if tidy may change it.
async fn unreviewed(store: &MemoryStore, raw: &str) -> Result<Memory, String> {
    let (scope, id) = raw
        .split_once('/')
        .ok_or(format!("{MISSING_SLASH}: '{raw}'"))?;
    let scope = writable_scope(scope)?;
    let id = MemoryId::new(id).map_err(|err| err.to_string())?;
    let memory = store
        .read(scope, &id)
        .await
        .map_err(|err| err.to_string())?;
    if memory.is_reviewed() {
        return Err(REVIEWED_LOCKED.to_owned());
    }
    Ok(memory)
}

fn writable_scope(raw: &str) -> Result<MemoryScope, String> {
    match MemoryScope::parse(raw) {
        Some(MemoryScope::Shared) => Err(SHARED_READ_ONLY.to_owned()),
        Some(scope) => Ok(scope),
        None => Err(format!("{UNKNOWN_SCOPE} '{raw}'")),
    }
}

fn log(store: &MemoryStore, logs: &mut Logs, scope: MemoryScope, line: String) {
    if let Ok(dir) = store.roots().dir(scope) {
        logs.entry(dir.to_path_buf()).or_default().push(line);
    }
}

fn entry(word: &str, id: &MemoryId, text: &str) -> String {
    format!("* **{word}**: [{id}](/{id}.md) {text}")
}

fn deletion(id: &MemoryId, reason: &str) -> String {
    format!("* **Deletion**: {id}: {reason}")
}

#[cfg(test)]
#[path = "apply_test.rs"]
mod apply_test;
