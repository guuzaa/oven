use std::fmt;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_yaml::Mapping;

use crate::error::MemoryError;

pub const MAX_ID: usize = 64;
pub const MAX_DESCRIPTION: usize = 160;
pub const MAX_WHY: usize = 200;
pub const MAX_BODY: usize = 800;
pub const MAX_ANCHORS: usize = 8;
pub const MAX_ANCHOR: usize = 200;
pub const MAX_NOTE: usize = 1000;
pub const GLOBAL_BUDGET: usize = 2000;
pub const PROJECT_BUDGET: usize = 5000;
pub const SHARED_BUDGET: usize = 3000;

/// OKF reserves these names at every level of a bundle.
pub const RESERVED_IDS: &[&str] = &["index", "log"];

const ID_DASH: char = '-';
const MODEL_ACTOR_PREFIX: &str = "oven/";
const HUMAN_ACTOR_PREFIX: &str = "human:";
const PROJECT_RESOURCE_PREFIX: &str = "project:";
const SESSION_RESOURCE_PREFIX: &str = "session:";
const PROJECT_SOURCE_ID: &str = "project";
const SESSION_SOURCE_ID: &str = "session";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryId(String);

impl MemoryId {
    pub fn new(raw: &str) -> Result<Self, MemoryError> {
        if raw.is_empty() {
            return Err(MemoryError::EmptyId);
        }
        if !raw.chars().all(is_id_char) {
            return Err(MemoryError::InvalidId);
        }
        if raw.starts_with(ID_DASH) {
            return Err(MemoryError::LeadingDash);
        }
        if raw.chars().count() > MAX_ID {
            return Err(MemoryError::IdTooLong);
        }
        if RESERVED_IDS.contains(&raw) {
            return Err(MemoryError::ReservedId { id: raw.to_owned() });
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == ID_DASH
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    Fact,
    Preference,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Preference => "preference",
        }
    }

    /// The OKF `type` value.
    pub fn okf_type(self) -> &'static str {
        match self {
            Self::Fact => "Fact",
            Self::Preference => "Preference",
        }
    }

    /// Any spelling of either kind; `None` for a type oven does not know.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        [Self::Fact, Self::Preference]
            .into_iter()
            .find(|kind| raw.eq_ignore_ascii_case(kind.as_str()))
    }
}

/// Where a memory applies and who sees it. The order is the listing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    Project,
    Shared,
    Global,
}

impl MemoryScope {
    pub const ALL: [Self; 3] = [Self::Project, Self::Shared, Self::Global];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Shared => "shared",
            Self::Global => "global",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.as_str() == raw)
    }

    /// The catalog characters this layer may spend.
    pub fn budget(self) -> usize {
        match self {
            Self::Project => PROJECT_BUDGET,
            Self::Shared => SHARED_BUDGET,
            Self::Global => GLOBAL_BUDGET,
        }
    }

    /// Whether anchors resolve against the workspace for this layer.
    pub fn is_local(self) -> bool {
        !matches!(self, Self::Global)
    }
}

impl fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryStatus {
    Draft,
    #[default]
    Stable,
    Deprecated,
}

impl MemoryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Stable => "stable",
            Self::Deprecated => "deprecated",
        }
    }

    /// An unknown status reads as the OKF default.
    pub fn parse(raw: &str) -> Self {
        [Self::Draft, Self::Deprecated]
            .into_iter()
            .find(|status| raw.trim() == status.as_str())
            .unwrap_or_default()
    }
}

/// An OKF actor and when it acted: `generated` and each `verified` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

impl Stamp {
    pub fn model(model: &str) -> Self {
        Self {
            by: format!("{MODEL_ACTOR_PREFIX}{model}"),
            at: Some(now_rfc3339()),
        }
    }

    pub fn human(name: &str) -> Self {
        Self {
            by: format!("{HUMAN_ACTOR_PREFIX}{name}"),
            at: Some(now_rfc3339()),
        }
    }

    pub fn is_human(&self) -> bool {
        self.by.starts_with(HUMAN_ACTOR_PREFIX)
    }
}

/// An OKF `sources` entry. Keys oven does not use are kept for the rewrite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub extra: Mapping,
}

impl Source {
    pub fn project(root: &str) -> Self {
        Self::keyed(
            PROJECT_SOURCE_ID,
            format!("{PROJECT_RESOURCE_PREFIX}{root}"),
        )
    }

    pub fn session(id: &str) -> Self {
        Self::keyed(SESSION_SOURCE_ID, format!("{SESSION_RESOURCE_PREFIX}{id}"))
    }

    fn keyed(id: &str, resource: String) -> Self {
        Self {
            id: Some(id.to_owned()),
            resource,
            title: None,
            extra: Mapping::new(),
        }
    }

    /// The workspace root a `project:` source names.
    pub fn project_root(&self) -> Option<&str> {
        self.resource.strip_prefix(PROJECT_RESOURCE_PREFIX)
    }

    /// The session id a `session:` source names.
    pub fn session_id(&self) -> Option<&str> {
        self.resource.strip_prefix(SESSION_RESOURCE_PREFIX)
    }
}

/// One curated memory: an OKF concept file.
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    pub id: MemoryId,
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub description: String,
    pub why: Option<String>,
    pub body: String,
    pub anchors: Vec<String>,
    pub status: MemoryStatus,
    pub stale_after: Option<String>,
    pub generated: Option<Stamp>,
    pub verified: Vec<Stamp>,
    pub sources: Vec<Source>,
    /// Frontmatter keys oven does not read, kept so a rewrite loses nothing.
    pub extra: Mapping,
}

impl Memory {
    pub fn new(id: MemoryId, scope: MemoryScope, kind: MemoryKind, description: &str) -> Self {
        Self {
            id,
            scope,
            kind,
            description: description.to_owned(),
            why: None,
            body: String::new(),
            anchors: Vec::new(),
            status: MemoryStatus::Stable,
            stale_after: None,
            generated: None,
            verified: Vec::new(),
            sources: Vec::new(),
            extra: Mapping::new(),
        }
    }

    /// OKF's human-reviewed trust tier.
    pub fn is_reviewed(&self) -> bool {
        self.verified.iter().any(Stamp::is_human)
    }

    /// The `scope/id` the catalog and every command name it by.
    pub fn reference(&self) -> String {
        format!("{}/{}", self.scope, self.id)
    }

    /// Adds a source unless an equal resource is already listed.
    pub fn add_source(&mut self, source: Source) {
        if !self.sources.iter().any(|s| s.resource == source.resource) {
            self.sources.push(source);
        }
    }
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::{MAX_ID, MemoryId, MemoryKind, MemoryScope, MemoryStatus, Source, Stamp};
    use crate::error::{EMPTY_ID, ID_TOO_LONG, INVALID_ID, LEADING_DASH, MemoryError, RESERVED_ID};

    #[test]
    fn valid_slug_ok() {
        let id = MemoryId::new("proxy-requires-http2").unwrap();
        assert_eq!(id.as_str(), "proxy-requires-http2");
        assert!(MemoryId::new(&"a".repeat(MAX_ID)).is_ok());
    }

    #[test]
    fn rejects_invalid_ids() {
        assert_eq!(MemoryId::new("../x"), Err(MemoryError::InvalidId));
        assert_eq!(MemoryId::new("../x").unwrap_err().to_string(), INVALID_ID);
        assert_eq!(MemoryId::new("a/b"), Err(MemoryError::InvalidId));
        assert_eq!(MemoryId::new("A"), Err(MemoryError::InvalidId));
        assert_eq!(MemoryId::new(""), Err(MemoryError::EmptyId));
        assert_eq!(MemoryId::new("").unwrap_err().to_string(), EMPTY_ID);
        assert_eq!(MemoryId::new("-leading"), Err(MemoryError::LeadingDash));
        assert_eq!(
            MemoryId::new("-leading").unwrap_err().to_string(),
            LEADING_DASH
        );
        let too_long = "a".repeat(MAX_ID + 1);
        assert_eq!(MemoryId::new(&too_long), Err(MemoryError::IdTooLong));
        assert_eq!(
            MemoryId::new(&too_long).unwrap_err().to_string(),
            format!("{ID_TOO_LONG} ({MAX_ID})")
        );
    }

    #[test]
    fn okf_reserved_names_are_not_ids() {
        for reserved in ["index", "log"] {
            let err = MemoryId::new(reserved).unwrap_err();
            assert_eq!(err.to_string(), format!("{RESERVED_ID}: {reserved}"));
        }
        assert!(MemoryId::new("logging").is_ok());
    }

    #[test]
    fn kinds_parse_in_either_spelling() {
        assert_eq!(MemoryKind::parse("Fact"), Some(MemoryKind::Fact));
        assert_eq!(
            MemoryKind::parse("preference"),
            Some(MemoryKind::Preference)
        );
        assert_eq!(MemoryKind::parse("Playbook"), None);
    }

    #[test]
    fn scopes_round_trip_through_their_names() {
        for scope in MemoryScope::ALL {
            assert_eq!(MemoryScope::parse(scope.as_str()), Some(scope));
        }
        assert_eq!(MemoryScope::parse("user"), None);
    }

    #[test]
    fn an_unknown_status_is_stable() {
        assert_eq!(MemoryStatus::parse("draft"), MemoryStatus::Draft);
        assert_eq!(MemoryStatus::parse("archived"), MemoryStatus::Stable);
    }

    #[test]
    fn actors_follow_the_okf_convention() {
        assert_eq!(Stamp::model("m1").by, "oven/m1");
        assert!(Stamp::human("me").is_human());
        assert!(!Stamp::model("m1").is_human());
    }

    #[test]
    fn sources_name_projects_and_sessions() {
        assert_eq!(Source::project("/ws").project_root(), Some("/ws"));
        assert_eq!(Source::session("01J").session_id(), Some("01J"));
        assert_eq!(Source::session("01J").project_root(), None);
    }
}
